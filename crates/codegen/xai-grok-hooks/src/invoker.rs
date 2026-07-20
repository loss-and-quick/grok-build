//! Plugin hook invocation seam.
//!
//! A `Plugin` handler ([`crate::config::HandlerType::Plugin`]) runs its hook by
//! handing a [`PluginHookRequest`] to an injected [`PluginHookInvoker`] rather
//! than spawning a process or POSTing a URL. The host wires a concrete invoker
//! (a TS plugin sidecar bridge) into [`crate::runner::RunContext::plugin_invoker`];
//! this crate stays dependency-light and never sees the wire protocol.
//!
//! Requests carry the fully serialized event envelope as `payload` (parity with
//! the command/http runners, which send the same JSON on stdin / in the body);
//! the host translates it to its own protocol DTOs. Responses map back onto the
//! crate's existing decision vocabulary in [`crate::runner::plugin`].

use std::future::Future;
use std::pin::Pin;

/// A plugin hook invocation, translated by the host onto its wire protocol.
#[derive(Debug, Clone)]
pub struct PluginHookRequest {
    /// The plugin that owns the handler (from `HookSpec::plugin`).
    pub plugin: String,
    /// The handler id within the plugin. Defaults to the event name when the
    /// spec left `handler` unset (see [`crate::runner::plugin`]).
    pub handler: String,
    /// The fired event name (snake_case, as [`crate::event::HookEventName`] displays).
    pub event: String,
    /// The serialized [`crate::event::HookEventEnvelope`], forwarded verbatim.
    pub payload: serde_json::Value,
    /// Deadline mirrored from `HookSpec::timeout_ms`. The runner also enforces
    /// this with `tokio::time::timeout`, so the invoker may treat it as advisory.
    pub timeout_ms: u64,
}

/// A plugin hook response, normalized onto the crate's decision vocabulary.
///
/// The variant a plugin returns is expected to match the fired gate
/// ([`crate::event::GateKind`]); a mismatch is handled leniently (fail-open) by
/// [`crate::runner::plugin`].
#[derive(Debug, Clone)]
pub enum PluginHookResponse {
    /// Observe gate: the hook ran, no decision.
    Observed,
    /// Tool gate: allow or deny the tool call, with an optional deny reason.
    Decision { allow: bool, reason: Option<String> },
    /// Stop gate: any combination of a block, a forced stop, and extra context.
    Stop {
        /// Block the stop and feed `reason` back to the model.
        block: bool,
        /// Feedback for a block and/or a forced stop.
        reason: Option<String>,
        /// `Some(false)` forces the agent to stop (overrides blocks).
        continue_: Option<bool>,
        /// Injected into the next turn's context.
        additional_context: Option<String>,
    },
}

/// A plugin invocation failure (transport error, plugin crash, protocol error).
///
/// The runner treats it as a hook failure and fails open, exactly like a
/// command that exits non-zero or an HTTP hook that errors.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PluginInvokeError {
    pub message: String,
}

impl PluginInvokeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The future returned by [`PluginHookInvoker::invoke`].
///
/// Aliased so the fixed boxed-future signature reads cleanly and doesn't trip
/// `clippy::type_complexity`.
pub type PluginHookFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PluginHookResponse, PluginInvokeError>> + Send + 'a>>;

/// Injected seam the `Plugin` runner calls to execute a hook.
///
/// Implemented by the host (a TS plugin sidecar bridge). Kept object-safe with a
/// boxed future so it can live behind an `Arc<dyn PluginHookInvoker>` in
/// [`crate::runner::RunContext`].
pub trait PluginHookInvoker: Send + Sync {
    fn invoke<'a>(&'a self, req: PluginHookRequest) -> PluginHookFuture<'a>;
}
