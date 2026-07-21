//! Plugin hook handler runner.
//!
//! Executes a [`HandlerType::Plugin`](crate::config::HandlerType::Plugin) hook
//! by handing the serialized event envelope to the invoker registered on
//! [`RunContext::plugin_invoker`], then maps the response onto the same
//! [`HookRunnerResult`] vocabulary the command/http runners produce.
//!
//! Fail-open, like the other runners: no invoker registered, an invocation
//! error, or a timeout all yield [`HookRunnerResult::Failed`], which the
//! dispatcher records but does not let block the operation.

use std::time::{Duration, Instant};

use crate::config::HookSpec;
use crate::event::HookEventEnvelope;
use crate::invoker::{PluginHookRequest, PluginHookResponse};
use crate::result::{HookDecision, StopHookOutcome};

use super::{GateKind, HookRunnerResult, RunContext};

/// Run a single plugin hook via the injected invoker.
///
/// Spawns nothing: builds a [`PluginHookRequest`] carrying the serialized
/// envelope, awaits `invoker.invoke` under the spec's timeout, and interprets
/// the response per `mode`.
pub async fn run_plugin_hook(
    spec: &HookSpec,
    envelope: &HookEventEnvelope,
    ctx: &RunContext<'_>,
    mode: GateKind,
    payload_override: Option<&serde_json::Value>,
) -> (HookRunnerResult, Duration) {
    let start = Instant::now();

    let Some(ref plugin) = spec.plugin else {
        return (
            HookRunnerResult::Failed("plugin hook has no 'plugin' field".into()),
            start.elapsed(),
        );
    };

    // No invoker wired means the plugin host is absent: treat as a hook failure
    // (fail-open) rather than silently allowing, so the miss is visible in logs
    // and the UI scrollback.
    let Some(invoker) = ctx.plugin_invoker.as_ref() else {
        return (
            HookRunnerResult::Failed(format!("no plugin invoker registered (plugin '{plugin}')")),
            start.elapsed(),
        );
    };

    // Handler id defaults to the event name when the spec left it unset.
    let handler = spec
        .plugin_handler
        .clone()
        .unwrap_or_else(|| envelope.hook_event_name.to_string());

    // Forward the whole envelope verbatim, matching the command/http runners
    // (which send the same JSON on stdin / in the body); the host reshapes it.
    // A `payload_override` (Replace-gate chaining) supplants it so each hook sees
    // the prior hook's transformed payload rather than the original envelope.
    let payload = match payload_override {
        Some(v) => v.clone(),
        None => match serde_json::to_value(envelope) {
            Ok(v) => v,
            Err(e) => {
                return (
                    HookRunnerResult::Failed(format!("failed to serialize envelope: {e}")),
                    start.elapsed(),
                );
            }
        },
    };

    let req = PluginHookRequest {
        plugin: plugin.clone(),
        handler,
        event: envelope.hook_event_name.to_string(),
        payload,
        timeout_ms: spec.timeout_ms,
    };

    // Enforce the timeout here too: the invoker may honor `req.timeout_ms`, but
    // the runner is the single source of truth (parity with command/http).
    let timeout = Duration::from_millis(spec.timeout_ms);
    let result = tokio::time::timeout(timeout, invoker.invoke(req)).await;
    let elapsed = start.elapsed();

    match result {
        Err(_) => (
            HookRunnerResult::Failed(format!("timed out after {}ms", spec.timeout_ms)),
            elapsed,
        ),
        Ok(Err(e)) => (
            HookRunnerResult::Failed(format!("plugin invoke failed: {e}")),
            elapsed,
        ),
        Ok(Ok(response)) => {
            tracing::debug!(
                hook_name = %spec.name,
                plugin = %plugin,
                elapsed_ms = elapsed.as_millis() as u64,
                "plugin hook completed"
            );
            (response_to_result(response, &spec.name, mode), elapsed)
        }
    }
}

