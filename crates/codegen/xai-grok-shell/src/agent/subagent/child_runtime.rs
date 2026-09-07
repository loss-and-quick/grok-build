use tokio::sync::{mpsc, oneshot};
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    ActiveMessageAdmission, ChildControl, LocalBoxFuture, ParentReportDelivery, SendBoxFuture,
    SubagentProgress,
};
use xai_grok_tools::implementations::grok_build::task::types::ActiveAgentMessageDelivery;
use xai_message_delivery_core::DeliveryEnvelope;

use super::prompt_turn_receipt::{PromptTurnReceipt, cancel_shell_child_turn};
use crate::session::{SessionCommand, SessionThread};

/// Shell runtime handle retained while a child is active.
pub(crate) struct ShellChildRuntime {
    pub(crate) child_cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    pub(crate) message_delivery: crate::session::message_delivery::MessageDeliveryHandle,
    pub(crate) active_message_target_session_id: String,
    pub(crate) child_signals: crate::session::signals::SessionSignalsHandle,
    /// Held by the worker until promotion succeeds.
    /// `None` means the caller still owns the join handle, so a cancel during promotion can wait for the actor to exit.
    pub(crate) _child_thread: Option<SessionThread>,
    pub(crate) receipt_sink: mpsc::Sender<PromptTurnReceipt>,
    #[cfg(test)]
    pub(crate) force_queue_envelope: bool,
    pub(crate) active_message_parent_session_id: String,
    pub(crate) active_message_parent_prompt_index: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// The parent session's mailbox, for the child's reports back up. `None`
    /// when the parent handle was already gone at spawn — the child still runs,
    /// it simply has nowhere to report to.
    pub(crate) parent_cmd_tx: Option<mpsc::UnboundedSender<SessionCommand>>,
    /// The child's id as its parent knows it, carried in the report so the
    /// parent can answer the right subagent.
    pub(crate) subagent_id: String,
}

impl ChildControl for ShellChildRuntime {
    type ProgressFuture = LocalBoxFuture<SubagentProgress>;
    type ParentReportFuture = LocalBoxFuture<ParentReportDelivery>;

    fn progress(&self) -> Self::ProgressFuture {
        let signals = self.child_signals.clone();
        Box::pin(async move {
            signals
                .snapshot()
                .await
                .map(|snapshot| SubagentProgress {
                    turn_count: snapshot.turn_count,
                    tool_call_count: snapshot.tool_call_count,
                    tokens_used: snapshot.context_tokens_used,
                    context_window_tokens: snapshot.context_window_tokens,
                    context_usage_pct: snapshot.context_window_usage,
                    tools_used: snapshot.tools_used,
                    error_count: snapshot.error_count,
                })
                .unwrap_or_default()
        })
    }

    fn send_active_message(
        &self,
        delivery: ActiveAgentMessageDelivery,
    ) -> SendBoxFuture<ActiveMessageAdmission> {
        let message_delivery = self.message_delivery.clone();
        let receipt_sink = self.receipt_sink.clone();
        let message = delivery.message();
        let envelope = DeliveryEnvelope::from_agent(
            {
                #[cfg(test)]
                if self.force_queue_envelope {
                    xai_message_delivery_core::Operation::Queue
                } else {
                    crate::session::message_delivery::delivery_operation(delivery.operation())
                }
                #[cfg(not(test))]
                crate::session::message_delivery::delivery_operation(delivery.operation())
            },
            message.text.clone(),
            crate::session::message_delivery::agent_delivery_identity(message.message_id.clone()),
            crate::session::message_delivery::OwnedActiveDescendantGrant::new(
                self.active_message_target_session_id.clone(),
                delivery,
            ),
        );
        let parent_prompt_index = self
            .active_message_parent_prompt_index
            .load(std::sync::atomic::Ordering::Acquire);
        let parent_telemetry_ctx = xai_grok_telemetry::TelemetryCtx::new(
            self.active_message_parent_session_id.clone(),
            std::sync::Arc::new(tokio::sync::Mutex::new(parent_prompt_index)),
        );
        Box::pin(async move {
            message_delivery
                .send(envelope, receipt_sink, parent_telemetry_ctx)
                .await
        })
    }

    /// Post the child's text to the parent session's mailbox and resolve on the
    /// parent's take, not on its read.
    ///
    /// A deadline would be wrong here for the same reason it is wrong in
    /// [`Self::message`], but so would waiting for delivery: a parent that
    /// foreground-spawned this child is parked inside that `task` call and
    /// reaches no injection point until the child finishes, so a delivery ack
    /// would deadlock the pair. The parent answers the take immediately from
    /// its command loop, which runs alongside its turn.
    fn message_parent(&self, text: String) -> Self::ParentReportFuture {
        let Some(cmd_tx) = self.parent_cmd_tx.clone() else {
            return Box::pin(std::future::ready(ParentReportDelivery::Unreachable));
        };
        let subagent_id = self.subagent_id.clone();
        Box::pin(async move {
            let (ack, ack_rx) = oneshot::channel();
            if cmd_tx
                .send(SessionCommand::ChildReport {
                    subagent_id,
                    text,
                    ack,
                })
                .is_err()
            {
                return ParentReportDelivery::Unreachable;
            }
            match ack_rx.await {
                Ok(true) => ParentReportDelivery::Buffered,
                Ok(false) => ParentReportDelivery::NoTurnRunning,
                Err(_) => ParentReportDelivery::Unreachable,
            }
        })
    }

    fn cancel(&self) {
        cancel_shell_child_turn(&self.child_cmd_tx);
    }
}

const SESSION_THREAD_EXIT_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// How long to wait for an unpromoted child actor to exit after cancel and shutdown.
pub(crate) const UNPROMOTED_SESSION_THREAD_EXIT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(5);

/// Polls `SessionThread::is_finished` until the actor thread exits or `timeout` elapses.
/// A `timeout` of `Duration::ZERO` checks once and never sleeps.
pub(crate) async fn await_session_thread_exit(
    thread: &SessionThread,
    timeout: std::time::Duration,
) -> bool {
    if thread.is_finished() {
        return true;
    }
    if timeout.is_zero() {
        return false;
    }
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return thread.is_finished();
        }
        tokio::time::sleep(remaining.min(SESSION_THREAD_EXIT_POLL)).await;
        if thread.is_finished() {
            return true;
        }
    }
}

/// Whether an unpromoted child may drop its worktree and workspace binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnpromotedResourceFate {
    Release,
    Preserve,
}

impl UnpromotedResourceFate {
    pub(crate) fn from_thread_exit(exited: bool) -> Self {
        if exited {
            Self::Release
        } else {
            Self::Preserve
        }
    }

    pub(crate) fn should_release(self) -> bool {
        matches!(self, Self::Release)
    }
}

#[cfg(test)]
#[path = "child_runtime_tests.rs"]
mod tests;
