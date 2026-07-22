//! [`PluginHost`]: owns the sidecars, supervises restarts, and routes hook
//! invocations onto the wire.
//!
//! # Restart policy
//!
//! Each plugin tracks `consecutive_crashes`. A crash (a dead transport observed
//! on invoke, or a failed start) increments it and schedules the next restart
//! after an exponential backoff. On [`MAX_CONSECUTIVE_CRASHES`] the plugin is
//! **disabled** and no longer started; a protocol-version mismatch disables it
//! immediately (retrying can't help). A successful invocation resets the counter.
//! Everything fails **open**: an unavailable plugin yields a
//! [`PluginInvokeError`], which the hook runner treats exactly like a command
//! hook that errored — the underlying operation is never blocked.
//!
//! # Request correlation
//!
//! Correlation lives in [`crate::sidecar`]: an mpsc writer task plus an
//! id-keyed pending map (the `xai-acp-lib` gateway idiom, adapted to plain
//! `tokio::spawn`). The supervisor holds the per-plugin state behind a
//! `tokio::sync::Mutex` that it never keeps locked across the invoke RPC — the
//! sidecar `Arc` is cloned out and the guard dropped first, so concurrent
//! invokes to one plugin don't serialize on the network round-trip.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Mutex;
use xai_grok_hooks::event::{GateKind, HookEventName};
use xai_grok_hooks::invoker::{
    PluginHookFuture, PluginHookInvoker, PluginHookRequest, PluginHookResponse, PluginInvokeError,
};
use xai_grok_plugin_protocol::{
    DecisionDto, GateKindDto, HookInvokeResult, ToolCallContextDto, ToolInvokeResult,
};

use crate::capabilities::{PluginCapabilities, storage_path};
use crate::sidecar::{PluginSidecar, StartError, initialize_params};
use crate::{PluginState, PluginStatus, RegisteredPlugin};

/// Crashes in a row before a plugin is disabled.
const MAX_CONSECUTIVE_CRASHES: u32 = 3;
/// Base backoff; doubled per consecutive crash, capped at [`BACKOFF_CAP`].
const DEFAULT_BACKOFF_BASE: Duration = Duration::from_millis(500);
/// Upper bound on a single backoff interval.
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Deadline for the `initialize` handshake.
const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Fallback invoke timeout when a request carries `timeout_ms == 0`.
const DEFAULT_INVOKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Fallback `tool_invoke` timeout when a call carries `timeout_ms == 0`.
/// Deliberately longer than the hook default: tool handlers legitimately do
/// real work (spawn subagents, batch storage) where a hook must stay snappy.
const DEFAULT_TOOL_INVOKE_TIMEOUT: Duration = Duration::from_secs(120);

/// Produces the spawn `Command` for a plugin. Overridable in tests to inject a
/// fake sidecar; production uses [`crate::runtime::build_command`].
type CommandFactory =
    Box<dyn Fn(&RegisteredPlugin) -> Result<tokio::process::Command, String> + Send + Sync>;

/// A last-mile hardening hook applied to the spawn [`tokio::process::Command`]
/// just before the sidecar is launched, receiving the plugin's `network` flag.
///
/// The host is deliberately ignorant of what hardening means: the shell injects
/// a closure (via [`PluginHost::set_spawn_hardener`]) that installs the per-child
/// seccomp network filter (`xai-grok-sandbox`) when `network == false`. Keeping
/// this an injected callback is what lets the host crate stay free of any
/// dependency on `xai-grok-sandbox` while still confining `network: false`
/// sidecars. See the `TODO(sandbox-wiring)` in [`crate::runtime::build_command`].
pub type SpawnHardener =
    Arc<dyn Fn(&mut tokio::process::Command, /* network: */ bool) + Send + Sync>;

