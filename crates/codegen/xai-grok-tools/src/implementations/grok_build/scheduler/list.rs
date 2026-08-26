use crate::types::requirements::{Expr, ToolRequirement};

use crate::types::tool::{ToolKind, ToolNamespace};

use super::interval::interval_to_human;
use super::types::{SchedulerCommand, describe_creator};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SchedulerListInput {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskSummary {
    pub id: String,
    pub prompt: String,
    pub interval_human: String,
    pub next_fire_at: String,
    pub created_at: String,
    pub recurring: bool,
    /// Who scheduled it, relative to whoever is reading — the list is shared
    /// across every agent that reuses this scheduler actor.
    pub created_by: String,
    /// Whether this session created it. Only tasks it created are a subagent's
    /// to update or delete; anything else it should report upwards instead.
    pub created_here: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SchedulerListOutput {
    pub tasks: Vec<ScheduledTaskSummary>,
}

impl xai_tool_runtime::ToolOutput for SchedulerListOutput {}

#[derive(Debug, Default)]
pub struct SchedulerListTool;

impl crate::types::tool_metadata::ToolMetadata for SchedulerListTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "List all active scheduled tasks with their IDs, prompts, intervals, next fire times, \
         and who created them. Subagents share the scheduler with the session that spawned \
         them, so this list spans all of them: `createdHere` marks the tasks this session \
         created, and only those are a subagent's to update or delete."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        use super::create::SchedulerCreateTool;
        use crate::types::tool_metadata::ToolMetadata as TM;
        Expr::Value(ToolRequirement::Tool {
            namespace: TM::tool_namespace(&SchedulerCreateTool).to_string(),
            id: xai_tool_runtime::Tool::id(&SchedulerCreateTool).to_string(),
            if_params: None,
        })
    }
}

impl xai_tool_runtime::Tool for SchedulerListTool {
    type Args = SchedulerListInput;
    type Output = SchedulerListOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("scheduler_list").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "scheduler_list",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(xai_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.scheduler_list", skip_all)]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        _input: SchedulerListInput,
    ) -> Result<SchedulerListOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let sender = super::provenance::scheduler_sender(&resources).await?;
        let caller = super::provenance::caller(&resources).await;
        let caller_session = caller.as_ref().map(|caller| caller.session_id.clone());

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        sender
            .send(SchedulerCommand::List { reply: reply_tx })
            .map_err(|_| {
                xai_tool_runtime::ToolError::execution(
                    xai_tool_protocol::ToolId::new("scheduler_list").expect("valid"),
                    "Scheduler actor stopped",
                )
            })?;

        let snapshot = reply_rx.await.map_err(|_| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("scheduler_list").expect("valid"),
                "Scheduler actor dropped reply",
            )
        })?;

        let summaries = snapshot
            .tasks
            .into_iter()
            .map(|t| {
                let next_fire = t.next_fire_at().to_rfc3339();
                let created = t.created_at.to_rfc3339();
                let created_by = describe_creator(t.created_by.as_ref(), caller_session.as_deref());
                let created_here = t
                    .created_by
                    .as_ref()
                    .zip(caller_session.as_deref())
                    .is_some_and(|(creator, session)| creator.session_id == session);
                let prompt = if t.prompt.len() > 80 {
                    let cut = crate::util::floor_char_boundary(&t.prompt, 80);
                    format!("{}...", &t.prompt[..cut])
                } else {
                    t.prompt
                };
                ScheduledTaskSummary {
                    id: t.id,
                    prompt,
                    interval_human: interval_to_human(t.interval_secs),
                    next_fire_at: next_fire,
                    created_at: created,
                    recurring: t.recurring,
                    created_by,
                    created_here,
                }
            })
            .collect();

        Ok(SchedulerListOutput { tasks: summaries })
    }
}

#[cfg(test)]
mod tests {
    use super::super::create::{SchedulerCreateInput, SchedulerCreateTool};
    use super::super::provenance::test_support::SharedScheduler;
    use super::*;
    use crate::types::resources::SharedResources;
    use crate::types::tool_metadata::test_ctx;
    use xai_tool_runtime::Tool;

    async fn schedule(resources: SharedResources, prompt: &str) {
        let input: SchedulerCreateInput =
            serde_json::from_value(serde_json::json!({"interval": "5m", "prompt": prompt}))
                .expect("valid input json");
        SchedulerCreateTool
            .run(test_ctx(resources), input)
            .await
            .expect("create succeeds");
    }

    async fn list(resources: SharedResources) -> Vec<ScheduledTaskSummary> {
        SchedulerListTool
            .run(test_ctx(resources), SchedulerListInput {})
            .await
            .expect("list succeeds")
            .tasks
    }

    /// The list is shared, so every row is named from the reader's own vantage
    /// point: a bare session id would tell neither the user nor the model
    /// anything, and the model never sees its own id to compare against.
    #[tokio::test]
    async fn every_row_names_its_creator_relative_to_the_reader() {
        let scheduler = SharedScheduler::start("root");
        let child = scheduler.subagent("child", "general-purpose", 1);
        schedule(scheduler.root.clone(), "the root's task").await;
        schedule(child.clone(), "the subagent's task").await;

        let from_root = list(scheduler.root.clone()).await;
        assert_eq!(from_root[0].created_by, "this session");
        assert!(from_root[0].created_here);
        assert_eq!(
            from_root[1].created_by,
            "a general-purpose subagent (depth 1)"
        );
        assert!(!from_root[1].created_here);

        let from_child = list(child).await;
        assert_eq!(from_child[0].created_by, "the main session");
        assert!(!from_child[0].created_here);
        assert_eq!(from_child[1].created_by, "this session");
        assert!(from_child[1].created_here);

        scheduler.cancel.cancel();
    }

    /// Tasks restored from state written before creators were recorded stay
    /// listable; they simply say so.
    #[tokio::test]
    async fn a_task_without_a_recorded_creator_says_so() {
        let scheduler = SharedScheduler::start("root");
        schedule(scheduler.root.clone(), "legacy").await;
        {
            let mut res = scheduler.root.lock().await;
            res.get_or_default::<crate::types::resources::State<super::super::types::SchedulerState>>()
                .tasks[0]
                .created_by = None;
        }

        let listed = list(scheduler.root.clone()).await;
        assert_eq!(
            listed[0].created_by,
            super::super::types::UNRECORDED_CREATOR
        );
        assert!(!listed[0].created_here);
        scheduler.cancel.cancel();
    }
}
