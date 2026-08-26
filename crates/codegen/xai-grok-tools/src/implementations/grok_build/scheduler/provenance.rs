//! Who created a scheduled task, and who may change it afterwards.
//!
//! A subagent reuses its parent's scheduler actor so scheduled tasks survive
//! subagent exit. That makes the scheduler a shared table: with nesting, several
//! agents write into one list, any of them can see the whole list, and the
//! creator is usually gone by the time a task fires. [`TaskCreator`] is stamped
//! at create time so the list can say where a row came from, and
//! [`ensure_caller_may_modify`] keeps a subagent from changing rows it did not
//! write.
//!
//! The rule is deliberately asymmetric:
//!
//! - The session that owns the actor (depth 0, or any embedder that never wires
//!   subagent resources) may change every task. It has to be able to: a subagent
//!   that scheduled something and exited can no longer clean up after itself, and
//!   strict per-creator ownership would strand those tasks for their full TTL.
//! - A subagent may change only what it created. Cleaning up a sibling's task is
//!   something it should report upwards, not do.

use crate::implementations::grok_build::task::types::{
    AgentTypeResource, SessionIdResource, SubagentDepthCounter,
};
use crate::types::resources::SharedResources;

use super::types::{SchedulerCommand, SchedulerHandle, TaskCreator};

/// Identify the session running the current tool call.
///
/// `None` when the host wires no session id — an embedder without subagents,
/// where there is no second writer to distinguish from.
pub(super) async fn caller(resources: &SharedResources) -> Option<TaskCreator> {
    let res = resources.lock().await;
    let session_id = res.get::<SessionIdResource>()?.0.clone();
    Some(TaskCreator {
        session_id,
        agent: res.get::<AgentTypeResource>().map(|agent| agent.0.clone()),
        depth: res.get::<SubagentDepthCounter>().map_or(0, |depth| depth.0),
    })
}

/// Whether `caller` is allowed to change the task `created_by` created.
pub(super) fn may_modify(caller: Option<&TaskCreator>, created_by: Option<&TaskCreator>) -> bool {
    let Some(caller) = caller.filter(|caller| caller.is_subagent()) else {
        return true;
    };
    created_by.is_some_and(|created_by| created_by.session_id == caller.session_id)
}

/// Reject an update or delete aimed at another agent's task.
///
/// An id that matches nothing passes: the actor owns the canonical
/// "no scheduled task with id" error, and duplicating it here would let the two
/// wordings drift apart.
pub(super) async fn ensure_caller_may_modify(
    sender: &tokio::sync::mpsc::UnboundedSender<SchedulerCommand>,
    caller: Option<&TaskCreator>,
    task_id: &str,
) -> Result<(), xai_tool_runtime::ToolError> {
    if caller.is_none_or(|caller| !caller.is_subagent()) {
        return Ok(());
    }

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    sender
        .send(SchedulerCommand::List { reply: reply_tx })
        .map_err(|_| {
            xai_tool_runtime::ToolError::custom("process_manager", "Scheduler actor stopped")
        })?;
    let snapshot = reply_rx.await.map_err(|_| {
        xai_tool_runtime::ToolError::custom("process_manager", "Scheduler actor dropped reply")
    })?;

    let Some(task) = snapshot.tasks.iter().find(|task| task.id == task_id) else {
        return Ok(());
    };
    if may_modify(caller, task.created_by.as_ref()) {
        return Ok(());
    }

    Err(xai_tool_runtime::ToolError::custom(
        "scheduler_not_owner",
        format!(
            "scheduled task {task_id} was created by {} and cannot be changed from a subagent; \
             report it to whoever spawned you and let them decide",
            super::types::describe_creator(
                task.created_by.as_ref(),
                caller.map(|caller| caller.session_id.as_str()),
            ),
        ),
    ))
}