/// Owns and supervises the plugin sidecars. Share behind an `Arc`; it coerces to
/// `Arc<dyn PluginHookInvoker>` for the hook runner.
pub struct PluginHost {
    data_dir: PathBuf,
    command_factory: CommandFactory,
    /// Optional last-mile hardener applied to each spawn `Command`; `None` means
    /// no extra confinement (the default, and what keeps `new_for_test` and the
    /// crate's own tests independent of any sandbox).
    spawn_hardener: Option<SpawnHardener>,
    backoff_base: Duration,
    handshake_timeout: Duration,
    next_invocation: AtomicU64,
    plugins: std::sync::Mutex<HashMap<String, Arc<PluginEntry>>>,
    /// The injected subagent-orchestration seam, fanned out to every plugin's
    /// capability server (current and future registrations). `None` keeps the
    /// `agent_*` methods answering `method_not_found`.
    agent_orchestrator: std::sync::Mutex<Option<Arc<dyn crate::orchestration::AgentOrchestrator>>>,
    /// The injected UI-panel seam, fanned out to every plugin's capability
    /// server (current and future registrations). `None` keeps
    /// `ui_publish_panel`/`ui_close_panel` answering `method_not_found`.
    panel_sink: std::sync::Mutex<Option<Arc<dyn crate::orchestration::PanelSink>>>,
}

/// One registered plugin: its spec, capability server (shared across restarts),
/// and supervised runtime state.
struct PluginEntry {
    spec: RegisteredPlugin,
    caps: Arc<PluginCapabilities>,
    state: Mutex<SupervisorState>,
}

#[derive(Default)]
struct SupervisorState {
    sidecar: Option<Arc<PluginSidecar>>,
    consecutive_crashes: u32,
    next_retry_at: Option<tokio::time::Instant>,
    disabled: bool,
    last_error: Option<String>,
    /// Canonicalized subscription set, used to match fired events. Aliases
    /// (`subagent_end`) collapse to their canonical form (`subagent_stop`) so a
    /// plugin subscribed under either spelling receives the event.
    subscriptions: HashSet<String>,
    /// The subscription names exactly as the plugin declared them, for UI
    /// display (before alias canonicalization).
    subscription_labels: Vec<String>,
    plugin_version: Option<String>,
}

