//! Session-level wiring for the TypeScript plugin sidecar host.
//!
//! Bridges three landed building blocks into a live session:
//!
//! - `xai-grok-agent`'s [`PluginRegistry`] surfaces which loaded plugins declare
//!   a TS sidecar (`LoadedPlugin::sidecar_spec()`).
//! - `xai-grok-plugin-host`'s [`PluginHost`] owns one sidecar process per plugin
//!   and implements [`xai_grok_hooks::invoker::PluginHookInvoker`].
//! - `xai-grok-hooks`'s [`HookSpec`] with [`HandlerType::Plugin`] is what the
//!   dispatcher fires; we synthesize one per event so plugin-declared hooks run.
//!
//! Nothing here spawns a sidecar: the host starts them lazily on first matching
//! invocation. Building the host is cheap and safe even when a session has no
//! sidecar plugins (in which case we return `None` and no host is held).

use std::path::PathBuf;
use std::sync::Arc;

use xai_grok_agent::plugins::{PluginRegistry, PluginRuntime};
use xai_grok_hooks::config::{
    DEFAULT_STOP_GATE_TIMEOUT_MS, DEFAULT_TIMEOUT_MS, HandlerType, HookSpec,
};
use xai_grok_hooks::event::{GateKind, HookEventName};
use xai_grok_plugin_host::{PluginHost, RegisteredPlugin, RuntimeKind};

/// Canonical hook events a sidecar plugin is auto-subscribed to. All events the
/// core fires except the `SubagentEnd` alias (canonicalizes to `SubagentStop`,
/// so listing both would fire a subscribed plugin twice for one event). The
/// host short-circuits events a plugin didn't actually subscribe to (known
/// post-handshake), so registering the full set costs nothing at runtime.
///
/// `SubagentResolve` is included so an SDK plugin's `hooks: { subagent_resolve }`
/// works without a hooks.json declaration; the provider seams stay out because
/// their dispatch sits on the per-request hot path and must remain opt-in
/// (declared hooks only) — a spec here would arm the interceptor for every
/// session with any sidecar plugin.
pub(crate) const SIDECAR_HOOK_EVENTS: &[HookEventName] = &[
    HookEventName::SessionStart,
    HookEventName::SessionEnd,
    HookEventName::Stop,
    HookEventName::StopFailure,
    HookEventName::PreToolUse,
    HookEventName::PostToolUse,
    HookEventName::PostToolUseFailure,
    HookEventName::PermissionDenied,
    HookEventName::UserPromptSubmit,
    HookEventName::Notification,
    HookEventName::SubagentStart,
    HookEventName::SubagentStop,
    HookEventName::PreCompact,
    HookEventName::PostCompact,
    HookEventName::SubagentResolve,
    // Credential seams (dispatched from the auth boundary, not a session fire
    // site). resolve/refresh are Replace; the interactive authorization flow is
    // Intercept and gets a longer deadline (below).
    HookEventName::ResolveCredential,
    HookEventName::RefreshCredential,
    HookEventName::StartOauthFlow,
];

/// Per-hook deadline for the interactive authorization flow (`start_oauth_flow`,
/// an Intercept gate). Far longer than [`DEFAULT_TIMEOUT_MS`] because the flow
/// waits on a human (browser sign-in / code paste); still bounded so a stuck
/// plugin can't hang forever. Bounded below the hook-timeout cap.
const DEFAULT_INTERACTIVE_GATE_TIMEOUT_MS: u64 = 300_000;

/// Map the agent-side runtime selection onto the host's runtime enum. Both are
/// `auto|bun|node|deno`; kept as an explicit match so a future variant on either
/// side fails to compile until it's mapped, rather than drifting silently.
fn runtime_kind(runtime: PluginRuntime) -> RuntimeKind {
    match runtime {
        PluginRuntime::Auto => RuntimeKind::Auto,
        PluginRuntime::Bun => RuntimeKind::Bun,
        PluginRuntime::Node => RuntimeKind::Node,
        PluginRuntime::Deno => RuntimeKind::Deno,
    }
}

