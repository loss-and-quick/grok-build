use crate::types::requirements::{Expr, ToolRequirement};

use crate::types::tool::{ToolKind, ToolNamespace};

use super::types::{SchedulerCommand, scheduler_tool_error};

/// Canonical tool name advertised by `SchedulerDeleteTool::id()`.
/// See note on `SCHEDULER_CREATE_TOOL_NAME`.
pub const SCHEDULER_DELETE_TOOL_NAME: &str = "scheduler_delete";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SchedulerDeleteInput {
    /// The scheduled task ID to cancel.
    #[schemars(description = "The task ID to cancel (from scheduler_create output)")]
    pub id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SchedulerDeleteOutput {
    pub success: bool,
    pub message: String,
}

impl xai_tool_runtime::ToolOutput for SchedulerDeleteOutput {}

#[derive(Debug, Default)]
pub struct SchedulerDeleteTool;

impl crate::types::tool_metadata::ToolMetadata for SchedulerDeleteTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        r#"Cancel a scheduled task by ID.

Returns success: true if the task was found and removed, false if no task with that ID exists.

The scheduler is shared with the session that spawned you: from a subagent, only tasks you created yourself can be cancelled (`createdHere` in scheduler_list). Report anything else upwards instead."#
    }

    fn emitted_notifications(&self) -> &'static [&'static str] {
        &["ScheduledTaskRemoved"]
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

impl xai_tool_runtime::Tool for SchedulerDeleteTool {
    type Args = SchedulerDeleteInput;
    type Output = SchedulerDeleteOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("scheduler_delete").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "scheduler_delete",
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

    #[tracing::instrument(
        name = "tool.scheduler_delete",
        skip_all,
        fields(id = %input.id)
    )]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: SchedulerDeleteInput,
    ) -> Result<SchedulerDeleteOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let sender = super::provenance::scheduler_sender(&resources).await?;
        let caller = super::provenance::caller(&resources).await;
        super::provenance::ensure_caller_may_modify(&sender, caller.as_ref(), &input.id).await?;

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        sender
            .send(SchedulerCommand::Delete {
                id: input.id.clone(),
                reply: reply_tx,
            })
            .map_err(|_| {
                xai_tool_runtime::ToolError::custom("process_manager", "Scheduler actor stopped")
            })?;

        let removed = reply_rx
            .await
            .map_err(|_| {
                xai_tool_runtime::ToolError::custom(
                    "process_manager",
                    "Scheduler actor dropped reply",
                )
            })?
            .map_err(scheduler_tool_error)?;

        if removed {
            Ok(SchedulerDeleteOutput {
                success: true,
                message: format!("Scheduled task {} cancelled.", input.id),
            })
        } else {
            Ok(SchedulerDeleteOutput {
                success: false,
                message: format!(
                    "No scheduled task with ID {} found. Use scheduler_list to see active tasks.",
                    input.id
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::create::{SchedulerCreateInput, SchedulerCreateTool};
    use super::super::provenance::test_support::SharedScheduler;
    use super::*;
    use crate::types::tool_metadata::test_ctx;
    use xai_tool_runtime::Tool;

    fn create(json: serde_json::Value) -> SchedulerCreateInput {
        serde_json::from_value(json).expect("valid input json")
    }

    async fn task_created_by(resources: crate::types::resources::SharedResources) -> String {
        SchedulerCreateTool
            .run(
                test_ctx(resources),
                create(serde_json::json!({"interval": "5m", "prompt": "check deploy"})),
            )
            .await
            .expect("create succeeds")
            .id
    }

    #[tokio::test]
    async fn subagent_cannot_delete_a_task_it_did_not_create() {
        let scheduler = SharedScheduler::start("root");
        let id = task_created_by(scheduler.root.clone()).await;

        let child = scheduler.subagent("child", "general-purpose", 1);
        let err = SchedulerDeleteTool
            .run(test_ctx(child), SchedulerDeleteInput { id: id.clone() })
            .await
            .expect_err("a subagent must not cancel the root's task");
        assert!(
            err.to_string()
                .contains("cannot be changed from a subagent")
        );
        assert_eq!(scheduler.tasks().await.len(), 1, "task survives");
        scheduler.cancel.cancel();
    }

    #[tokio::test]
    async fn subagent_can_delete_its_own_task() {
        let scheduler = SharedScheduler::start("root");
        let child = scheduler.subagent("child", "general-purpose", 1);
        let id = task_created_by(child.clone()).await;

        let result = SchedulerDeleteTool
            .run(test_ctx(child), SchedulerDeleteInput { id })
            .await
            .expect("its own task stays cancellable");
        assert!(result.success);
        scheduler.cancel.cancel();
    }

    /// The reason ownership is not symmetric: a subagent that scheduled
    /// something and exited can never clean up after itself, so the session
    /// that owns the actor has to be able to.
    #[tokio::test]
    async fn root_can_delete_a_task_left_behind_by_a_subagent() {
        let scheduler = SharedScheduler::start("root");
        let child = scheduler.subagent("child", "general-purpose", 1);
        let id = task_created_by(child).await;

        let result = SchedulerDeleteTool
            .run(
                test_ctx(scheduler.root.clone()),
                SchedulerDeleteInput { id },
            )
            .await
            .expect("the root session is the janitor of record");
        assert!(result.success);
        assert!(scheduler.tasks().await.is_empty());
        scheduler.cancel.cancel();
    }

    /// An unknown id must keep reporting as not-found rather than as a
    /// permission problem, so the actor stays the single source of that wording.
    #[tokio::test]
    async fn unknown_id_still_reports_not_found_to_a_subagent() {
        let scheduler = SharedScheduler::start("root");
        let child = scheduler.subagent("child", "general-purpose", 1);

        let result = SchedulerDeleteTool
            .run(
                test_ctx(child),
                SchedulerDeleteInput {
                    id: "nonexistent".to_string(),
                },
            )
            .await
            .expect("no task, no error");
        assert!(!result.success);
        assert!(result.message.contains("No scheduled task"));
        scheduler.cancel.cancel();
    }
}