impl PluginHost {
    /// Create a host. `data_dir` is where per-plugin storage files live.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            command_factory: Box::new(|spec| {
                crate::runtime::build_command(spec).map_err(|e| e.to_string())
            }),
            spawn_hardener: None,
            backoff_base: DEFAULT_BACKOFF_BASE,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            next_invocation: AtomicU64::new(1),
            plugins: std::sync::Mutex::new(HashMap::new()),
            agent_orchestrator: std::sync::Mutex::new(None),
            panel_sink: std::sync::Mutex::new(None),
        }
    }

    /// Install a [`SpawnHardener`] applied to every sidecar's spawn `Command`
    /// just before launch. Call before sharing the host behind an `Arc` (takes
    /// `&mut self`). The default is no hardener; production wires one that
    /// installs the seccomp network filter for `network: false` plugins.
    pub fn set_spawn_hardener(&mut self, hardener: SpawnHardener) {
        self.spawn_hardener = Some(hardener);
    }

    /// Test constructor: inject a command factory (e.g. a fake sidecar bin) and a
    /// tiny backoff base so restart/backoff tests don't sleep for real seconds.
    #[doc(hidden)]
    pub fn new_for_test(
        data_dir: PathBuf,
        command_factory: CommandFactory,
        backoff_base: Duration,
    ) -> Self {
        Self {
            data_dir,
            command_factory,
            spawn_hardener: None,
            backoff_base,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            next_invocation: AtomicU64::new(1),
            plugins: std::sync::Mutex::new(HashMap::new()),
            agent_orchestrator: std::sync::Mutex::new(None),
            panel_sink: std::sync::Mutex::new(None),
        }
    }

    /// Install the subagent-orchestration seam, fanning it out to every
    /// registered plugin's capability server; plugins registered later inherit
    /// it too. Takes `&self`: the shell wires it after the host is built (the
    /// coordinator channel may not exist yet at construction).
    pub fn set_agent_orchestrator(
        &self,
        orchestrator: Arc<dyn crate::orchestration::AgentOrchestrator>,
    ) {
        *self
            .agent_orchestrator
            .lock()
            .expect("orchestrator slot poisoned") = Some(Arc::clone(&orchestrator));
        let plugins = self.plugins.lock().expect("registry poisoned");
        for entry in plugins.values() {
            entry.caps.set_orchestrator(Arc::clone(&orchestrator));
        }
    }

    /// Install the UI-panel seam, fanning it out to every registered plugin's
    /// capability server; plugins registered later inherit it too. Takes
    /// `&self`: the shell wires it after the host is built (the pager channel
    /// may not exist yet at construction).
    pub fn set_panel_sink(&self, sink: Arc<dyn crate::orchestration::PanelSink>) {
        *self.panel_sink.lock().expect("panel sink slot poisoned") = Some(Arc::clone(&sink));
        let plugins = self.plugins.lock().expect("registry poisoned");
        for entry in plugins.values() {
            entry.caps.set_panel_sink(Arc::clone(&sink));
        }
    }

    /// Register a plugin. Idempotent per name (a re-register replaces the entry,
    /// dropping any prior sidecar on next access).
    pub fn register_plugin(&self, spec: RegisteredPlugin) {
        let caps = Arc::new(PluginCapabilities::new(
            spec.name.clone(),
            spec.config.clone(),
            storage_path(&self.data_dir, &spec.name),
        ));
        if let Some(orchestrator) = self
            .agent_orchestrator
            .lock()
            .expect("orchestrator slot poisoned")
            .as_ref()
        {
            caps.set_orchestrator(Arc::clone(orchestrator));
        }
        if let Some(sink) = self
            .panel_sink
            .lock()
            .expect("panel sink slot poisoned")
            .as_ref()
        {
            caps.set_panel_sink(Arc::clone(sink));
        }
        let entry = Arc::new(PluginEntry {
            caps,
            state: Mutex::new(SupervisorState::default()),
            spec,
        });
        let mut plugins = self.plugins.lock().expect("registry poisoned");
        if plugins
            .insert(entry.spec.name.clone(), Arc::clone(&entry))
            .is_some()
        {
            tracing::debug!(plugin = %entry.spec.name, "re-registered plugin");
        }
    }

    /// Eagerly start (spawn + handshake) every registered plugin. Optional —
    /// lazy start on first invoke is the default. Errors are logged, not
    /// propagated: a plugin that fails to warm up is simply left for its next
    /// invocation to retry.
    pub async fn start_all(&self) {
        let entries: Vec<Arc<PluginEntry>> = {
            let plugins = self.plugins.lock().expect("registry poisoned");
            plugins.values().cloned().collect()
        };
        for entry in entries {
            let mut state = entry.state.lock().await;
            if let Err(e) = self.ensure_alive(&entry, &mut state).await {
                tracing::info!(plugin = %entry.spec.name, "eager start deferred: {e}");
            }
        }
    }

    /// Snapshot every plugin's status for UI listing.
    pub async fn status(&self) -> Vec<PluginStatus> {
        let entries: Vec<Arc<PluginEntry>> = {
            let plugins = self.plugins.lock().expect("registry poisoned");
            plugins.values().cloned().collect()
        };
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let state = entry.state.lock().await;
            out.push(PluginStatus {
                name: entry.spec.name.clone(),
                state: state.derive_state(),
                consecutive_crashes: state.consecutive_crashes,
                subscriptions: {
                    let mut s = state.subscription_labels.clone();
                    s.sort();
                    s
                },
                plugin_version: state.plugin_version.clone(),
                last_error: state.last_error.clone(),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Gracefully shut every sidecar down (`shutdown` notification, 2s grace,
    /// then SIGKILL). Idempotent.
    pub async fn dispose(&self) {
        let entries: Vec<Arc<PluginEntry>> = {
            let plugins = self.plugins.lock().expect("registry poisoned");
            plugins.values().cloned().collect()
        };
        for entry in entries {
            let sidecar = {
                let mut state = entry.state.lock().await;
                state.sidecar.take()
            };
            if let Some(sidecar) = sidecar {
                sidecar.dispose("host_dispose").await;
            }
        }
    }

    /// Ensure a live sidecar, starting or refusing per the restart policy.
    ///
    /// Called with the plugin's state guard held. Returns the live sidecar, or a
    /// fail-open error (disabled / backing off / start failed).
    async fn ensure_alive(
        &self,
        entry: &Arc<PluginEntry>,
        state: &mut SupervisorState,
    ) -> Result<Arc<PluginSidecar>, PluginInvokeError> {
        let name = &entry.spec.name;
        if state.disabled {
            return Err(PluginInvokeError::new(format!(
                "plugin '{name}' is disabled"
            )));
        }

        if let Some(sidecar) = &state.sidecar {
            if sidecar.is_alive() {
                return Ok(Arc::clone(sidecar));
            }
            // Died since we last looked (idle crash). Count it and back off.
            self.note_crash(state, name, "sidecar exited");
            return Err(self.unavailable_error(state, name));
        }

        if let Some(retry) = state.next_retry_at
            && tokio::time::Instant::now() < retry
        {
            return Err(PluginInvokeError::new(format!(
                "plugin '{name}' is backing off after {} crash(es)",
                state.consecutive_crashes
            )));
        }

        match self.start_sidecar(entry, state).await {
            Ok(sidecar) => Ok(sidecar),
            Err(StartError::VersionMismatch { plugin, host }) => {
                state.sidecar = None;
                state.disabled = true;
                state.last_error = Some(format!(
                    "protocol version mismatch: plugin={plugin} host={host}"
                ));
                tracing::warn!(plugin = %name, plugin_version = plugin, host_version = host,
                    "disabling plugin: protocol version mismatch");
                Err(PluginInvokeError::new(format!(
                    "plugin '{name}' disabled: protocol version mismatch (plugin={plugin} host={host})"
                )))
            }
            Err(e) => {
                self.note_crash(state, name, &e.to_string());
                Err(self.unavailable_error(state, name))
            }
        }
    }

    /// Spawn + handshake a fresh sidecar, recording its subscriptions on success.
    /// Leaves `consecutive_crashes` untouched (it resets only on a successful
    /// invoke), so a plugin that handshakes then instantly crashes still trips
    /// the disable threshold.
    async fn start_sidecar(
        &self,
        entry: &Arc<PluginEntry>,
        state: &mut SupervisorState,
    ) -> Result<Arc<PluginSidecar>, StartError> {
        let spec = &entry.spec;
        let mut cmd = (self.command_factory)(spec).map_err(StartError::Spawn)?;
        // Apply the injected last-mile hardener (production: seccomp network
        // filter for `network: false` plugins) after the factory builds the
        // argv/cwd but before we launch the child.
        if let Some(hardener) = &self.spawn_hardener {
            hardener(&mut cmd, spec.network);
        }
        let sidecar = PluginSidecar::spawn(cmd, spec.name.clone(), Arc::clone(&entry.caps))
            .map_err(|e| StartError::Spawn(e.to_string()))?;

        let params = initialize_params(
            spec.name.clone(),
            spec.config.clone(),
            spec.workspace_root.clone(),
            spec.session_id.clone(),
            true,
            spec.leader_socket.clone(),
        );
        let init = sidecar.handshake(params, self.handshake_timeout).await?;

        state.sidecar = Some(Arc::clone(&sidecar));
        state.subscriptions = init
            .subscriptions
            .iter()
            .map(|s| canonical_event(s))
            .collect();
        state.subscription_labels = init.subscriptions;
        state.plugin_version = init.plugin_version;
        state.next_retry_at = None;
        state.last_error = None;

        // Tool drift warnings: the manifest (what the model sees) and the
        // code-registered handlers should agree. Neither direction is fatal —
        // a missing handler surfaces as an error tool result on invoke, and an
        // undeclared handler is simply unreachable from the model.
        let handler_names: HashSet<&str> = init.tools.iter().map(|t| t.name.as_str()).collect();
        for declared in &spec.declared_tools {
            if !handler_names.contains(declared.as_str()) {
                tracing::warn!(plugin = %spec.name, tool = %declared,
                    "manifest declares a tool the sidecar registered no handler for");
            }
        }
        for handler in &init.tools {
            if !spec.declared_tools.iter().any(|d| d == &handler.name) {
                tracing::warn!(plugin = %spec.name, tool = %handler.name,
                    "sidecar registered a tool handler the manifest does not declare; \
                     it is not visible to the model");
            }
        }

        tracing::debug!(plugin = %spec.name, subscriptions = ?state.subscriptions, "sidecar handshaked");
        Ok(sidecar)
    }

    /// Record a crash: drop the sidecar, bump the counter, schedule a backoff,
    /// and disable past the threshold.
    fn note_crash(&self, state: &mut SupervisorState, name: &str, reason: &str) {
        state.sidecar = None;
        state.consecutive_crashes += 1;
        state.last_error = Some(reason.to_string());
        let backoff = self.backoff_for(state.consecutive_crashes);
        state.next_retry_at = Some(tokio::time::Instant::now() + backoff);
        if state.consecutive_crashes >= MAX_CONSECUTIVE_CRASHES {
            state.disabled = true;
            tracing::warn!(plugin = %name, crashes = state.consecutive_crashes,
                "disabling plugin after repeated crashes: {reason}");
        } else {
            tracing::info!(plugin = %name, crashes = state.consecutive_crashes,
                backoff_ms = backoff.as_millis() as u64, "plugin crashed, will restart: {reason}");
        }
    }

    fn backoff_for(&self, crashes: u32) -> Duration {
        let shift = crashes.saturating_sub(1).min(16);
        self.backoff_base
            .saturating_mul(1u32 << shift)
            .min(BACKOFF_CAP)
    }

    fn unavailable_error(&self, state: &SupervisorState, name: &str) -> PluginInvokeError {
        if state.disabled {
            PluginInvokeError::new(format!("plugin '{name}' is disabled"))
        } else {
            PluginInvokeError::new(format!(
                "plugin '{name}' unavailable, backing off after {} crash(es)",
                state.consecutive_crashes
            ))
        }
    }

    /// Reset the crash counter after a successful invocation.
    async fn reset_crashes(&self, entry: &Arc<PluginEntry>) {
        let mut state = entry.state.lock().await;
        state.consecutive_crashes = 0;
        state.next_retry_at = None;
        state.last_error = None;
    }

    async fn invoke_inner(
        &self,
        req: PluginHookRequest,
    ) -> Result<PluginHookResponse, PluginInvokeError> {
        let entry = {
            let plugins = self.plugins.lock().expect("registry poisoned");
            plugins.get(&req.plugin).cloned()
        };
        let Some(entry) = entry else {
            return Err(PluginInvokeError::new(format!(
                "no plugin registered as '{}'",
                req.plugin
            )));
        };

        // Hold the state guard only long enough to get a live sidecar and check
        // the subscription; drop it before the network round-trip.
        let sidecar = {
            let mut state = entry.state.lock().await;
            let sidecar = self.ensure_alive(&entry, &mut state).await?;
            if !state.subscriptions.contains(&canonical_event(&req.event)) {
                // Not subscribed: no-op without an RPC.
                return Ok(PluginHookResponse::Observed);
            }
            sidecar
        };

        let invocation_id = format!(
            "inv-{}",
            self.next_invocation.fetch_add(1, Ordering::Relaxed)
        );
        let timeout = if req.timeout_ms == 0 {
            DEFAULT_INVOKE_TIMEOUT
        } else {
            Duration::from_millis(req.timeout_ms)
        };
        let params = json!({
            "invocation_id": invocation_id,
            "event": req.event,
            "gate": event_gate(&req.event),
            "payload": req.payload,
            "timeout_ms": req.timeout_ms,
        });

        match sidecar.call("hook_invoke", params, timeout).await {
            Ok(value) => {
                self.reset_crashes(&entry).await;
                map_result(&req.event, value)
            }
            Err(crate::sidecar::SidecarError::Timeout) => Err(PluginInvokeError::new(format!(
                "plugin '{}' timed out after {} ms",
                req.plugin, req.timeout_ms
            ))),
            Err(crate::sidecar::SidecarError::Closed) => {
                let mut state = entry.state.lock().await;
                self.note_crash(&mut state, &req.plugin, "transport closed during invoke");
                Err(PluginInvokeError::new(format!(
                    "plugin '{}' transport closed during invoke",
                    req.plugin
                )))
            }
            Err(crate::sidecar::SidecarError::Rpc(e)) => Err(PluginInvokeError::new(format!(
                "plugin '{}' returned an error: {e}",
                req.plugin
            ))),
        }
    }

    /// Execute one plugin tool call (`tool_invoke`) in the plugin's sidecar.
    ///
    /// `tool` is the bare (unprefixed) name as declared in the manifest;
    /// `context` is the per-call context ({session_id, cwd, agent}) the
    /// handler receives. `timeout_ms == 0` selects
    /// [`DEFAULT_TOOL_INVOKE_TIMEOUT`]; the timeout is a hard deadline — on
    /// expiry (and on a sidecar crash mid-call) the caller gets an `Err`
    /// immediately, never a hang, and maps it onto an error tool result for
    /// the model. Timeouts do not count as crashes (the sidecar may be
    /// legitimately busy); a closed transport does, feeding the normal
    /// restart/backoff policy.
    pub async fn invoke_tool(
        &self,
        plugin: &str,
        tool: &str,
        arguments: Value,
        context: ToolCallContextDto,
        timeout_ms: u64,
    ) -> Result<ToolInvokeResult, PluginInvokeError> {
        let entry = {
            let plugins = self.plugins.lock().expect("registry poisoned");
            plugins.get(plugin).cloned()
        };
        let Some(entry) = entry else {
            return Err(PluginInvokeError::new(format!(
                "no plugin registered as '{plugin}'"
            )));
        };

        // Same locking discipline as `invoke_inner`: hold the state guard only
        // to obtain a live sidecar, never across the RPC round-trip, so
        // concurrent tool calls (and hook invokes) to one plugin don't
        // serialize on the wire.
        let sidecar = {
            let mut state = entry.state.lock().await;
            self.ensure_alive(&entry, &mut state).await?
        };

        let invocation_id = format!(
            "tinv-{}",
            self.next_invocation.fetch_add(1, Ordering::Relaxed)
        );
        let timeout = if timeout_ms == 0 {
            DEFAULT_TOOL_INVOKE_TIMEOUT
        } else {
            Duration::from_millis(timeout_ms)
        };
        let params = json!({
            "invocation_id": invocation_id.clone(),
            "tool": tool,
            "arguments": arguments,
            "context": context,
            "timeout_ms": timeout.as_millis() as u64,
        });

        // Abandon-on-abort: if this future is dropped before the call resolves
        // (the parent turn was aborted mid-tool-call — the shell aborts the
        // turn task, dropping this `invoke_tool` await), tell the plugin so its
        // handler's `AbortSignal` fires and it can cancel invocation-scoped
        // work (e.g. the subagents it spawned). Disarmed the moment the call
        // returns, so a normal completion or timeout sends nothing.
        let cancel_guard = ToolInvokeAbortGuard {
            sidecar: Arc::clone(&sidecar),
            invocation_id,
            armed: true,
        };
        let call_result = sidecar.call("tool_invoke", params, timeout).await;
        cancel_guard.disarm();
        match call_result {
            Ok(value) => {
                self.reset_crashes(&entry).await;
                serde_json::from_value::<ToolInvokeResult>(value).map_err(|e| {
                    PluginInvokeError::new(format!(
                        "plugin '{plugin}' returned a bad tool result for '{tool}': {e}"
                    ))
                })
            }
            Err(crate::sidecar::SidecarError::Timeout) => Err(PluginInvokeError::new(format!(
                "plugin '{plugin}' tool '{tool}' timed out after {} ms",
                timeout.as_millis()
            ))),
            Err(crate::sidecar::SidecarError::Closed) => {
                let mut state = entry.state.lock().await;
                self.note_crash(&mut state, plugin, "transport closed during tool_invoke");
                Err(PluginInvokeError::new(format!(
                    "plugin '{plugin}' crashed during tool '{tool}'"
                )))
            }
            Err(crate::sidecar::SidecarError::Rpc(e)) => Err(PluginInvokeError::new(format!(
                "plugin '{plugin}' tool '{tool}' failed: {e}"
            ))),
        }
    }

    /// Deliver a `panel_action` notification to a named plugin (the panel's
    /// owner). Best-effort, fire-and-forget, like tool_cancel: an unregistered
    /// plugin or a sidecar that won't start is logged and dropped, never a
    /// panic — a stale button press must not wedge the UI.
    pub async fn deliver_panel_action(
        &self,
        plugin: &str,
        params: xai_grok_plugin_protocol::PanelActionParams,
    ) {
        let entry = {
            let plugins = self.plugins.lock().expect("registry poisoned");
            plugins.get(plugin).cloned()
        };
        let Some(entry) = entry else {
            tracing::debug!(plugin = %plugin, "panel_action for unregistered plugin; dropping");
            return;
        };
        // Same locking discipline as invoke_tool: obtain a live sidecar under
        // the state guard, then drop it before touching the wire.
        let sidecar = {
            let mut state = entry.state.lock().await;
            match self.ensure_alive(&entry, &mut state).await {
                Ok(sidecar) => sidecar,
                Err(e) => {
                    tracing::debug!(plugin = %plugin, "panel_action undeliverable: {e}");
                    return;
                }
            }
        };
        sidecar.notify_panel_action(&params);
    }
}

/// Drop guard that fires a `tool_cancel` notification to the plugin when an
/// in-flight `tool_invoke` is abandoned (the awaiting future is dropped before
/// the call resolves). [`Self::disarm`] on normal return/timeout suppresses it.
struct ToolInvokeAbortGuard {
    sidecar: Arc<crate::sidecar::PluginSidecar>,
    invocation_id: String,
    armed: bool,
}

impl ToolInvokeAbortGuard {
    /// Consume the guard without notifying (the call resolved on its own).
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for ToolInvokeAbortGuard {
    fn drop(&mut self) {
        if self.armed {
            self.sidecar.notify_tool_cancel(&self.invocation_id);
        }
    }
}

impl SupervisorState {
    fn derive_state(&self) -> PluginState {
        if self.disabled {
            PluginState::Disabled
        } else if self.sidecar.as_ref().is_some_and(|s| s.is_alive()) {
            PluginState::Running
        } else if self
            .next_retry_at
            .is_some_and(|r| tokio::time::Instant::now() < r)
        {
            PluginState::BackingOff
        } else {
            PluginState::Idle
        }
    }
}

impl PluginHookInvoker for PluginHost {
    fn invoke<'a>(&'a self, req: PluginHookRequest) -> PluginHookFuture<'a> {
        Box::pin(async move { self.invoke_inner(req).await })
    }
}

/// Collapse an event name to its canonical wire form (e.g. `subagent_end` →
/// `subagent_stop`) so a subscription and a fired event meet regardless of which
/// spelling each used. Unknown/future names pass through unchanged, still
/// comparing equal to themselves.
fn canonical_event(event: &str) -> String {
    match serde_json::from_value::<HookEventName>(Value::String(event.to_string())) {
        // `Display` renders the canonical snake_case wire form.
        Ok(name) => name.canonical().to_string(),
        Err(_) => event.to_string(),
    }
}

/// The wire gate for an event, from the authoritative `xai-grok-hooks` traits
/// table (so the two never drift). Unknown events default to `Observe`.
fn event_gate(event: &str) -> GateKindDto {
    let parsed: Result<HookEventName, _> = serde_json::from_value(Value::String(event.to_string()));
    match parsed {
        Ok(name) => match name.traits().gate {
            GateKind::Observe => GateKindDto::Observe,
            GateKind::Tool => GateKindDto::Tool,
            GateKind::Stop => GateKindDto::Stop,
            GateKind::Replace => GateKindDto::Replace,
            GateKind::Intercept => GateKindDto::Intercept,
        },
        Err(_) => GateKindDto::Observe,
    }
}

/// Map a plugin's `HookInvokeResult` onto the runner's response vocabulary.
///
/// `Replace` maps to [`PluginHookResponse::Replace`]; the runner keeps or
/// substitutes the payload per the fired gate (a Replace reply on a non-Replace
/// gate is logged and passed through there). A malformed reply is a plugin bug
/// reported as an invoke error, which the runner fails open on.
fn map_result(event: &str, value: Value) -> Result<PluginHookResponse, PluginInvokeError> {
    let result: HookInvokeResult = serde_json::from_value(value)
        .map_err(|e| PluginInvokeError::new(format!("bad hook result for '{event}': {e}")))?;
    Ok(match result {
        HookInvokeResult::Observed => PluginHookResponse::Observed,
        HookInvokeResult::Decision { decision, reason } => PluginHookResponse::Decision {
            allow: matches!(decision, DecisionDto::Allow),
            reason,
        },
        HookInvokeResult::Stop {
            block,
            reason,
            continue_,
            additional_context,
        } => PluginHookResponse::Stop {
            block,
            reason,
            continue_,
            additional_context,
        },
        HookInvokeResult::Replace { payload } => PluginHookResponse::Replace { payload },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_gate_matches_hooks_traits() {
        assert_eq!(event_gate("pre_tool_use"), GateKindDto::Tool);
        assert_eq!(event_gate("stop"), GateKindDto::Stop);
        assert_eq!(event_gate("subagent_stop"), GateKindDto::Stop);
        assert_eq!(event_gate("post_tool_use"), GateKindDto::Observe);
        assert_eq!(event_gate("session_start"), GateKindDto::Observe);
        assert_eq!(event_gate("provider_request"), GateKindDto::Replace);
        assert_eq!(event_gate("provider_error"), GateKindDto::Replace);
        assert_eq!(event_gate("permission_ask"), GateKindDto::Tool);
        // Unknown -> Observe (fail-open).
        assert_eq!(event_gate("nonexistent_event"), GateKindDto::Observe);
    }

    #[test]
    fn map_decision_and_stop_and_replace() {
        let d = map_result(
            "pre_tool_use",
            json!({ "kind": "decision", "decision": "deny", "reason": "nope" }),
        )
        .unwrap();
        assert!(matches!(
            d,
            PluginHookResponse::Decision { allow: false, reason: Some(r) } if r == "nope"
        ));

        let s = map_result(
            "stop",
            json!({ "kind": "stop", "block": true, "continue": false }),
        )
        .unwrap();
        assert!(matches!(
            s,
            PluginHookResponse::Stop {
                block: true,
                continue_: Some(false),
                ..
            }
        ));

        // Replace maps through to a Replace response (payload preserved); the
        // runner decides passthrough vs. substitution per the fired gate.
        let r = map_result(
            "provider_request",
            json!({ "kind": "replace", "payload": { "swapped": true } }),
        )
        .unwrap();
        assert!(matches!(
            r,
            PluginHookResponse::Replace { payload: Some(p) } if p == json!({ "swapped": true })
        ));

        let r = map_result(
            "provider_request",
            json!({ "kind": "replace", "payload": null }),
        )
        .unwrap();
        assert!(matches!(r, PluginHookResponse::Replace { payload: None }));
    }

    #[test]
    fn map_bad_result_is_error() {
        assert!(map_result("stop", json!({ "kind": "bogus" })).is_err());
    }

    #[test]
    fn canonical_event_collapses_aliases() {
        // `subagent_end` is an alias of `subagent_stop`: both canonicalize alike
        // so a subscription under either spelling matches the fired event.
        assert_eq!(canonical_event("subagent_end"), "subagent_stop");
        assert_eq!(canonical_event("subagent_stop"), "subagent_stop");
        assert_eq!(canonical_event("pre_tool_use"), "pre_tool_use");
        // Unknown/future names pass through, still equal to themselves.
        assert_eq!(canonical_event("future_event"), "future_event");
    }

    #[test]
    fn backoff_grows_and_caps() {
        let host = PluginHost::new_for_test(
            PathBuf::from("/tmp"),
            Box::new(|_| Err("unused".into())),
            Duration::from_millis(100),
        );
        assert_eq!(host.backoff_for(1), Duration::from_millis(100));
        assert_eq!(host.backoff_for(2), Duration::from_millis(200));
        assert_eq!(host.backoff_for(3), Duration::from_millis(400));
        // Caps at 30s well before overflow.
        assert_eq!(host.backoff_for(100), BACKOFF_CAP);
    }
}
