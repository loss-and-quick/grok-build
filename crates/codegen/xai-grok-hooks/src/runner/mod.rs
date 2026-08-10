pub mod command;
pub mod http;
pub mod plugin;

use std::sync::Arc;
use std::time::Duration;

use crate::config::HookSpec;
use crate::event::HookEventEnvelope;
use crate::invoker::PluginHookInvoker;
use serde::Deserialize;

use crate::result::{HookDecision, HttpInfo, StopHookOutcome};

/// How a hook's output is interpreted, per the event's [`GateKind`]: `Observe`
/// ignores output, `Tool` parses the allow/deny vocabulary, `Stop` the stop
/// vocabulary.
pub use crate::event::GateKind;

pub struct RunContext<'a> {
    pub session_id: &'a str,
    pub workspace_root: &'a str,
    /// Injected bridge for [`HandlerType::Plugin`](crate::config::HandlerType::Plugin)
    /// hooks. `None` (the default) means no plugin host is wired: plugin hooks
    /// then fail open. Command/http hooks never consult it.
    pub plugin_invoker: Option<Arc<dyn PluginHookInvoker>>,
    pub process_scope: Option<xai_grok_tools::util::ProcessScope>,
}

/// Result of running a single hook (any handler type).
#[derive(Debug)]
pub enum HookRunnerResult {
    Decision(HookDecision),
    Stop(StopHookOutcome),
    /// Replace gate: `Some` is the transformed payload, `None` a passthrough.
    Replace(Option<serde_json::Value>),
    /// Observe gate: the hook ran and attached model-facing text. Separate from
    /// [`Self::Success`] (a hook that ran and said nothing) so the observe
    /// dispatcher can aggregate the text without every silent hook paying for
    /// an `Option`. Only [`crate::dispatcher::dispatch_non_blocking`] reads it;
    /// every other gate treats it exactly as [`Self::Success`].
    Context(String),
    Success,
    /// Nothing ran: the handler provably does not exist, so there is no
    /// execution to report. Only the plugin runner produces it, for a plugin
    /// with no handler subscribed to the fired event
    /// ([`crate::invoker::PluginHookResponse::NotSubscribed`]).
    ///
    /// Carries no signal, so every dispatcher decides exactly as it would for a
    /// no-signal success, but records
    /// [`HookRunResult::Skipped`](crate::result::HookRunResult::Skipped) so the
    /// UI does not render a run that never happened. A hook that ran and stayed
    /// silent is [`Self::Success`]; one that could not run is [`Self::Failed`] —
    /// both stay visible.
    Skipped,
    /// Failed: the caller fails open.
    Failed(String),
}

/// JSON from `PreToolUse` gate hooks:
/// `{"decision": "allow" | "deny", "reason": "…"}`.
#[derive(Debug, Deserialize)]
pub(crate) struct GateHookJson {
    pub decision: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Interpret a [`GateHookJson`] as a [`HookDecision`]. An unknown decision value
/// is an error so typos surface instead of failing open.
pub(crate) fn gate_json_to_decision(
    json: GateHookJson,
    hook_name: &str,
) -> Result<HookDecision, String> {
    match json.decision.as_str() {
        "deny" => Ok(HookDecision::Deny {
            reason: json
                .reason
                .unwrap_or_else(|| format!("denied by hook '{hook_name}'")),
            hook_name: hook_name.to_string(),
        }),
        "allow" => Ok(HookDecision::Allow),
        other => Err(format!(
            "unknown decision value '{other}' from hook '{hook_name}'"
        )),
    }
}

/// JSON from `Stop`/`SubagentStop` gate hooks. All fields optional; one output
/// can combine several signals.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct StopHookJson {
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default, rename = "continue")]
    pub continue_: Option<bool>,
    #[serde(default, rename = "stopReason")]
    pub stop_reason: Option<String>,
    #[serde(default, rename = "hookSpecificOutput")]
    pub hook_specific_output: Option<HookSpecificOutputJson>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct HookSpecificOutputJson {
    #[serde(default, rename = "additionalContext")]
    pub additional_context: Option<String>,
}