/// Storage root for plugin sidecars: `~/.grok/plugin-storage/`, alongside the
/// `plugin-data/` tree `LoadedPlugin::data_dir()` uses. The host namespaces each
/// plugin into its own file underneath.
fn plugin_storage_dir() -> PathBuf {
    xai_grok_config::grok_home().join("plugin-storage")
}

/// The last-mile spawn hardener the host applies to each sidecar `Command`. On
/// Linux, `network: false` plugins get the per-child seccomp network filter
/// (`xai-grok-sandbox`) installed via `pre_exec`; the host crate itself never
/// depends on the sandbox — this closure is the seam. `None` elsewhere.
#[cfg(target_os = "linux")]
fn spawn_hardener() -> Option<xai_grok_plugin_host::SpawnHardener> {
    Some(Arc::new(
        |cmd: &mut tokio::process::Command, network: bool| {
            if !network {
                // SAFETY: `install_child_network_filter` performs only
                // async-signal-safe syscalls (prctl + seccomp install), the
                // documented contract for a `pre_exec` hook.
                unsafe {
                    cmd.pre_exec(|| xai_grok_sandbox::child_net::install_child_network_filter());
                }
            }
        },
    ))
}

/// No sidecar network hardening on non-Linux (matches how the sandbox crate is
/// `cfg`'d for other child spawns in the shell).
#[cfg(not(target_os = "linux"))]
fn spawn_hardener() -> Option<xai_grok_plugin_host::SpawnHardener> {
    None
}

/// Build a [`PluginHost`] for a session's TS sidecar plugins, or `None` when the
/// registry has no sidecar plugins (the common case — session startup stays free
/// of any plugin-host machinery).
///
/// Registers one plugin per active plugin that resolves a `sidecar_spec()`;
/// spawning is deferred until the first matching hook fires.
/// `subagent_event_tx` (the session's coordinator channel) arms the `agent_*`
/// orchestration RPCs; without it they answer `method_not_found`.
pub(crate) fn build_session_plugin_host(
    plugin_registry: Option<&PluginRegistry>,
    session_id: &str,
    workspace_root: &str,
    subagent_event_tx: Option<
        tokio::sync::mpsc::UnboundedSender<
            xai_grok_tools::implementations::grok_build::task::types::SubagentEvent,
        >,
    >,
) -> Option<Arc<PluginHost>> {
    let registry = plugin_registry?;
    let workspace_root = PathBuf::from(workspace_root);
    // Tier 1 orchestration: when this process is a leader, every sidecar gets
    // the session leader's socket (initialize capability + GROK_LEADER_SOCKET
    // env) so a plugin can attach as one more headless ACP client.
    let leader_socket = crate::leader::active_leader_socket()
        .map(|p| p.to_string_lossy().into_owned());

    let sidecar_plugins: Vec<RegisteredPlugin> = registry
        .active_plugins()
        .iter()
        .filter_map(|plugin| {
            let spec = plugin.sidecar_spec()?;
            Some(RegisteredPlugin {
                name: plugin.name.clone(),
                entry: spec.entry,
                runtime: runtime_kind(spec.runtime),
                network: spec.network,
                // TODO(plugin-config): plugins have no per-plugin settings
                // mechanism today (`PluginManifest` carries only mcp/lsp config
                // paths). When one lands, forward it here so `initialize` and
                // `config_get` see the plugin's own config instead of `{}`.
                config: serde_json::json!({}),
                declared_tools: spec.tools.iter().map(|t| t.name.clone()).collect(),
                workspace_root: workspace_root.clone(),
                session_id: session_id.to_string(),
                leader_socket: leader_socket.clone(),
            })
        })
        .collect();

    if sidecar_plugins.is_empty() {
        return None;
    }

    let mut host = PluginHost::new(plugin_storage_dir());
    if let Some(hardener) = spawn_hardener() {
        host.set_spawn_hardener(hardener);
    }
    // Tier 2 orchestration: route the `agent_*` RPCs through this session's
    // subagent coordinator channel, so plugin spawns are real children of the
    // session (TUI-visible, cancellable) on the exact same path as Task spawns.
    if let Some(tx) = subagent_event_tx {
        host.set_agent_orchestrator(Arc::new(SessionAgentOrchestrator {
            session_id: session_id.to_string(),
            tx,
        }));
    }
    for spec in &sidecar_plugins {
        tracing::info!(
            plugin = %spec.name,
            runtime = ?spec.runtime,
            network = spec.network,
            "registering TS sidecar plugin with host",
        );
        host.register_plugin(spec.clone());
    }
    Some(Arc::new(host))
}

