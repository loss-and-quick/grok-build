//! `message_subagent` tool — steer a subagent that is still running.
//!
//! The model-facing end of the same steering chain the `agent_message` plugin
//! RPC drives: [`SubagentBackend::message`] → `SubagentEvent::Message` →
//! coordinator → the child's `pending_steering`, injected as a system reminder
//! at the child's next injection point.
//!
//! Only the *surface* differs from the plugin RPC. A plugin branches on a
//! machine-readable outcome enum and can retry on a timer; the model gets one
//! sentence per outcome that names the call to make next, because that is the
//! only form it can act on.
//!
//! Not to be confused with resuming: `task`'s `resume_from` continues a subagent
//! that has already *finished* and mints a **new** id. This tool reaches a child
//! that is running right now and keeps its id.

use crate::implementations::grok_build::task::backend::SubagentBackendResource;
use crate::implementations::grok_build::task::types::SubagentMessageOutcome;
use crate::types::output::ToolOutput;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::template_renderer::TemplateRenderer;
use crate::types::tool::{ToolKind, ToolNamespace};
use serde::{Deserialize, Serialize};

/// Registered name of the `message_subagent` tool.
pub const MESSAGE_SUBAGENT_TOOL_NAME: &str = "message_subagent";

/// Input for the `message_subagent` tool.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct MessageSubagentInput {
    /// The id of the running subagent to message, as reported on the
    /// `subagent_id:` line when it was backgrounded, or in its completion
    /// notification. Not a bash task id — this tool only reaches subagents.
    pub subagent_id: String,
    /// What to tell the subagent. Plain text, delivered as-is; write it as an
    /// instruction to the child, not as a note about the child.
    pub message: String,
}

/// Output schema for `message_subagent` (JSON Schema generation only).
#[derive(Debug, schemars::JsonSchema)]
pub struct MessageSubagentOutput {
    /// Whether the running subagent took the message, and what to do if not.
    pub result: String,
}

#[derive(Debug, Default)]
pub struct MessageSubagentTool;