/// Handle lookup shared by every scheduler tool.
pub(super) async fn scheduler_sender(
    resources: &SharedResources,
) -> Result<tokio::sync::mpsc::UnboundedSender<SchedulerCommand>, xai_tool_runtime::ToolError> {
    let res = resources.lock().await;
    Ok(res
        .get::<SchedulerHandle>()
        .ok_or_else(|| xai_tool_runtime::ToolError::custom("missing_resource", "SchedulerHandle"))?
        .0
        .clone())
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use crate::types::resources::{Resources, State};

    use super::super::actor::SchedulerActor;
    use super::super::types::{ScheduledTask, SchedulerState};

    /// One scheduler actor with the resource maps of the sessions that share it.
    ///
    /// Mirrors the real topology: the actor and its state belong to the root
    /// session, and a subagent gets a resource map of its own that holds nothing
    /// but a clone of the root's [`SchedulerHandle`].
    pub(in crate::implementations::grok_build::scheduler) struct SharedScheduler {
        pub root: SharedResources,
        sender: tokio::sync::mpsc::UnboundedSender<SchedulerCommand>,
        pub cancel: tokio_util::sync::CancellationToken,
    }

    impl SharedScheduler {
        pub fn start(root_session_id: &str) -> Self {
            let (sender, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut resources = Resources::new();
            resources.register_state::<SchedulerState>();
            resources.insert(SchedulerHandle(sender.clone()));
            resources.insert(SessionIdResource(root_session_id.to_string()));
            resources.insert(SubagentDepthCounter(0));
            let root = resources.into_shared();

            // Removal is durable: it refuses to proceed without a consumer that
            // acknowledges the tombstone, so these tests need one.
            let (notification_handle, mut notifications) =
                crate::notification::types::ToolNotificationHandle::acknowledged_channel();
            tokio::spawn(async move {
                while let Some(delivery) = notifications.recv().await {
                    if let Some(acknowledgement) = delivery.acknowledgement {
                        let _ = acknowledgement.send(Ok(()));
                    }
                }
            });
            let cancel = tokio_util::sync::CancellationToken::new();
            tokio::spawn(
                SchedulerActor {
                    resources: root.clone(),
                    resources_persistence: std::sync::Arc::new(
                        crate::persistence::ResourcesPersistence::noop(),
                    ),
                    notification_handle,
                    cmd_rx,
                    cancel_token: cancel.clone(),
                    clock: Default::default(),
                    pending_removal: None,
                    blocked_expiries: Default::default(),
                }
                .run(),
            );
            Self {
                root,
                sender,
                cancel,
            }
        }

        pub fn subagent(&self, session_id: &str, agent: &str, depth: u32) -> SharedResources {
            let mut resources = Resources::new();
            resources.insert(SchedulerHandle(self.sender.clone()));
            resources.insert(SessionIdResource(session_id.to_string()));
            resources.insert(AgentTypeResource(agent.to_string()));
            resources.insert(SubagentDepthCounter(depth));
            resources.into_shared()
        }

        pub async fn tasks(&self) -> Vec<ScheduledTask> {
            let res = self.root.lock().await;
            res.get::<State<SchedulerState>>()
                .map(|state| state.tasks.clone())
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creator(session: &str, depth: u32) -> TaskCreator {
        TaskCreator {
            session_id: session.to_string(),
            agent: Some("general-purpose".to_string()),
            depth,
        }
    }

    #[test]
    fn root_caller_may_modify_anything() {
        let root = creator("root", 0);
        assert!(may_modify(Some(&root), Some(&creator("child", 1))));
        assert!(may_modify(Some(&root), None));
        // An embedder that wires no session id keeps the pre-provenance behaviour.
        assert!(may_modify(None, Some(&creator("child", 1))));
    }

    #[test]
    fn subagent_may_modify_only_its_own() {
        let child = creator("child", 1);
        assert!(may_modify(Some(&child), Some(&creator("child", 1))));
        assert!(!may_modify(Some(&child), Some(&creator("sibling", 1))));
        assert!(!may_modify(Some(&child), Some(&creator("root", 0))));
        // Pre-provenance tasks belong to the main session, which is still around
        // to remove them.
        assert!(!may_modify(Some(&child), None));
    }
}