/// Synthesize the [`HandlerType::Plugin`] hook specs for one sidecar plugin —
/// one per canonical event in [`SIDECAR_HOOK_EVENTS`] — so the dispatcher routes
/// those events to the plugin's sidecar via the injected invoker.
///
/// Registered beside the command/http plugin-hook appends in the hooks_adapter
/// merge path (`reload_hooks_impl` / `apply_plugin_registry_snapshot`), so they
/// inherit the same load lifecycle and precedence.
pub(crate) fn sidecar_plugin_hook_specs(
    plugin_name: &str,
    source_dir: &std::path::Path,
) -> Vec<HookSpec> {
    SIDECAR_HOOK_EVENTS
        .iter()
        .map(|&event| {
            let timeout_ms = match event.traits().gate {
                GateKind::Stop => DEFAULT_STOP_GATE_TIMEOUT_MS,
                GateKind::Intercept => DEFAULT_INTERACTIVE_GATE_TIMEOUT_MS,
                _ => DEFAULT_TIMEOUT_MS,
            };
            HookSpec {
                name: format!("plugin/{plugin_name}/sidecar:{event}"),
                event,
                handler_type: HandlerType::Plugin,
                configured_matcher: None,
                matcher: None,
                enabled: true,
                command: None,
                command_raw: None,
                url: None,
                url_raw: None,
                plugin: Some(plugin_name.to_string()),
                // `None` → the plugin runner uses the event name as the handler
                // id, matching the SDK's `hooks: { <event>: ... }` dictionary.
                plugin_handler: None,
                timeout_ms,
                source_dir: source_dir.to_path_buf(),
                extra_env: std::collections::HashMap::new(),
            }
        })
        .collect()
}

/// The shell's [`xai_grok_plugin_host::AgentOrchestrator`]: routes every
/// plugin `agent_*` RPC through the session's subagent coordinator channel.
/// Plugin-spawned subagents are therefore real children of the session —
/// spawned, tracked, surfaced, and cancelled by the exact machinery behind the
/// model's Task tool. All methods are callable from the host's plain
/// `tokio::spawn` request tasks (the channel is `Send`; the coordinator drain
/// runs on the agent's own thread).
pub(crate) struct SessionAgentOrchestrator {
    pub(crate) session_id: String,
    pub(crate) tx: tokio::sync::mpsc::UnboundedSender<
        xai_grok_tools::implementations::grok_build::task::types::SubagentEvent,
    >,
}

/// Default agent type for a plugin spawn that names none.
const PLUGIN_SPAWN_DEFAULT_AGENT_TYPE: &str = "general-purpose";