/// JSON from an `Observe` gate hook. An observe hook has no decision to return,
/// so only `hookSpecificOutput.additionalContext` carries anything — the same
/// key the `Stop` gate uses, so one hook script can serve both.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ObserveHookJson {
    #[serde(default, rename = "hookSpecificOutput")]
    pub hook_specific_output: Option<HookSpecificOutputJson>,
}

/// Interpret an observe hook's stdout: JSON carrying a non-blank
/// `hookSpecificOutput.additionalContext` becomes
/// [`HookRunnerResult::Context`]; anything else (no output, plain text,
/// unparseable JSON) is a plain [`HookRunnerResult::Success`]. Never fails —
/// an observe hook has no decision that a malformed reply could corrupt.
pub(crate) fn observe_result_from_stdout(stdout: &str) -> HookRunnerResult {
    let trimmed = stdout.trim();
    if !trimmed.starts_with('{') {
        return HookRunnerResult::Success;
    }
    match serde_json::from_str::<ObserveHookJson>(trimmed) {
        Ok(json) => json
            .hook_specific_output
            .and_then(|output| output.additional_context)
            .filter(|context| !context.trim().is_empty())
            .map_or(HookRunnerResult::Success, HookRunnerResult::Context),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "observe hook stdout looks like JSON but failed to parse; ignoring"
            );
            HookRunnerResult::Success
        }
    }
}

/// Interpret a [`StopHookJson`] as a [`StopHookOutcome`].
///
/// `decision: "block"` requires a reason (a missing one falls back to a generic
/// message). `decision: "approve"` is a no-op; any other value is an error so
/// typos surface.
pub(crate) fn stop_json_to_outcome(
    json: StopHookJson,
    hook_name: &str,
) -> Result<StopHookOutcome, String> {
    let block_reason = match json.decision.as_deref() {
        Some("block") => Some(
            json.reason
                .filter(|reason| !reason.trim().is_empty())
                .unwrap_or_else(|| format!("Blocked by stop hook '{hook_name}'")),
        ),
        Some("approve") | None => None,
        Some(other) => {
            return Err(format!(
                "unknown decision value '{other}' from hook '{hook_name}'"
            ));
        }
    };
    Ok(StopHookOutcome {
        block_reason,
        additional_context: json
            .hook_specific_output
            .and_then(|output| output.additional_context)
            .filter(|context| !context.trim().is_empty()),
        force_stop: (json.continue_ == Some(false)).then_some(crate::result::StopOverride {
            reason: json.stop_reason,
        }),
    })
}

/// Each runner returns the result, wall-clock duration, and optional HTTP
/// metadata for enriched scrollback logging.
pub type HookRunOutput = (HookRunnerResult, Duration, Option<HttpInfo>);

pub async fn run_hook(
    spec: &HookSpec,
    envelope: &HookEventEnvelope,
    ctx: &RunContext<'_>,
    mode: GateKind,
) -> HookRunOutput {
    run_hook_with_payload(spec, envelope, ctx, mode, None).await
}

/// Like [`run_hook`], but with an optional `payload_override` that supplants the
/// serialized envelope as the plugin request payload. Only the plugin runner
/// consults it (command/http build their input from the envelope); it exists so
/// [`crate::dispatcher::dispatch_replace`] can chain one Replace hook's output
/// into the next.
pub async fn run_hook_with_payload(
    spec: &HookSpec,
    envelope: &HookEventEnvelope,
    ctx: &RunContext<'_>,
    mode: GateKind,
    payload_override: Option<&serde_json::Value>,
) -> HookRunOutput {
    match spec.handler_type {
        crate::config::HandlerType::Command => {
            let (result, elapsed) = command::run_command_hook(spec, envelope, ctx, mode).await;
            (result, elapsed, None)
        }
        crate::config::HandlerType::Http => http::run_http_hook(spec, envelope, ctx, mode).await,
        crate::config::HandlerType::Plugin => {
            let (result, elapsed) =
                plugin::run_plugin_hook(spec, envelope, ctx, mode, payload_override).await;
            (result, elapsed, None)
        }
    }
}
