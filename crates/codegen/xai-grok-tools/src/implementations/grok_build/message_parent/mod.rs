//! `message_parent` tool — a running subagent reporting back up.
//!
//! The reverse of
//! [`send_subagent_message`](super::send_subagent_message), and deliberately
//! not its mirror image — which is why it is a tool of its own rather than a
//! direction argument on that one. That tool is an instrument of control: a
//! parent redirects a child it owns, over an admission it can be told the fate
//! of, and an idle child is queued a turn. This one is an instrument of report:
//! a child tells the agent that spawned it something the parent needs before
//! the child finishes, and then goes straight back to work. Nothing here queues
//! and nothing here wakes, because the sender is below the receiver.
//!
//! Three properties keep the pair from becoming a conversation.
//!
//! **A hard cap.** A child may send
//! [`MAX_PARENT_MESSAGES_PER_SUBAGENT`] messages for its whole life, counted on
//! the coordinator's registry entry where the child cannot reach it. With a
//! reverse route open, "child reports, parent replies, child reports again" is
//! a cycle both models can sustain indefinitely, and nothing else bounds it —
//! a message continues a turn rather than starting one, so no turn budget is
//! spent, and a sampler loop may iterate as long as it likes. The cap bounds
//! the exchange whatever either model decides.
//!
//! **No wake.** A parent between turns is not started into one. Its child's
//! *completion* can wake it, through a path already gated on whether anything
//! is awaiting that child; a mid-task report carries no such gate.
//!
//! **No waiting.** The call returns as soon as the parent has taken the text,
//! never once the parent has read it. A parent that foreground-spawned this
//! child is parked inside that `task` call and reaches no injection point until
//! the child finishes, so waiting for a read would be waiting for itself.
//!
//! There is no addressee argument, and that is the design, not an omission. The
//! coordinator resolves the parent from its own registry, so the channel
//! reaches exactly one session and cannot be turned into an agent-to-agent bus.

use crate::implementations::grok_build::task::backend::SubagentBackendResource;
use crate::implementations::grok_build::task::types::{
    MAX_PARENT_MESSAGES_PER_SUBAGENT, ParentMessageOutcome,
};
use crate::types::output::ToolOutput;
use crate::types::requirements::Expr;
use crate::types::tool::{ToolKind, ToolNamespace};
use serde::{Deserialize, Serialize};

/// Registered name of the `message_parent` tool.
pub const MESSAGE_PARENT_TOOL_NAME: &str = "message_parent";

/// Input for the `message_parent` tool.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct MessageParentInput {
    /// What to tell the agent that spawned you. Plain text, delivered as-is.
    /// Write it as a report, not as a question: you keep working either way.
    pub message: String,
}

/// Output schema for `message_parent` (JSON Schema generation only).
#[derive(Debug, schemars::JsonSchema)]
pub struct MessageParentOutput {
    /// Whether the parent took the message, and how many sends remain.
    pub result: String,
}

#[derive(Debug, Default)]
pub struct MessageParentTool;

impl crate::types::tool_metadata::ToolMetadata for MessageParentTool {
    fn kind(&self) -> ToolKind {
        ToolKind::MessageParentAction
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Send a message to the agent that spawned you, while you are still working. The \
         text lands in that agent's conversation before its next step, so a wrong \
         assumption in your brief gets corrected now instead of after you have built on \
         it.\n\n\
         Use this for the few things that cannot wait for your final result: a premise in \
         your instructions that turns out to be false, a blocker only the agent that sent \
         you can clear, or a finding that changes what other subagents should be doing. \
         Everything else belongs in your result, which reaches that agent anyway.\n\n\
         You do not wait for an answer and you do not stop working. The result tells you \
         only whether the message was taken. If the parent needs to change your course it \
         will message you back; keep going until it does.\n\n\
         You may send at most 3 messages for the whole task, and a message that finds the \
         parent between turns is dropped rather than queued — that still spends one of the \
         3, so do not retry. When they are gone, put anything further in your final \
         result.\n\n\
         No addressee: this reaches the agent that spawned you and nobody else. It cannot \
         reach a sibling subagent — report to the parent and let it redirect the sibling."
    }