impl xai_grok_plugin_host::AgentOrchestrator for SessionAgentOrchestrator {
    fn spawn(
        &self,
        spec: xai_grok_plugin_host::AgentSpawnSpec,
    ) -> Result<xai_grok_plugin_host::SpawnedSubagent, String> {
        use xai_grok_plugin_host::AgentStatusDto;
        use xai_grok_tools::implementations::grok_build::task::types::{
            ModelOverrideProvenance, SubagentEvent, SubagentRequest, SubagentResult,
            SubagentRuntimeOverrides,
        };

        let id = uuid::Uuid::now_v7().to_string();
        let (result_tx, coord_rx) = tokio::sync::oneshot::channel::<SubagentResult>();
        let (outcome_tx, outcome_rx) = tokio::sync::oneshot::channel();
        // Bridge the coordinator's result type onto the host's wire-shaped
        // outcome. Dropping without a send (session teardown) propagates as a
        // dropped `outcome_tx`, which the host reports as a failure.
        tokio::spawn(async move {
            if let Ok(result) = coord_rx.await {
                let status = match result.status() {
                    "completed" => AgentStatusDto::Completed,
                    "cancelled" => AgentStatusDto::Cancelled,
                    _ => AgentStatusDto::Failed,
                };
                let _ = outcome_tx.send(xai_grok_plugin_host::AgentOutcome {
                    status,
                    output: result.output.to_string(),
                    error: result.error,
                    tokens_used: result.tokens_used,
                    duration_ms: result.duration_ms,
                    tool_calls: result.tool_calls,
                    turns: result.turns,
                });
            }
        });

        let request = SubagentRequest {
            id: id.clone(),
            prompt: spec.prompt,
            description: spec
                .description
                .unwrap_or_else(|| format!("plugin:{}", spec.plugin)),
            subagent_type: spec
                .agent_type
                .unwrap_or_else(|| PLUGIN_SPAWN_DEFAULT_AGENT_TYPE.to_string()),
            parent_session_id: self.session_id.clone(),
            // No parent prompt: a plugin spawn belongs to the session, not to
            // whichever turn happens to be running (turn cancellation must not
            // reap it; the per-spawn timeout and agent_cancel do).
            parent_prompt_id: None,
            resume_from: None,
            cwd: spec.cwd,
            runtime_overrides: SubagentRuntimeOverrides {
                model: spec.model,
                // Tool provenance: a plugin-supplied slug gets the same
                // catalog validation as a model-emitted `Task.model`.
                model_override_provenance: ModelOverrideProvenance::Tool,
                ..Default::default()
            },
            // Background: never block the parent's turn, survive turn ends.
            run_in_background: true,
            // The plugin owns the result; don't queue a between-turn
            // completion reminder at the model.
            surface_completion: false,
            fork_context: false,
            result_tx,
        };
        self.tx
            .send(SubagentEvent::Spawn(Box::new(request)))
            .map_err(|_| "subagent coordinator unavailable (agent shutting down?)".to_string())?;
        Ok(xai_grok_plugin_host::SpawnedSubagent {
            id,
            result_rx: outcome_rx,
        })
    }