/// Map a [`PluginHookResponse`] onto a [`HookRunnerResult`] per gate `mode`,
/// mirroring how `command::run_command_hook` interprets exit codes / JSON.
///
/// Observe ignores decisions. A response whose variant doesn't match the gate
/// (e.g. a `Stop` reply to a `Tool` gate) is handled leniently — no signal,
/// which fails open — with a warning so the protocol bug surfaces.
fn response_to_result(
    response: PluginHookResponse,
    hook_name: &str,
    mode: GateKind,
) -> HookRunnerResult {
    match mode {
        GateKind::Observe => {
            if !matches!(response, PluginHookResponse::Observed) {
                tracing::warn!(
                    hook_name,
                    "plugin returned a decision for an observe gate; ignoring"
                );
            }
            HookRunnerResult::Success
        }
        GateKind::Tool => match response {
            PluginHookResponse::Decision {
                allow: false,
                reason,
            } => HookRunnerResult::Decision(HookDecision::Deny {
                reason: reason.unwrap_or_else(|| format!("denied by plugin hook '{hook_name}'")),
                hook_name: hook_name.to_string(),
            }),
            PluginHookResponse::Decision { allow: true, .. } => {
                HookRunnerResult::Decision(HookDecision::Allow)
            }
            // Observe/Stop/Replace replies to a Tool gate carry no allow/deny
            // signal: fail open (allow), warning on the clear gate mismatch.
            PluginHookResponse::Observed => HookRunnerResult::Decision(HookDecision::Allow),
            PluginHookResponse::Stop { .. } => {
                tracing::warn!(
                    hook_name,
                    "plugin returned a stop decision for a tool gate; allowing"
                );
                HookRunnerResult::Decision(HookDecision::Allow)
            }
            PluginHookResponse::Replace { .. } => {
                tracing::warn!(
                    hook_name,
                    "plugin returned a replace payload for a tool gate; allowing"
                );
                HookRunnerResult::Decision(HookDecision::Allow)
            }
        },
        GateKind::Stop => match response {
            PluginHookResponse::Stop {
                block,
                reason,
                continue_,
                additional_context,
            } => {
                let reason = reason.filter(|r| !r.trim().is_empty());
                let block_reason = block.then(|| {
                    reason
                        .clone()
                        .unwrap_or_else(|| format!("Blocked by plugin stop hook '{hook_name}'"))
                });
                // The wire contract carries a single `reason`; reuse it as the
                // force-stop reason (there is no separate `stopReason`).
                let force_stop =
                    (continue_ == Some(false)).then_some(crate::result::StopOverride {
                        reason: reason.clone(),
                    });
                HookRunnerResult::Stop(StopHookOutcome {
                    block_reason,
                    additional_context: additional_context.filter(|c| !c.trim().is_empty()),
                    force_stop,
                })
            }
            // Observe/Decision/Replace replies to a Stop gate carry no stop
            // signal: allow the stop (empty outcome), warning on the mismatch.
            PluginHookResponse::Observed => HookRunnerResult::Stop(StopHookOutcome::default()),
            PluginHookResponse::Decision { .. } => {
                tracing::warn!(
                    hook_name,
                    "plugin returned an allow/deny decision for a stop gate; allowing stop"
                );
                HookRunnerResult::Stop(StopHookOutcome::default())
            }
            PluginHookResponse::Replace { .. } => {
                tracing::warn!(
                    hook_name,
                    "plugin returned a replace payload for a stop gate; allowing stop"
                );
                HookRunnerResult::Stop(StopHookOutcome::default())
            }
        },
        GateKind::Replace => match response {
            // The transformed payload (Some) or an explicit passthrough (None).
            PluginHookResponse::Replace { payload } => HookRunnerResult::Replace(payload),
            // A non-replace reply to a Replace gate carries no substitution:
            // pass the current payload through, warning on the clear mismatch.
            PluginHookResponse::Observed => HookRunnerResult::Replace(None),
            PluginHookResponse::Decision { .. } | PluginHookResponse::Stop { .. } => {
                tracing::warn!(
                    hook_name,
                    "plugin returned a decision/stop for a replace gate; passing through"
                );
                HookRunnerResult::Replace(None)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invoker::{PluginHookFuture, PluginHookInvoker, PluginInvokeError};
    use crate::result::StopOverride;
    use std::sync::Arc;

    /// A mock invoker that returns a canned response (or error), or sleeps past
    /// the deadline to exercise the timeout path.
    struct MockInvoker {
        outcome: MockOutcome,
    }

    enum MockOutcome {
        Respond(PluginHookResponse),
        Fail(String),
        Sleep(Duration),
    }

    impl PluginHookInvoker for MockInvoker {
        fn invoke<'a>(&'a self, _req: PluginHookRequest) -> PluginHookFuture<'a> {
            Box::pin(async move {
                match &self.outcome {
                    MockOutcome::Respond(r) => Ok(r.clone()),
                    MockOutcome::Fail(msg) => Err(PluginInvokeError::new(msg.clone())),
                    MockOutcome::Sleep(dur) => {
                        tokio::time::sleep(*dur).await;
                        Ok(PluginHookResponse::Observed)
                    }
                }
            })
        }
    }

    fn plugin_spec(handler: Option<&str>) -> HookSpec {
        HookSpec {
            name: "test-plugin-hook".into(),
            event: crate::event::HookEventName::PreToolUse,
            handler_type: crate::config::HandlerType::Plugin,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            command: None,
            command_raw: None,
            url: None,
            url_raw: None,
            plugin: Some("demo".into()),
            plugin_handler: handler.map(str::to_string),
            timeout_ms: 5000,
            source_dir: std::path::PathBuf::from("/tmp"),
            extra_env: std::collections::HashMap::new(),
        }
    }

    fn envelope() -> HookEventEnvelope {
        use crate::event::HookPayload;
        HookEventEnvelope {
            hook_event_name: crate::event::HookEventName::PreToolUse,
            session_id: "s".into(),
            cwd: "/tmp".into(),
            workspace_root: "/tmp".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: None,
            payload: HookPayload::PreToolUse {
                tool_name: "run_terminal_cmd".into(),
                tool_use_id: "tu-1".into(),
                tool_input: serde_json::json!({"command": "ls"}),
                tool_input_truncated: false,
                subagent_type: None,
            },
        }
    }

    fn ctx_with(invoker: Option<Arc<dyn PluginHookInvoker>>) -> RunContext<'static> {
        RunContext {
            session_id: "s",
            workspace_root: "/tmp",
            plugin_invoker: invoker,
        }
    }

    #[tokio::test]
    async fn deny_on_pre_tool_use() {
        let invoker = Arc::new(MockInvoker {
            outcome: MockOutcome::Respond(PluginHookResponse::Decision {
                allow: false,
                reason: Some("blocked by policy".into()),
            }),
        });
        let (result, _) = run_plugin_hook(
            &plugin_spec(None),
            &envelope(),
            &ctx_with(Some(invoker)),
            GateKind::Tool,
            None,
        )
        .await;
        match result {
            HookRunnerResult::Decision(HookDecision::Deny { reason, hook_name }) => {
                assert_eq!(reason, "blocked by policy");
                assert_eq!(hook_name, "test-plugin-hook");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deny_without_reason_uses_default() {
        let invoker = Arc::new(MockInvoker {
            outcome: MockOutcome::Respond(PluginHookResponse::Decision {
                allow: false,
                reason: None,
            }),
        });
        let (result, _) = run_plugin_hook(
            &plugin_spec(None),
            &envelope(),
            &ctx_with(Some(invoker)),
            GateKind::Tool,
            None,
        )
        .await;
        match result {
            HookRunnerResult::Decision(HookDecision::Deny { reason, .. }) => {
                assert!(reason.contains("test-plugin-hook"), "got {reason}");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn allow_on_pre_tool_use() {
        let invoker = Arc::new(MockInvoker {
            outcome: MockOutcome::Respond(PluginHookResponse::Decision {
                allow: true,
                reason: None,
            }),
        });
        let (result, _) = run_plugin_hook(
            &plugin_spec(None),
            &envelope(),
            &ctx_with(Some(invoker)),
            GateKind::Tool,
            None,
        )
        .await;
        assert!(matches!(
            result,
            HookRunnerResult::Decision(HookDecision::Allow)
        ));
    }

    #[tokio::test]
    async fn stop_block_with_additional_context() {
        let invoker = Arc::new(MockInvoker {
            outcome: MockOutcome::Respond(PluginHookResponse::Stop {
                block: true,
                reason: Some("tests are failing".into()),
                continue_: None,
                additional_context: Some("run the suite first".into()),
            }),
        });
        let (result, _) = run_plugin_hook(
            &plugin_spec(None),
            &envelope(),
            &ctx_with(Some(invoker)),
            GateKind::Stop,
            None,
        )
        .await;
        match result {
            HookRunnerResult::Stop(outcome) => {
                assert_eq!(outcome.block_reason.as_deref(), Some("tests are failing"));
                assert_eq!(
                    outcome.additional_context.as_deref(),
                    Some("run the suite first")
                );
                assert!(outcome.force_stop.is_none());
            }
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stop_force_stop_reuses_reason() {
        let invoker = Arc::new(MockInvoker {
            outcome: MockOutcome::Respond(PluginHookResponse::Stop {
                block: false,
                reason: Some("budget exhausted".into()),
                continue_: Some(false),
                additional_context: None,
            }),
        });
        let (result, _) = run_plugin_hook(
            &plugin_spec(None),
            &envelope(),
            &ctx_with(Some(invoker)),
            GateKind::Stop,
            None,
        )
        .await;
        match result {
            HookRunnerResult::Stop(outcome) => {
                assert_eq!(
                    outcome.force_stop,
                    Some(StopOverride {
                        reason: Some("budget exhausted".into()),
                    })
                );
                assert!(outcome.block_reason.is_none());
            }
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn observe_returns_success() {
        let invoker = Arc::new(MockInvoker {
            outcome: MockOutcome::Respond(PluginHookResponse::Observed),
        });
        let (result, _) = run_plugin_hook(
            &plugin_spec(None),
            &envelope(),
            &ctx_with(Some(invoker)),
            GateKind::Observe,
            None,
        )
        .await;
        assert!(matches!(result, HookRunnerResult::Success));
    }

    #[tokio::test]
    async fn invoker_timeout_fails_open() {
        let invoker = Arc::new(MockInvoker {
            outcome: MockOutcome::Sleep(Duration::from_secs(5)),
        });
        let mut spec = plugin_spec(None);
        spec.timeout_ms = 100;
        let (result, _) = run_plugin_hook(
            &spec,
            &envelope(),
            &ctx_with(Some(invoker)),
            GateKind::Tool,
            None,
        )
        .await;
        assert!(
            matches!(&result, HookRunnerResult::Failed(msg) if msg.contains("timed out")),
            "expected a timeout failure, got {result:?}"
        );
    }

    #[tokio::test]
    async fn invoker_error_fails_open() {
        let invoker = Arc::new(MockInvoker {
            outcome: MockOutcome::Fail("sidecar crashed".into()),
        });
        let (result, _) = run_plugin_hook(
            &plugin_spec(None),
            &envelope(),
            &ctx_with(Some(invoker)),
            GateKind::Tool,
            None,
        )
        .await;
        assert!(
            matches!(&result, HookRunnerResult::Failed(msg) if msg.contains("sidecar crashed")),
            "expected an invoke failure, got {result:?}"
        );
    }

    #[tokio::test]
    async fn absent_invoker_fails_open() {
        let (result, _) = run_plugin_hook(
            &plugin_spec(None),
            &envelope(),
            &ctx_with(None),
            GateKind::Tool,
            None,
        )
        .await;
        assert!(
            matches!(&result, HookRunnerResult::Failed(msg) if msg.contains("no plugin invoker")),
            "expected a no-invoker failure, got {result:?}"
        );
    }
}