    /// No dependency on any other tool. Reaching a parent needs no spawner in
    /// this toolset — being spawned is not something the toolset can express —
    /// so availability is settled where the session knows whether it is a
    /// subagent at all, and the runtime answers `NoParent` if it slips through.
    fn requires_expr(&self) -> Expr<crate::types::requirements::ToolRequirement> {
        Expr::True
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

impl xai_tool_runtime::Tool for MessageParentTool {
    type Args = MessageParentInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(MESSAGE_PARENT_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            MESSAGE_PARENT_TOOL_NAME,
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    /// Mutating: it changes what another agent does. `ToolScope::Write` keeps
    /// the computer hub routing it to the leader, where the coordinator lives.
    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(xai_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.message_parent", skip_all)]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: MessageParentInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let backend = {
            let res = resources.lock().await;
            res.get::<SubagentBackendResource>().cloned()
        };
        let Some(backend) = backend else {
            return Ok(ToolOutput::Text(
                "Subagents are not available in this session, so there is no parent agent \
                 to reach."
                    .into(),
            ));
        };

        // Refused before the budget is touched: an empty report would spend one
        // of three sends and one of the parent's injection points on nothing.
        if input.message.trim().is_empty() {
            return Ok(ToolOutput::Text(
                "Nothing sent: `message` was empty, and no send was charged. Say what the \
                 agent that spawned you needs to know."
                    .into(),
            ));
        }

        let outcome = backend.backend().message_parent(&input.message).await;
        Ok(ToolOutput::Text(render_outcome(outcome).into()))
    }
}

/// Turn one [`ParentMessageOutcome`] into the sentence the model acts on.
///
/// Every answer says what to do next, and every one of them says "keep
/// working". A child that reads any of these as "wait for a reply" has stalled
/// for a parent that may not answer at all.
fn render_outcome(outcome: ParentMessageOutcome) -> String {
    match outcome {
        ParentMessageOutcome::Accepted { remaining: 0 } => format!(
            "Taken into the running turn of the agent that spawned you; it reads this at \
             its next step unless that turn is cancelled first. That was your last of \
             {MAX_PARENT_MESSAGES_PER_SUBAGENT} messages — anything further has to go in \
             your final result. Keep working; you will be messaged back only if your \
             course changes."
        ),
        ParentMessageOutcome::Accepted { remaining } => format!(
            "Taken into the running turn of the agent that spawned you; it reads this at \
             its next step unless that turn is cancelled first. {remaining} of \
             {MAX_PARENT_MESSAGES_PER_SUBAGENT} messages left. Keep working; you will be \
             messaged back only if your course changes."
        ),
        ParentMessageOutcome::NoTurnRunning => format!(
            "Not delivered: the agent that spawned you is between turns, so there was no \
             conversation to put this in. It was dropped, not queued, and it will not be \
             woken for it — do not send it again, the retry would cost another of your \
             {MAX_PARENT_MESSAGES_PER_SUBAGENT} messages and find the same. Put it in \
             your final result instead; that is what reaches an idle parent."
        ),
        ParentMessageOutcome::BudgetExhausted { limit } => format!(
            "Not sent: you have already sent {limit} messages, which is the limit for one \
             task. There are no more. Everything left goes in your final result."
        ),
        ParentMessageOutcome::Unreachable => {
            "Not delivered: the agent that spawned you is no longer reachable (finishing, \
             or torn down). Finish your task and put anything it needs in your result."
                .to_string()
        }
        ParentMessageOutcome::NoParent => {
            "Nothing sent: no agent spawned this session, so there is no parent to \
             message. This tool only works inside a subagent."
                .to_string()
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementations::grok_build::task::backend::ChannelBackend;
    use crate::implementations::grok_build::task::types::{
        SubagentEvent, SubagentParentMessageRequest,
    };
    use crate::types::resources::Resources;
    use crate::types::tool_metadata::{ToolMetadata, test_ctx_with_call_id};

    fn text_of(output: ToolOutput) -> String {
        match output {
            ToolOutput::Text(t) => t.text.to_string(),
            other => panic!("expected Text output, got {other:?}"),
        }
    }

    fn unwrap_report(event: SubagentEvent) -> SubagentParentMessageRequest {
        match event {
            SubagentEvent::MessageParent(r) => r,
            _ => panic!("expected SubagentEvent::MessageParent"),
        }
    }

    #[test]
    fn tool_identity() {
        let tool = MessageParentTool;
        assert_eq!(
            xai_tool_runtime::Tool::id(&tool).as_str(),
            MESSAGE_PARENT_TOOL_NAME
        );
        assert_eq!(
            ToolMetadata::kind(&tool),
            ToolKind::MessageParentAction,
            "the toolset plumbing keys off the kind, not the name"
        );
        assert!(!ToolMetadata::is_read_only(&tool));
    }

    /// The description must carry the three facts that keep a child from
    /// stalling or looping: the cap, that a dropped message must not be
    /// retried, and that it never waits for an answer.
    #[test]
    fn description_states_the_cap_and_that_nobody_waits() {
        let rendered = ToolMetadata::description_template(&MessageParentTool);
        assert!(rendered.contains("at most 3"), "{rendered}");
        assert!(rendered.contains("do not retry"), "{rendered}");
        assert!(rendered.contains("do not wait"), "{rendered}");
        // A child must not go hunting for a sibling channel that does not exist.
        assert!(rendered.contains("sibling"), "{rendered}");
    }

    /// Six outcomes, six answers, and none of them tells the child to wait.
    #[test]
    fn each_outcome_names_its_own_follow_up() {
        let rendered: Vec<String> = [
            ParentMessageOutcome::Accepted { remaining: 2 },
            ParentMessageOutcome::Accepted { remaining: 0 },
            ParentMessageOutcome::NoTurnRunning,
            ParentMessageOutcome::BudgetExhausted {
                limit: MAX_PARENT_MESSAGES_PER_SUBAGENT,
            },
            ParentMessageOutcome::Unreachable,
            ParentMessageOutcome::NoParent,
        ]
        .into_iter()
        .map(render_outcome)
        .collect();

        for (i, a) in rendered.iter().enumerate() {
            for b in rendered.iter().skip(i + 1) {
                assert_ne!(a, b, "two outcomes read the same to the model");
            }
        }
        // A dropped message must not read as queued, and must not invite a retry.
        assert!(rendered[2].contains("not queued"), "{}", rendered[2]);
        assert!(
            rendered[2].contains("do not send it again"),
            "{}",
            rendered[2]
        );
        // A spent budget points at the result, which is the honest fallback.
        assert!(rendered[3].contains("final result"), "{}", rendered[3]);
        // Every accepted answer says the child keeps going.
        assert!(rendered[0].contains("Keep working"), "{}", rendered[0]);
        assert!(rendered[1].contains("Keep working"), "{}", rendered[1]);
    }

    #[tokio::test]
    async fn a_report_reaches_the_coordinator_verbatim_and_names_its_sender() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
        let mut resources = Resources::new();
        resources.insert(ChannelBackend::for_session(tx, "child-session-4").into_resource());
        let shared = resources.into_shared();

        let coordinator = tokio::spawn(async move {
            let req = unwrap_report(rx.recv().await.unwrap());
            assert_eq!(req.child_session_id, "child-session-4");
            assert_eq!(req.text, "the repo is Rust, not Go");
            req.respond_to
                .send(ParentMessageOutcome::Accepted { remaining: 2 })
                .unwrap();
        });

        let out = xai_tool_runtime::Tool::run(
            &MessageParentTool,
            test_ctx_with_call_id(shared, "call-1"),
            MessageParentInput {
                message: "the repo is Rust, not Go".into(),
            },
        )
        .await
        .unwrap();

        coordinator.await.unwrap();
        assert!(text_of(out).starts_with("Taken into the running turn"));
    }

    /// An empty message is refused before the budget is touched: three sends is
    /// few enough that spending one on a blank reminder is a real loss.
    #[tokio::test]
    async fn an_empty_message_costs_nothing_and_never_leaves() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
        let mut resources = Resources::new();
        resources.insert(ChannelBackend::for_session(tx, "child-session-4").into_resource());
        let shared = resources.into_shared();

        let out = xai_tool_runtime::Tool::run(
            &MessageParentTool,
            test_ctx_with_call_id(shared, "call-2"),
            MessageParentInput {
                message: "   \n".into(),
            },
        )
        .await
        .unwrap();

        let text = text_of(out);
        assert!(text.contains("was empty"), "{text}");
        assert!(text.contains("no send was charged"), "{text}");
        assert!(rx.try_recv().is_err(), "no event should have been sent");
    }

    /// Without subagent support there is no coordinator holding a parent for
    /// this session; the tool says so instead of erroring.
    #[tokio::test]
    async fn no_backend_is_an_answer_not_an_error() {
        let shared = Resources::new().into_shared();
        let out = xai_tool_runtime::Tool::run(
            &MessageParentTool,
            test_ctx_with_call_id(shared, "call-3"),
            MessageParentInput {
                message: "hello".into(),
            },
        )
        .await
        .unwrap();
        assert!(text_of(out).contains("no parent agent"));
    }

    /// An unbound backend cannot name the session that is speaking, so there is
    /// nothing to resolve a parent from — the root-session answer.
    #[tokio::test]
    async fn a_session_that_cannot_name_itself_has_no_parent() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
        let mut resources = Resources::new();
        resources.insert(ChannelBackend::new(tx).into_resource());
        let shared = resources.into_shared();

        let out = xai_tool_runtime::Tool::run(
            &MessageParentTool,
            test_ctx_with_call_id(shared, "call-4"),
            MessageParentInput {
                message: "hello".into(),
            },
        )
        .await
        .unwrap();

        let text = text_of(out);
        assert!(text.contains("no parent to"), "{text}");
        assert!(rx.try_recv().is_err(), "unbound backends send no event");
    }
}