    fn progress<'a>(
        &'a self,
        id: &'a str,
    ) -> xai_grok_plugin_host::OrchestratorFuture<'a, Option<xai_grok_plugin_host::AgentProgress>>
    {
        use xai_grok_tools::implementations::grok_build::task::types::{
            SubagentEvent, SubagentQueryRequest, SubagentSnapshotStatus,
        };
        Box::pin(async move {
            let (respond_to, rx) = tokio::sync::oneshot::channel();
            self.tx
                .send(SubagentEvent::Query(SubagentQueryRequest {
                    subagent_id: id.to_string(),
                    block: false,
                    timeout_ms: None,
                    respond_to,
                }))
                .ok()?;
            let snapshot = rx.await.ok().flatten()?;
            match snapshot.status {
                SubagentSnapshotStatus::Initializing => {
                    Some(xai_grok_plugin_host::AgentProgress {
                        phase: "initializing",
                        turns: 0,
                        tool_calls: 0,
                        tokens_used: 0,
                        elapsed_ms: snapshot.duration_ms,
                    })
                }
                SubagentSnapshotStatus::Running {
                    turn_count,
                    tool_call_count,
                    tokens_used,
                    ..
                } => Some(xai_grok_plugin_host::AgentProgress {
                    phase: "running",
                    turns: turn_count,
                    tool_calls: tool_call_count,
                    tokens_used,
                    elapsed_ms: snapshot.duration_ms,
                }),
                // Terminal states are delivered through the outcome channel.
                _ => None,
            }
        })
    }

    fn cancel<'a>(
        &'a self,
        id: &'a str,
    ) -> xai_grok_plugin_host::OrchestratorFuture<'a, xai_grok_plugin_host::OrchestratorCancel>
    {
        use xai_grok_plugin_host::OrchestratorCancel;
        use xai_grok_tools::implementations::grok_build::task::types::{
            SubagentCancelOutcome, SubagentCancelRequest, SubagentCancelTarget, SubagentEvent,
        };
        Box::pin(async move {
            let (respond_to, rx) = tokio::sync::oneshot::channel();
            if self
                .tx
                .send(SubagentEvent::Cancel(SubagentCancelRequest {
                    target: SubagentCancelTarget::SubagentId(id.to_string()),
                    respond_to,
                }))
                .is_err()
            {
                return OrchestratorCancel::NotFound;
            }
            match rx.await {
                Ok(SubagentCancelOutcome::Cancelled) => OrchestratorCancel::Cancelled,
                Ok(SubagentCancelOutcome::AlreadyFinished { .. }) => {
                    OrchestratorCancel::AlreadyFinished
                }
                Ok(SubagentCancelOutcome::NotFound) | Err(_) => OrchestratorCancel::NotFound,
            }
        })
    }

    fn list_agent_types<'a>(&'a self) -> xai_grok_plugin_host::OrchestratorFuture<'a, Vec<String>> {
        use xai_grok_tools::implementations::grok_build::task::types::{
            SubagentEvent, SubagentListTypesRequest,
        };
        Box::pin(async move {
            let (respond_to, rx) = tokio::sync::oneshot::channel();
            if self
                .tx
                .send(SubagentEvent::ListTypes(SubagentListTypesRequest {
                    parent_session_id: self.session_id.clone(),
                    respond_to,
                }))
                .is_err()
            {
                return Vec::new();
            }
            rx.await.unwrap_or_default()
        })
    }
}

/// One manifest-declared sidecar tool prepared for catalog registration:
/// the qualified name, the dispatching [`PluginSidecarTool`], and the input
/// schema forwarded to the model.
pub(crate) struct PluginToolRegistration {
    pub(crate) qualified_name: String,
    pub(crate) tool: PluginSidecarTool,
    pub(crate) input_schema: serde_json::Value,
}

/// Build the tool-catalog registrations for every active sidecar plugin that
/// declares manifest tools. Pure (no registration side effects) so it is unit
/// testable; the session spawn path feeds the result to
/// `ToolBridge::register_mcp_tools`, which is the exact channel MCP tools ride —
/// the model sees `<plugin>__<tool>` names, and permission checks plus
/// pre/post_tool_use hooks apply on the shared dispatch path with no extra
/// wiring (the name parses as an MCP qualified name → `AccessKind::MCPTool`).
///
/// Invalid qualified names (should be impossible after manifest validation,
/// but the MCP-side validator is authoritative) are warned about and skipped,
/// mirroring `McpTool::into_registration`.
pub(crate) fn plugin_sidecar_tool_registrations(
    registry: &PluginRegistry,
    host: &Arc<PluginHost>,
    session_id: &str,
    agent: &str,
    fallback_cwd: &str,
) -> Vec<PluginToolRegistration> {
    let mut out = Vec::new();
    for plugin in registry.active_plugins() {
        let Some(spec) = plugin.sidecar_spec() else {
            continue;
        };
        for tool in &spec.tools {
            if let Some(reg) = sidecar_tool_registration(
                &plugin.name,
                tool,
                host,
                session_id,
                agent,
                fallback_cwd,
            ) {
                out.push(reg);
            }
        }
    }
    out
}

