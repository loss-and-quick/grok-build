//! Sidecar supervisor for grok-build TypeScript plugins.
//!
//! A [`PluginHost`] owns one sidecar process per registered TS plugin, speaks the
//! versioned wire contract (`xai-grok-plugin-protocol`) with each over
//! newline-delimited JSON-RPC 2.0, and implements
//! [`xai_grok_hooks::invoker::PluginHookInvoker`] so the hook runner can drive a
//! `Plugin` handler without knowing anything about processes or the wire.
//!
//! # Module map
//!
//! - [`runtime`] — runtime discovery (bun → node >=22 → deno) and argv construction.
//! - [`sidecar`] — one child + its bidirectional JSON-RPC loop.
//! - [`capabilities`] — the plugin→core server (`log_emit`, `storage_*`, `config_get`).
//! - [`supervisor`] — [`PluginHost`]: restart-on-crash, disable-after-N, routing.
//!
//! # Lifecycle: lazy start
//!
//! Sidecars start **lazily**, on a plugin's first invocation, not at
//! registration. Session startup stays cheap: a plugin that never fires an event
//! it subscribed to never costs a process. The one-time price is that the first
//! matching event pays the spawn+handshake latency and cannot short-circuit on
//! subscriptions (they're only known post-handshake). [`PluginHost::start_all`]
//! is offered for callers that prefer eager warm-up.
//!
//! # Sandboxing
//!
//! Plugins inherit the parent's Landlock/Seatbelt confinement automatically (they
//! are children of the sandboxed process). The per-child seccomp network filter
//! for `network: false` plugins is *not* wired here — see the `TODO` in
//! [`runtime::build_command`]; that `unsafe pre_exec` belongs to the
//! shell-integration task which owns `xai-grok-sandbox`.

mod capabilities;
mod rpc;
pub mod runtime;
pub mod sidecar;
pub mod supervisor;

use std::path::PathBuf;

pub use runtime::RuntimeKind;
pub use supervisor::{PluginHost, SpawnHardener};

/// A plugin registered with the host: everything needed to spawn and hand-shake
/// its sidecar. Cloneable so the host can rebuild `initialize` params on restart.
#[derive(Debug, Clone)]
pub struct RegisteredPlugin {
    /// Unique plugin name; the routing key from `HookSpec::plugin`.
    pub name: String,
    /// Entry `.ts` file executed by the runtime.
    pub entry: PathBuf,
    /// Declared or auto runtime.
    pub runtime: RuntimeKind,
    /// Network access. `false` (default) is the seccomp-filtered case; see the
    /// `TODO` in [`runtime::build_command`].
    pub network: bool,
    /// Opaque config forwarded verbatim at `initialize` and via `config_get`.
    pub config: serde_json::Value,
    /// Workspace root; the sidecar's cwd and deno's read/write scope.
    pub workspace_root: PathBuf,
    /// Session id, forwarded at `initialize`.
    pub session_id: String,
    /// The session leader's Unix-socket path, when this process runs (or is)
    /// a leader. Advertised as `HostCapabilities::leader_socket` at
    /// `initialize` and exported as `GROK_LEADER_SOCKET` in the sidecar's env,
    /// so a plugin can attach to the session as one more headless ACP client.
    /// `None` outside leader mode.
    pub leader_socket: Option<String>,
}

/// A plugin's supervised runtime state, for UI listing via [`PluginHost::status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// Registered but no live sidecar (never started, or cleanly idle).
    Idle,
    /// A live sidecar handshaked and serving.
    Running,
    /// Crashed recently; waiting out the backoff before the next restart.
    BackingOff,
    /// Permanently disabled (protocol mismatch, or too many crashes).
    Disabled,
}

/// A snapshot of one plugin's status for the UI.
#[derive(Debug, Clone)]
pub struct PluginStatus {
    pub name: String,
    pub state: PluginState,
    /// Consecutive crashes since the last successful invocation.
    pub consecutive_crashes: u32,
    /// Event subscriptions from the last handshake (empty until first start).
    pub subscriptions: Vec<String>,
    /// Informational `plugin_version` from the handshake, if any.
    pub plugin_version: Option<String>,
    /// Most recent error surfaced to the UI, if any.
    pub last_error: Option<String>,
}