impl crate::types::tool_metadata::ToolMetadata for MessageSubagentTool {
    fn kind(&self) -> ToolKind {
        ToolKind::MessageSubagentAction
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        // Template markers are rendered by the default `versioned_definition`,
        // so the sibling tools are named as the finalized toolset names them.
        "Send a message to a subagent that is still running. The text lands in the child's \
         conversation before its next step, so it changes course mid-task instead of \
         finishing the wrong work.\n\n\
         Use this when a background subagent needs a correction, a constraint you forgot to \
         give it, or an answer to something it is about to guess at. Killing and respawning \
         throws away everything it has done; this keeps it.\n\n\
         `subagent_id` is the id ${{ tools.by_kind.task }} reported for the child — the \
         `subagent_id:` line when it went to the background, or the id in its completion \
         notification. Bash task ids do not work here.\n\n\
         This reaches only a child that is running right now:\n\
         - A subagent that has already finished cannot be messaged. To carry its work \
         forward, call ${{ tools.by_kind.task }} with ${{ params.task.resume_from }} set to \
         its id — that starts a NEW subagent with a NEW id that inherits the old one's \
         transcript, which is a different thing from steering a live one.\n\
         - A foreground spawn blocks this turn until the child is done, so there is never a \
         moment in which to message it. Background the child if you may want to steer it.\n\n\
         One message per call, text only — no attachments and no slash commands. The result \
         tells you whether the child took it; a message that arrives after the child's turn \
         has ended is dropped rather than queued, so re-send it if it still matters."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        // Strictly narrower than the other task-lifecycle tools: those also act
        // on ids minted by a background-capable bash, this one only ever on
        // subagent ids. An agent that cannot spawn cannot steer.
        Expr::Value(ToolRequirement::tool_kind(ToolKind::Task))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

impl xai_tool_runtime::Tool for MessageSubagentTool {
    type Args = MessageSubagentInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(MESSAGE_SUBAGENT_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            MESSAGE_SUBAGENT_TOOL_NAME,
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    /// Mutating: it changes what another agent does. `ToolScope::Write` keeps
    /// the computer hub routing it to the leader, which is also the only place
    /// the subagent coordinator lives.
    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(xai_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(
        name = "tool.message_subagent",
        skip_all,
        fields(subagent_id = %input.subagent_id)
    )]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: MessageSubagentInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let backend = {
            let res = resources.lock().await;
            res.get::<SubagentBackendResource>().cloned()
        };
        let Some(backend) = backend else {
            return Ok(ToolOutput::Text(
                "Subagents are not available in this session, so there is nothing to message."
                    .into(),
            ));
        };

        if input.message.trim().is_empty() {
            return Ok(ToolOutput::Text(
                "Nothing sent: `message` was empty. Say what the subagent should do differently."
                    .into(),
            ));
        }

        let outcome = backend
            .backend()
            .message(&input.subagent_id, &input.message)
            .await;

        Ok(ToolOutput::Text(
            render_outcome(&resources, &input.subagent_id, outcome)
                .await
                .into(),
        ))
    }
}

/// Turn one [`SubagentMessageOutcome`] into the sentence the model acts on.
///
/// The six outcomes stay six answers. "The child has it", "its turn outran the
/// text" and "it finished first" call for opposite next moves, and a model that
/// is handed a bare success flag will pick one at random — so each variant names
/// its own follow-up call instead.
async fn render_outcome(
    resources: &crate::types::resources::SharedResources,
    id: &str,
    outcome: SubagentMessageOutcome,
) -> String {
    // `resolve_tool_name`, not a template render: a missing kind yields `None`
    // here rather than an empty marker in the middle of a sentence.
    let task_tool = TemplateRenderer::resolve_tool_name(resources, ToolKind::Task)
        .await
        .unwrap_or_else(|| "task".to_string());
    let output_tool =
        TemplateRenderer::resolve_tool_name(resources, ToolKind::BackgroundTaskAction)
            .await
            .unwrap_or_else(|| "get_task_output".to_string());

    match outcome {
        SubagentMessageOutcome::Delivered => format!(
            "Delivered to {id}. It is in the subagent's conversation and the subagent will \
             read it before its next step."
        ),
        SubagentMessageOutcome::NotDelivered => format!(
            "Not delivered: {id}'s turn ended before the message could reach it. The message \
             was dropped, not queued — it will never be replayed. Check what the subagent did \
             with {output_tool}, and send it again if it is still running and the correction \
             still matters."
        ),
        SubagentMessageOutcome::NotStarted => format!(
            "Not delivered: {id} has not started its session yet, so there is no turn to \
             steer. Nothing was queued. Try again once it is running."
        ),
        SubagentMessageOutcome::AlreadyFinished { status } => format!(
            "Not delivered: {id} already {status}. Read its result with {output_tool}. To \
             carry its work forward, call {task_tool} with resume_from=\"{id}\" — that starts \
             a new subagent with a new id that inherits this one's transcript."
        ),
        SubagentMessageOutcome::Unreachable => format!(
            "Not delivered: {id} is running but its session is unreachable (crashed, or being \
             torn down). Check it with {output_tool}; if it is gone, spawn a replacement with \
             {task_tool}."
        ),
        SubagentMessageOutcome::NotFound => format!(
            "No subagent {id} in this session. Only subagents this session spawned can be \
             messaged, by the id {task_tool} reported for them — bash task ids do not work here."
        ),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementations::grok_build::task::backend::ChannelBackend;
    use crate::implementations::grok_build::task::types::{SubagentEvent, SubagentMessageRequest};
    use crate::types::resources::{Resources, SharedResources};
    use crate::types::tool_metadata::{ToolMetadata, test_ctx_with_call_id};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn renderer_resources() -> SharedResources {
        let tools: HashMap<ToolKind, String> = [
            (ToolKind::Task, "spawn_subagent".to_string()),
            (
                ToolKind::BackgroundTaskAction,
                "get_command_or_subagent_output".to_string(),
            ),
        ]
        .into_iter()
        .collect();
        let mut resources = Resources::new();
        resources.insert(TemplateRenderer::new(tools, HashMap::new()));
        resources.into_shared()
    }

    fn text_of(output: ToolOutput) -> String {
        match output {
            ToolOutput::Text(t) => t.text.to_string(),
            other => panic!("expected Text output, got {other:?}"),
        }
    }

    #[test]
    fn tool_identity() {
        let tool = MessageSubagentTool;
        assert_eq!(
            xai_tool_runtime::Tool::id(&tool).as_str(),
            MESSAGE_SUBAGENT_TOOL_NAME
        );
        assert_eq!(
            ToolMetadata::kind(&tool),
            ToolKind::MessageSubagentAction,
            "the toolset plumbing keys off the kind, not the name"
        );
        assert!(!ToolMetadata::is_read_only(&tool));
    }

    /// The description must render clean against a toolset that renamed its
    /// siblings — a leaked `${` marker would tell the model to call a tool that
    /// does not exist under that name.
    #[test]
    fn description_follows_the_toolset_names() {
        let tools: HashMap<ToolKind, String> = [(ToolKind::Task, "spawn_subagent".to_string())]
            .into_iter()
            .collect();
        let params: HashMap<ToolKind, HashMap<String, String>> = [(
            ToolKind::Task,
            [("resume_from".to_string(), "resume_from".to_string())]
                .into_iter()
                .collect(),
        )]
        .into_iter()
        .collect();
        let renderer = TemplateRenderer::new(tools, params);
        let rendered = renderer
            .render(ToolMetadata::description_template(&MessageSubagentTool))
            .expect("description renders");

        assert!(!rendered.contains("${"), "leaked marker: {rendered}");
        assert!(rendered.contains("spawn_subagent"), "{rendered}");
        // The resume confusion is the whole reason this clause exists.
        assert!(rendered.contains("resume_from"), "{rendered}");
        assert!(rendered.contains("NEW id"), "{rendered}");
        // Foreground spawns leave no window in which to steer; say so.
        assert!(rendered.contains("foreground"), "{rendered}");
    }

    /// Six outcomes, six answers. Collapsing any pair would leave the model
    /// guessing between "send it again" and "resume it", which are opposite
    /// moves against a child that may or may not still exist.
    #[tokio::test]
    async fn each_outcome_names_its_own_follow_up() {
        let resources = renderer_resources();
        let rendered: Vec<String> = futures::future::join_all(
            [
                SubagentMessageOutcome::Delivered,
                SubagentMessageOutcome::NotDelivered,
                SubagentMessageOutcome::NotStarted,
                SubagentMessageOutcome::AlreadyFinished {
                    status: "completed".into(),
                },
                SubagentMessageOutcome::Unreachable,
                SubagentMessageOutcome::NotFound,
            ]
            .into_iter()
            .map(|o| render_outcome(&resources, "sub-1", o)),
        )
        .await;

        for (i, a) in rendered.iter().enumerate() {
            for b in rendered.iter().skip(i + 1) {
                assert_ne!(a, b, "two outcomes read the same to the model");
            }
            assert!(a.contains("sub-1"), "outcome must name the child: {a}");
        }

        assert!(rendered[0].starts_with("Delivered"), "{}", rendered[0]);
        // Everything else must be unmistakably a non-delivery.
        for r in &rendered[1..] {
            assert!(!r.starts_with("Delivered"), "{r}");
        }
        // A dropped message must not read as "queued".
        assert!(rendered[1].contains("not queued"), "{}", rendered[1]);
        assert!(
            rendered[2].contains("Nothing was queued"),
            "{}",
            rendered[2]
        );
        // A finished child is resumed, not re-messaged.
        assert!(rendered[3].contains("resume_from"), "{}", rendered[3]);
        assert!(rendered[3].contains("new id"), "{}", rendered[3]);
        // Sibling tools are named as this toolset names them.
        assert!(
            rendered[3].contains("get_command_or_subagent_output"),
            "{}",
            rendered[3]
        );
        assert!(rendered[4].contains("spawn_subagent"), "{}", rendered[4]);
    }

    fn unwrap_message(event: SubagentEvent) -> SubagentMessageRequest {
        match event {
            SubagentEvent::Message(r) => r,
            _ => panic!("expected SubagentEvent::Message"),
        }
    }

    #[tokio::test]
    async fn a_delivered_message_reaches_the_coordinator_verbatim() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
        let mut resources = Resources::new();
        resources.insert(SubagentBackendResource(Arc::new(ChannelBackend::new(tx))));
        let shared = resources.into_shared();

        let coordinator = tokio::spawn(async move {
            let req = unwrap_message(rx.recv().await.unwrap());
            assert_eq!(req.subagent_id, "sub-7");
            assert_eq!(req.text, "stop rewriting the parser");
            req.respond_to
                .send(SubagentMessageOutcome::Delivered)
                .unwrap();
        });

        let out = xai_tool_runtime::Tool::run(
            &MessageSubagentTool,
            test_ctx_with_call_id(shared, "call-1"),
            MessageSubagentInput {
                subagent_id: "sub-7".into(),
                message: "stop rewriting the parser".into(),
            },
        )
        .await
        .unwrap();

        coordinator.await.unwrap();
        assert!(text_of(out).starts_with("Delivered to sub-7"));
    }

    /// An empty message is refused before the coordinator is touched: handing a
    /// child a blank system reminder spends one of its injection points on
    /// nothing, and the model gets no signal that it did.
    #[tokio::test]
    async fn an_empty_message_never_reaches_the_child() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
        let mut resources = Resources::new();
        resources.insert(SubagentBackendResource(Arc::new(ChannelBackend::new(tx))));
        let shared = resources.into_shared();

        let out = xai_tool_runtime::Tool::run(
            &MessageSubagentTool,
            test_ctx_with_call_id(shared, "call-2"),
            MessageSubagentInput {
                subagent_id: "sub-7".into(),
                message: "   \n".into(),
            },
        )
        .await
        .unwrap();

        assert!(text_of(out).contains("was empty"));
        assert!(rx.try_recv().is_err(), "no event should have been sent");
    }

    /// Without subagent support there is no child to reach; the tool says so
    /// instead of erroring, because the model can still finish its turn.
    #[tokio::test]
    async fn no_backend_is_an_answer_not_an_error() {
        let shared = Resources::new().into_shared();
        let out = xai_tool_runtime::Tool::run(
            &MessageSubagentTool,
            test_ctx_with_call_id(shared, "call-3"),
            MessageSubagentInput {
                subagent_id: "sub-7".into(),
                message: "hello".into(),
            },
        )
        .await
        .unwrap();
        assert!(text_of(out).contains("not available"));
    }
}