/// Build one catalog registration for a validated manifest tool, or `None`
/// (with a warning) when the qualified name fails the authoritative MCP-side
/// validators. Split out of [`plugin_sidecar_tool_registrations`] for direct
/// unit testing (and reused by the sidecar e2e tests).
pub(crate) fn sidecar_tool_registration(
    plugin_name: &str,
    tool: &xai_grok_agent::plugins::SidecarToolSpec,
    host: &Arc<PluginHost>,
    session_id: &str,
    agent: &str,
    fallback_cwd: &str,
) -> Option<PluginToolRegistration> {
    use crate::session::mcp_servers::{
        MCP_TOOL_NAME_DELIMITER, parse_mcp_tool_name, validate_tool_name,
    };

    let qualified_name = format!("{plugin_name}{MCP_TOOL_NAME_DELIMITER}{}", tool.name);
    if parse_mcp_tool_name(&qualified_name).is_none() {
        tracing::warn!(plugin = %plugin_name, tool = %tool.name,
            "skipping sidecar tool with ambiguous qualified name");
        return None;
    }
    if let Err(reason) = validate_tool_name(&qualified_name) {
        tracing::warn!(plugin = %plugin_name, tool = %tool.name, reason = %reason,
            "skipping sidecar tool with invalid name");
        return None;
    }
    Some(PluginToolRegistration {
        qualified_name,
        tool: PluginSidecarTool {
            host: Arc::clone(host),
            plugin: plugin_name.to_string(),
            tool: tool.name.clone(),
            description: tool.description.clone(),
            timeout_ms: tool.timeout_ms,
            session_id: session_id.to_string(),
            agent: agent.to_string(),
            fallback_cwd: fallback_cwd.to_string(),
        },
        input_schema: tool.input_schema.clone(),
    })
}

/// A manifest-declared plugin tool in the session tool catalog. Dispatch is
/// the `tool_invoke` RPC: the handler runs in the plugin's sidecar with the
/// full plugin context (storage/agents/config/log) plus the per-call context
/// assembled here — {session_id, cwd, agent}, with cwd resolved per call
/// (`Cwd` override first, then the session resources), so a handler can key
/// its state per project and per caller.
pub(crate) struct PluginSidecarTool {
    host: Arc<PluginHost>,
    /// Owning plugin (= the `server` half of the qualified name).
    plugin: String,
    /// Bare tool name as declared in the manifest.
    tool: String,
    description: String,
    /// Per-tool deadline from the manifest; `0` → host default.
    timeout_ms: u64,
    session_id: String,
    /// `"main"` for the root session, otherwise the subagent type label.
    agent: String,
    /// Session cwd used when the runtime context carries none.
    fallback_cwd: String,
}

impl std::fmt::Debug for PluginSidecarTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginSidecarTool")
            .field("plugin", &self.plugin)
            .field("tool", &self.tool)
            .finish()
    }
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for PluginSidecarTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::Other
    }

    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        xai_grok_tools::types::tool::ToolNamespace::MCP
    }

    fn description_template(&self) -> &str {
        &self.description
    }
}

impl xai_tool_runtime::Tool for PluginSidecarTool {
    type Args = serde_json::Value;
    type Output = xai_grok_tools::types::output::ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        // Qualified so two plugins exposing the same bare tool name get
        // distinct LocalRegistry entries (mirrors `McpErasedTool::id`).
        let qualified = format!(
            "{}{}{}",
            self.plugin,
            crate::session::mcp_servers::MCP_TOOL_NAME_DELIMITER,
            self.tool
        );
        xai_tool_protocol::ToolId::new(&qualified)
            .unwrap_or_else(|_| xai_tool_protocol::ToolId::new("plugin_tool").expect("valid"))
    }

    fn description(
        &self,
        _ctx: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(&self.tool, &self.description)
    }

    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        raw: serde_json::Value,
    ) -> Result<xai_grok_tools::types::output::ToolOutput, xai_tool_runtime::ToolError> {
        use xai_grok_tools::types::output::{MCPOutput, ToolOutput};

        // Per-call cwd: the dispatch layer's `Cwd` override wins, then the
        // session resources' cwd, then the registration-time fallback.
        let cwd = match xai_grok_tools::types::tool_metadata::shared_resources(&ctx) {
            Ok(resources) => {
                xai_grok_tools::types::tool_metadata::resolve_cwd(&ctx, &resources)
                    .await
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| self.fallback_cwd.clone())
            }
            Err(_) => ctx
                .extensions
                .get::<xai_tool_runtime::Cwd>()
                .map(|c| c.0.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.fallback_cwd.clone()),
        };

        let call_context = xai_grok_plugin_host::ToolCallContextDto {
            session_id: self.session_id.clone(),
            cwd,
            agent: self.agent.clone(),
        };

        match self
            .host
            .invoke_tool(
                &self.plugin,
                &self.tool,
                raw,
                call_context,
                self.timeout_ms,
            )
            .await
        {
            // Handler-reported failure: an ordinary error tool result, the
            // same shape an MCP tool error takes in the conversation.
            Ok(result) if result.is_error => Ok(ToolOutput::MCP(MCPOutput::errored(
                self.tool.clone(),
                self.plugin.clone(),
                result.content,
            ))),
            Ok(result) => Ok(ToolOutput::MCP(MCPOutput::okay_output(
                self.tool.clone(),
                self.plugin.clone(),
                result.content,
            ))),
            // Infrastructure failure (timeout, sidecar crash, disabled
            // plugin): a ToolError, so the model sees the failure and the
            // post_tool_use_failure path fires — never a hang (the host's
            // deadline already bounded the wait).
            Err(e) => Err(xai_tool_runtime::ToolError::custom(
                "plugin_tool",
                e.message,
            )),
        }
    }
}

/// `permission_ask` seam over the session plugin host, handed to the permission
/// manager so a plugin can allow/deny a guarded tool call before the interactive
/// prompt (see `xai_grok_workspace::permission::PermissionAskHook`).
///
/// The manager is built before the plugin host exists (the host lands later in
/// session spawn), so this holds deferred slots filled via [`Self::attach`] once
/// the host is ready. A permission prompt only fires during a turn — well after
/// startup — so the slots are populated by then; an unfilled slot fails open
/// (passthrough → the normal prompt).
pub(crate) struct PluginPermissionAsk {
    host: std::sync::OnceLock<Arc<PluginHost>>,
    plugins: std::sync::OnceLock<Vec<String>>,
}

impl PluginPermissionAsk {
    pub(crate) fn new() -> Self {
        Self {
            host: std::sync::OnceLock::new(),
            plugins: std::sync::OnceLock::new(),
        }
    }

    /// Fill the deferred slots once the session plugin host and its registered
    /// plugin names are known. Idempotent: later calls are ignored.
    pub(crate) fn attach(&self, host: Arc<PluginHost>, plugins: Vec<String>) {
        let _ = self.host.set(host);
        let _ = self.plugins.set(plugins);
    }
}

#[async_trait::async_trait]
impl xai_grok_workspace::permission::PermissionAskHook for PluginPermissionAsk {
    async fn ask(
        &self,
        payload: serde_json::Value,
    ) -> xai_grok_workspace::permission::PermissionAskDecision {
        use xai_grok_hooks::invoker::{PluginHookInvoker, PluginHookRequest, PluginHookResponse};
        use xai_grok_workspace::permission::PermissionAskDecision;

        let (Some(host), Some(plugins)) = (self.host.get(), self.plugins.get()) else {
            return PermissionAskDecision::Passthrough;
        };
        let event = HookEventName::PermissionAsk.to_string();
        // Deny wins over allow across subscribers; a non-subscriber, observe, or
        // errored response contributes nothing (fail-open to the prompt).
        let mut allow = false;
        for plugin in plugins {
            let req = PluginHookRequest {
                plugin: plugin.clone(),
                handler: event.clone(),
                event: event.clone(),
                payload: payload.clone(),
                timeout_ms: DEFAULT_TIMEOUT_MS,
            };
            match host.invoke(req).await {
                Ok(PluginHookResponse::Decision {
                    allow: false,
                    reason,
                }) => {
                    return PermissionAskDecision::Deny(
                        reason.unwrap_or_else(|| "denied by permission_ask plugin".to_string()),
                    );
                }
                Ok(PluginHookResponse::Decision { allow: true, .. }) => allow = true,
                _ => {}
            }
        }
        if allow {
            PermissionAskDecision::Allow
        } else {
            PermissionAskDecision::Passthrough
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_specs_cover_canonical_events_without_alias() {
        let specs = sidecar_plugin_hook_specs("demo", std::path::Path::new("/tmp/demo"));
        assert_eq!(specs.len(), SIDECAR_HOOK_EVENTS.len());
        // No `SubagentEnd` alias (would double-fire on `subagent_stop`).
        assert!(!specs.iter().any(|s| s.event == HookEventName::SubagentEnd));
        // Every spec is a plugin handler routed to the named plugin.
        for s in &specs {
            assert_eq!(s.handler_type, HandlerType::Plugin);
            assert_eq!(s.plugin.as_deref(), Some("demo"));
            assert!(s.plugin_handler.is_none());
        }
        // Stop-gate events get the long stop-gate timeout; others the default.
        let stop = specs
            .iter()
            .find(|s| s.event == HookEventName::Stop)
            .unwrap();
        assert_eq!(stop.timeout_ms, DEFAULT_STOP_GATE_TIMEOUT_MS);
        let pre = specs
            .iter()
            .find(|s| s.event == HookEventName::PreToolUse)
            .unwrap();
        assert_eq!(pre.timeout_ms, DEFAULT_TIMEOUT_MS);
        // The interactive authorization flow (Intercept) gets the long deadline.
        let oauth = specs
            .iter()
            .find(|s| s.event == HookEventName::StartOauthFlow)
            .unwrap();
        assert_eq!(oauth.timeout_ms, DEFAULT_INTERACTIVE_GATE_TIMEOUT_MS);
    }

    #[test]
    fn runtime_mapping_is_total() {
        assert_eq!(runtime_kind(PluginRuntime::Auto), RuntimeKind::Auto);
        assert_eq!(runtime_kind(PluginRuntime::Bun), RuntimeKind::Bun);
        assert_eq!(runtime_kind(PluginRuntime::Node), RuntimeKind::Node);
        assert_eq!(runtime_kind(PluginRuntime::Deno), RuntimeKind::Deno);
    }

    fn tool_spec(name: &str) -> xai_grok_agent::plugins::SidecarToolSpec {
        xai_grok_agent::plugins::SidecarToolSpec {
            name: name.to_string(),
            description: "a tool".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
            timeout_ms: 0,
        }
    }

    fn test_host() -> Arc<PluginHost> {
        Arc::new(PluginHost::new(std::env::temp_dir().join("plugin-tool-test")))
    }

    #[test]
    fn sidecar_tool_registration_uses_mcp_qualified_name() {
        let host = test_host();
        let reg = sidecar_tool_registration(
            "demo-hooks",
            &tool_spec("echo"),
            &host,
            "sess-1",
            "main",
            "/ws",
        )
        .expect("valid tool registers");
        // Exactly the MCP convention: `<server>__<tool>` with the plugin as
        // the server half — permission matchers and the `AccessKind::MCPTool`
        // classification apply unchanged.
        assert_eq!(reg.qualified_name, "demo-hooks__echo");
        assert_eq!(reg.input_schema["type"], "object");
        use xai_tool_runtime::Tool as _;
        assert_eq!(reg.tool.id().as_str(), "demo-hooks__echo");
    }

    #[test]
    fn sidecar_tool_registration_rejects_ambiguous_names() {
        let host = test_host();
        // A bare name containing `__` would make the qualified name ambiguous
        // to split; the manifest validator already drops it, and this seam
        // (the authoritative MCP-side check) must also refuse it.
        assert!(
            sidecar_tool_registration(
                "demo-hooks",
                &tool_spec("has__delim"),
                &host,
                "sess-1",
                "main",
                "/ws",
            )
            .is_none()
        );
    }
}
