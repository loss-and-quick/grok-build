//! Session-level wiring for the plugin sidecar host.
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

use xai_grok_agent::plugins::{PluginRegistry, PluginRuntime, SidecarLaunch};
use xai_grok_hooks::config::{
    DEFAULT_STOP_GATE_TIMEOUT_MS, DEFAULT_TIMEOUT_MS, HandlerType, HookSpec,
};
use xai_grok_hooks::event::{GateKind, HookEventName};
use xai_grok_plugin_host::{PluginHost, PluginLaunch, RegisteredPlugin, RuntimeKind};

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

/// Shallow-merge a plugin's user config (`[plugins.<name>]` in config.toml)
/// over its manifest `config` defaults.
///
/// Merge depth is one level: every top-level key present in `user` replaces the
/// same key in `defaults` wholesale — nested objects/arrays are taken from
/// `user` as-is, not recursively merged — while keys only in `defaults` are
/// preserved. A non-object on either side contributes nothing (coerced to an
/// empty object), so the result is always a JSON object and the SDK's
/// `ctx.config()` never observes a non-object value.
fn merge_plugin_config(
    defaults: &serde_json::Value,
    user: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut out = defaults.as_object().cloned().unwrap_or_default();
    if let Some(user_obj) = user.and_then(|v| v.as_object()) {
        for (key, value) in user_obj {
            out.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(out)
}

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

/// Map the manifest-resolved launch form onto the host's. Same explicit-match
/// discipline as [`runtime_kind`]: the two enums are deliberately separate types
/// (the agent crate does not depend on the host) and must not drift.
fn plugin_launch(launch: SidecarLaunch) -> PluginLaunch {
    match launch {
        SidecarLaunch::Runtime { entry, runtime } => PluginLaunch::Runtime {
            entry,
            runtime: runtime_kind(runtime),
        },
        SidecarLaunch::Command { program, args } => PluginLaunch::Command { program, args },
    }
}

/// Say so, loudly, when a plugin asked for `network: false` on a platform that
/// has no per-child network filter to give it.
///
/// The manifest advertises `network: false` as a guarantee; without a hardener
/// the only thing still enforcing it is a deno sidecar's own withheld
/// `--allow-net`, and nothing at all enforces it for bun, node, or an `exec`
/// program. That gap predates directly-executed plugins — it is what
/// `spawn_hardener()` returning `None` off Linux has always meant — but a
/// guarantee that quietly evaporates is worse than one the manifest could not
/// express, so it gets a warning rather than silence.
fn warn_if_network_confinement_unavailable(plugins: &[RegisteredPlugin], hardened: bool) {
    if hardened {
        return;
    }
    let confined: Vec<&str> = plugins
        .iter()
        .filter(|p| !p.network)
        .map(|p| p.name.as_str())
        .collect();
    if !confined.is_empty() {
        tracing::warn!(
            plugins = %confined.join(", "),
            "no per-child network filter on this platform; `network: false` is not enforced \
             for these sidecars beyond a deno runtime's own permission flags",
        );
    }
}

/// A one-line label for a launch form, for the registration log.
fn launch_label(launch: &PluginLaunch) -> String {
    match launch {
        PluginLaunch::Runtime { entry, runtime } => {
            format!("{runtime:?} {}", entry.display())
        }
        PluginLaunch::Command { program, args } => {
            format!("exec {} {}", program.display(), args.join(" "))
        }
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
///
/// `cmd_tx` is the session's command channel, which arms the panel RPCs. It is
/// optional because this is also how the agent-level sign-in host is built
/// (`MvpAgent::plugin_sign_in_seam`): that host exists before any session, so
/// there is no channel to route panels onto and `ui_publish_panel` /
/// `ui_close_panel` answer `method_not_found` there — the sign-in seam is
/// deliberately the only surface a session-less host serves.
pub(crate) fn build_session_plugin_host(
    plugin_registry: Option<&PluginRegistry>,
    session_id: &str,
    workspace_root: &str,
    plugin_config: &std::collections::BTreeMap<String, serde_json::Value>,
    subagent_event_tx: Option<
        tokio::sync::mpsc::UnboundedSender<
            xai_grok_tools::implementations::grok_build::task::types::SubagentEvent,
        >,
    >,
    cmd_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::session::commands::SessionCommand>>,
) -> Option<Arc<PluginHost>> {
    let registry = plugin_registry?;
    let workspace_root = PathBuf::from(workspace_root);
    // Tier 1 orchestration: when this process is a leader, every sidecar gets
    // the session leader's socket (initialize capability + GROK_LEADER_SOCKET
    // env) so a plugin can attach as one more headless ACP client.
    let leader_socket =
        crate::leader::active_leader_socket().map(|p| p.to_string_lossy().into_owned());

    let sidecar_plugins = registered_sidecar_plugins(
        registry,
        session_id,
        &workspace_root,
        plugin_config,
        leader_socket.as_deref(),
    );

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
    // UI panels: route `ui_publish_panel` / `ui_close_panel` from any sidecar
    // onto this session's command channel, where the actor emits the
    // corresponding `plugin_panel` / `panel_closed` session notification (and
    // `panel_action` presses flow back the other way).
    if let Some(cmd_tx) = cmd_tx {
        host.set_panel_sink(Arc::new(SessionPanelSink { cmd_tx }));
    }
    // Interactive sign-in: `auth_publish_url` / `auth_await_code` drive the
    // core's own login screen, which is session-independent (a sign-in happens
    // before any session exists). Every host gets the same process-wide prompt,
    // so a `/login` served from a live session and one served by the
    // agent-level host reach the identical screen.
    host.set_sign_in_sink(crate::auth::plugin_sign_in::PluginSignInPrompt::global());
    warn_if_network_confinement_unavailable(&sidecar_plugins, spawn_hardener().is_some());
    for spec in &sidecar_plugins {
        tracing::info!(
            plugin = %spec.name,
            launch = %launch_label(&spec.launch),
            network = spec.network,
            "registering sidecar plugin with host",
        );
        host.register_plugin(spec.clone());
    }
    Some(Arc::new(host))
}

/// The sidecar host a spawning session runs with.
///
/// A subagent borrows its parent's host (`inherited`) instead of standing up its
/// own. A sidecar is a process: building one per child turns a fan-out of ten
/// into ten more processes, ten handshakes, and — because each host loads a
/// plugin's storage file into memory once and then writes the whole map back —
/// ten copies of that map racing to overwrite each other. The host's per-plugin
/// lock only orders writers inside one process, so the "core guarantees
/// atomicity + locking" promise the storage contract makes holds exactly as far
/// as one host does.
///
/// Nothing is lost by sharing: which agent a hook came from rides on each
/// invocation (`subagentType` on the tool payloads, the child's own session id
/// on the envelope, `agent` in the tool-call context), not on the host. The
/// sinks the parent installed stay pointed at the parent, which is where a
/// panel or a plugin-spawned agent belongs — the child's channels are not on
/// screen and die with the child.
///
/// `build` is called only for a top-level session; a child never evaluates it,
/// so the config read behind it stays off the spawn path of every subagent.
pub(crate) fn session_plugin_host(
    is_subagent: bool,
    inherited: Option<Arc<PluginHost>>,
    build: impl FnOnce() -> Option<Arc<PluginHost>>,
) -> Option<Arc<PluginHost>> {
    if is_subagent { inherited } else { build() }
}

/// Build the [`RegisteredPlugin`] list for a registry's active sidecar plugins,
/// stamping each with its merged per-plugin config
/// (`merge_plugin_config(manifest_defaults, user[name])`). Pure (no host side
/// effects) so the config wiring is unit-testable; `build_session_plugin_host`
/// feeds the result to the host.
fn registered_sidecar_plugins(
    registry: &PluginRegistry,
    session_id: &str,
    workspace_root: &std::path::Path,
    plugin_config: &std::collections::BTreeMap<String, serde_json::Value>,
    leader_socket: Option<&str>,
) -> Vec<RegisteredPlugin> {
    registry
        .active_plugins()
        .iter()
        .filter_map(|plugin| {
            let spec = plugin.sidecar_spec()?;
            Some(RegisteredPlugin {
                name: plugin.name.clone(),
                launch: plugin_launch(spec.launch),
                network: spec.network,
                // Per-plugin config the sidecar sees at `initialize` and via
                // `config_get`: the manifest's `config` defaults with the user's
                // `[plugins.<name>]` config.toml entries shallow-merged on top.
                config: merge_plugin_config(&spec.config, plugin_config.get(&plugin.name)),
                declared_tools: spec.tools.iter().map(|t| t.name.clone()).collect(),
                workspace_root: workspace_root.to_path_buf(),
                session_id: session_id.to_string(),
                leader_socket: leader_socket.map(|s| s.to_string()),
            })
        })
        .collect()
}

/// Synthesize the [`HandlerType::Plugin`] hook specs for one sidecar plugin —
/// one per canonical event in [`SIDECAR_HOOK_EVENTS`] — so the dispatcher routes
/// those events to the plugin's sidecar via the injected invoker.
///
/// Emitted beside the command/http plugin hooks by [`plugin_hook_specs`], the
/// one place every merge path (session spawn, `reload_hooks_impl`,
/// `apply_plugin_registry_snapshot`) collects plugin specs, so they inherit the
/// same load lifecycle and precedence.
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
                layer: xai_grok_hooks::config::HookProvenance::Plugin,
            }
        })
        .collect()
}

/// Every hook spec a plugin registry contributes, in the order the merge paths
/// append them: per active plugin, its `hooks.json` file specs, then its inline
/// manifest hook specs, then the synthetic sidecar specs. The single definition
/// of "what plugins add to a hook registry" — session spawn, `/hooks reload`,
/// and the plugin-registry snapshot all route through it, so no path can go
/// blind to a spec kind the others honour.
///
/// Adapter warnings are returned rather than logged so each caller can log them
/// in its own context. Every spec is named `plugin/…`, which is what lets the
/// snapshot path re-clean its previous contribution with
/// `remove_by_prefix("plugin/")` before re-appending.
pub(crate) fn plugin_hook_specs(registry: &PluginRegistry) -> (Vec<HookSpec>, Vec<String>) {
    let mut specs = Vec::new();
    let mut warnings = Vec::new();
    for plugin in registry.active_plugins() {
        // File-based hooks (`hooks.json`, or the manifest's `hooks` path).
        if let Some(ref hooks_path) = plugin.hooks_path {
            let (file_specs, file_warnings) =
                xai_grok_agent::plugins::hooks_adapter::parse_plugin_hooks(
                    hooks_path,
                    &plugin.name,
                    &plugin.root_str(),
                    &plugin.data_dir_str(),
                );
            specs.extend(file_specs);
            warnings.extend(file_warnings);
        }
        // Inline manifest hooks (`"hooks": { … }` instead of a path).
        if let Some(ref inline_value) = plugin.inline_hooks {
            let (inline_specs, inline_warnings) =
                xai_grok_agent::plugins::hooks_adapter::parse_plugin_hooks_from_value(
                    inline_value,
                    &plugin.name,
                    &plugin.root_str(),
                    &plugin.data_dir_str(),
                );
            specs.extend(inline_specs);
            warnings.extend(inline_warnings);
        }
        // TS sidecar plugins: synthetic `HandlerType::Plugin` specs for all
        // canonical events so the dispatcher routes them to the sidecar via the
        // injected `PluginHookInvoker`. Registering the full event set (rather
        // than probing subscriptions here) is cheap: the host short-circuits
        // events a plugin didn't subscribe to after its handshake.
        if plugin.sidecar_spec().is_some() {
            specs.extend(sidecar_plugin_hook_specs(&plugin.name, &plugin.root));
        }
    }
    (specs, warnings)
}

/// Append every [`plugin_hook_specs`] contribution to a session's hook registry,
/// returning the registry the session should run with.
///
/// Hook *discovery* builds the registry from hook files and config layers and
/// knows nothing about plugins, so this is what puts a plugin on the dispatcher
/// at all. Without it a freshly spawned session has no plugin hooks: every seam
/// — `session_start`, `provider_request`, the credential events,
/// `permission_ask` — stays dead until an unrelated `/hooks reload` or
/// `ReloadPlugins` fan-out happens to rebuild the registry.
///
/// Returns the input unchanged when no active plugin contributes a spec, so a
/// plugin-less session keeps a `None` registry and skips the machinery.
///
/// The append is idempotent: any `plugin/…` specs already on the input registry
/// are dropped first. A subagent session spawns with its parent's registry as
/// the override, and that registry has been through here already — without the
/// re-clean it ended up holding two specs per event per plugin, so every plugin
/// hook ran twice for the whole life of the child.
pub(crate) fn registry_with_plugin_specs(
    registry: Option<Arc<xai_grok_hooks::discovery::HookRegistry>>,
    plugin_registry: Option<&PluginRegistry>,
) -> Option<Arc<xai_grok_hooks::discovery::HookRegistry>> {
    let (specs, warnings) = plugin_registry.map(plugin_hook_specs).unwrap_or_default();
    // Logged even when nothing was contributed: an unreadable `hooks.json` warns
    // and yields no specs, and session startup is where that must surface.
    for w in &warnings {
        tracing::warn!("session spawn: {w}");
    }
    if specs.is_empty() {
        return registry;
    }
    let mut merged = registry.map(|arc| (*arc).clone()).unwrap_or_default();
    // Same re-clean `apply_plugin_registry_snapshot` performs before its own
    // append: every spec this function contributes is named `plugin/…`, so the
    // prefix removal takes back exactly the previous contribution and nothing
    // else (config-layer and `agent:` specs keep their own names).
    merged.remove_by_prefix("plugin/");
    merged.append_specs(specs);
    Some(Arc::new(merged))
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
            ModelOverrideProvenance, SubagentEvent, SubagentOwner, SubagentRequest, SubagentResult,
            SubagentRuntimeOverrides, SubagentSpawnRequest,
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
            // `agent_send` sets this to a prior terminal subagent id: the
            // coordinator resumes that conversation into this child (raw
            // transcript, tool state, model), then runs `prompt`. A plain
            // `agent_spawn` leaves it `None`.
            resume_from: spec.resume_from,
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
            // The plugin polls its own result channel; the coordinator never
            // blocks a turn on this spawn.
            await_to_completion: false,
            fork_context: false,
            owner: SubagentOwner::Task,
            // Session-owned, not turn-owned: a fresh token so turn cancellation
            // can't reap it (per-spawn timeout and agent_cancel still apply).
            cancel_token: tokio_util::sync::CancellationToken::new(),
        };
        // The terminal reply channel rides beside the request in the spawn
        // envelope (upstream split it out of `SubagentRequest`).
        self.tx
            .send(SubagentEvent::Spawn(SubagentSpawnRequest {
                request: Box::new(request),
                result_tx,
            }))
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
                    // Scope to this session: a plugin may only observe children
                    // its own session spawned, never another session's.
                    parent_session_id: Some(self.session_id.clone()),
                    block: false,
                    timeout_ms: None,
                    respond_to,
                }))
                .ok()?;
            let snapshot = rx.await.ok().flatten()?;
            match snapshot.status {
                SubagentSnapshotStatus::Initializing => Some(xai_grok_plugin_host::AgentProgress {
                    phase: "initializing",
                    turns: 0,
                    tool_calls: 0,
                    tokens_used: 0,
                    elapsed_ms: snapshot.duration_ms,
                }),
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
                    // Same scoping as `progress`: a plugin cannot cancel a
                    // child belonging to another session.
                    parent_session_id: Some(self.session_id.clone()),
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

    fn message<'a>(
        &'a self,
        id: &'a str,
        text: &'a str,
    ) -> xai_grok_plugin_host::OrchestratorFuture<'a, xai_grok_plugin_host::OrchestratorMessage>
    {
        use xai_grok_plugin_host::OrchestratorMessage;
        use xai_grok_tools::implementations::grok_build::task::types::{
            SubagentEvent, SubagentMessageOutcome, SubagentMessageRequest,
        };
        Box::pin(async move {
            let (respond_to, rx) = tokio::sync::oneshot::channel();
            if self
                .tx
                .send(SubagentEvent::Message(SubagentMessageRequest {
                    subagent_id: id.to_string(),
                    // Same scoping as `progress` and `cancel`: a plugin cannot
                    // steer a child belonging to another session.
                    parent_session_id: Some(self.session_id.clone()),
                    text: text.to_string(),
                    respond_to,
                }))
                .is_err()
            {
                return OrchestratorMessage::Unreachable;
            }
            match rx.await {
                Ok(SubagentMessageOutcome::Delivered) => OrchestratorMessage::Delivered,
                Ok(SubagentMessageOutcome::NotDelivered) => OrchestratorMessage::NotDelivered,
                Ok(SubagentMessageOutcome::NotStarted) => OrchestratorMessage::NotStarted,
                Ok(SubagentMessageOutcome::AlreadyFinished { .. }) => {
                    OrchestratorMessage::AlreadyFinished
                }
                Ok(SubagentMessageOutcome::Unreachable) => OrchestratorMessage::Unreachable,
                Ok(SubagentMessageOutcome::NotFound) => OrchestratorMessage::NotFound,
                // The coordinator dropped the reply channel: the agent is going
                // away, which is an unreachable child rather than a missing one.
                Err(_) => OrchestratorMessage::Unreachable,
            }
        })
    }

    fn list_agent_types<'a>(
        &'a self,
    ) -> xai_grok_plugin_host::OrchestratorFuture<'a, Vec<xai_grok_plugin_host::AgentDescriptor>>
    {
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
            rx.await
                .unwrap_or_default()
                .into_iter()
                .map(|d| xai_grok_plugin_host::AgentDescriptor {
                    name: d.name,
                    description: d.description,
                    model: d.model,
                })
                .collect()
        })
    }
}

/// The shell's [`xai_grok_plugin_host::PanelSink`]: forwards a sidecar's
/// `ui_publish_panel` / `ui_close_panel` onto the session command channel, where
/// the actor turns them into `plugin_panel` / `panel_closed` session
/// notifications (the same `send_xai_notification` path every other update
/// rides). Fire-and-forget: a closed channel (session tearing down) drops the
/// emit rather than erroring — a late panel update must never wedge a plugin.
pub(crate) struct SessionPanelSink {
    pub(crate) cmd_tx: tokio::sync::mpsc::UnboundedSender<crate::session::commands::SessionCommand>,
}

impl xai_grok_plugin_host::PanelSink for SessionPanelSink {
    fn publish_panel(&self, plugin: &str, view_model: xai_grok_plugin_protocol::PanelViewModel) {
        let _ = self
            .cmd_tx
            .send(crate::session::commands::SessionCommand::EmitPluginPanel {
                plugin: plugin.to_string(),
                view_model,
            });
    }

    fn close_panel(&self, plugin: &str, panel_id: &str) {
        let _ = self
            .cmd_tx
            .send(crate::session::commands::SessionCommand::ClosePluginPanel {
                plugin: plugin.to_string(),
                panel_id: panel_id.to_string(),
            });
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
            if let Some(reg) =
                sidecar_tool_registration(&plugin.name, tool, host, session_id, agent, fallback_cwd)
            {
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
            Ok(resources) => xai_grok_tools::types::tool_metadata::resolve_cwd(&ctx, &resources)
                .await
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| self.fallback_cwd.clone()),
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
            .invoke_tool(&self.plugin, &self.tool, raw, call_context, self.timeout_ms)
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

    /// A `DiscoveredPlugin` rooted at `parent/<name>`, exactly as discovery
    /// would hand it to [`PluginRegistry::from_discovered`]. `sidecar` writes the
    /// entry file too: the manifest's `plugin` field only resolves to a
    /// `SidecarSpec` when that file is really on disk.
    fn discovered_plugin(
        parent: &std::path::Path,
        name: &str,
        sidecar: bool,
        trusted: bool,
    ) -> xai_grok_agent::plugins::discovery::DiscoveredPlugin {
        use xai_grok_agent::plugins::discovery::{DiscoveredPlugin, PluginId};
        use xai_grok_agent::plugins::{PluginOrigin, PluginScope};

        let root = parent.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let mut manifest = serde_json::json!({ "name": name });
        if sidecar {
            std::fs::write(root.join("index.ts"), "export default {};").unwrap();
            manifest["plugin"] = serde_json::json!("./index.ts");
        }
        DiscoveredPlugin {
            manifest: serde_json::from_value(manifest).unwrap(),
            id: PluginId::new(PluginScope::User, &root, name),
            root: root.clone(),
            canonical_root: root,
            scope: PluginScope::User,
            origin: PluginOrigin::UserGrok,
            trusted,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: None,
            lsp_config_path: None,
            conflict: None,
        }
    }

    /// The `hooks` block a test plugin declares: one `post_tool_use` command
    /// hook, in the settings-file shape both the file and inline parsers take.
    fn hooks_json(command: &str) -> serde_json::Value {
        serde_json::json!({
            "hooks": {
                "PostToolUse": [{ "hooks": [{ "type": "command", "command": command }] }]
            }
        })
    }

    /// Give a discovered plugin inline manifest hooks (`plugin.inline_hooks`).
    fn with_inline_hooks(
        mut dp: xai_grok_agent::plugins::discovery::DiscoveredPlugin,
        hooks: serde_json::Value,
    ) -> xai_grok_agent::plugins::discovery::DiscoveredPlugin {
        dp.manifest.hooks = Some(xai_grok_agent::plugins::manifest::PathOrInline::Inline(
            hooks,
        ));
        dp
    }

    /// Write a real `hooks.json` into the plugin root and point discovery at it
    /// (`plugin.hooks_path`) — the file must exist, since the adapter reads it.
    fn with_hooks_file(
        mut dp: xai_grok_agent::plugins::discovery::DiscoveredPlugin,
        hooks: serde_json::Value,
    ) -> xai_grok_agent::plugins::discovery::DiscoveredPlugin {
        let path = dp.root.join("hooks.json");
        std::fs::write(&path, serde_json::to_string(&hooks).unwrap()).unwrap();
        dp.hooks_path = Some(path);
        dp
    }

    /// A registry built the production way, so `active_plugins()`'s real
    /// enabled/trusted rules — the filter `registry_with_plugin_specs` leans on
    /// — actually run instead of being simulated by a hand-picked plugin list.
    fn plugin_registry(
        plugins: Vec<xai_grok_agent::plugins::discovery::DiscoveredPlugin>,
        enabled: &[&str],
    ) -> PluginRegistry {
        let enabled: Vec<String> = enabled.iter().map(|s| (*s).to_string()).collect();
        PluginRegistry::from_discovered(plugins, &[], &enabled)
    }

    /// A registry holding one non-plugin hook, standing in for what discovery
    /// found in the user's config layer before any plugin append.
    fn registry_with_config_layer_hook() -> xai_grok_hooks::discovery::HookRegistry {
        let mut registry = xai_grok_hooks::discovery::HookRegistry::default();
        registry.append_specs(vec![HookSpec {
            name: "global/audit".to_string(),
            event: HookEventName::PreToolUse,
            handler_type: HandlerType::Command,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            command: Some(std::path::PathBuf::from("/bin/true")),
            command_raw: Some("/bin/true".to_string()),
            url: None,
            url_raw: None,
            plugin: None,
            plugin_handler: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            source_dir: std::path::PathBuf::from("/ws"),
            extra_env: std::collections::HashMap::new(),
            layer: xai_grok_hooks::config::HookProvenance::User,
        }]);
        registry
    }

    /// The spawn-time append is what puts a sidecar plugin on the dispatcher at
    /// all: hook discovery reads files and config layers only, so a session that
    /// skipped this step ran with no plugin hooks and every sidecar seam silently
    /// dead. Built from a real `PluginRegistry` — a test that seeds the hook
    /// registry with `sidecar_plugin_hook_specs` itself cannot catch that.
    #[test]
    fn sidecar_specs_reach_the_dispatcher_from_a_plugin_registry_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = plugin_registry(
            vec![discovered_plugin(tmp.path(), "council", true, true)],
            &["council"],
        );

        // `None` = what discovery hands a session with no hook files at all.
        let registry = registry_with_plugin_specs(None, Some(&plugins))
            .expect("a sidecar plugin must produce a registry");

        // Looked up the way the dispatcher looks it up, not merely counted:
        // `start_oauth_flow` is the seam whose silent death exposed the gap.
        assert!(registry.has_enabled_hooks_for_canonical(HookEventName::StartOauthFlow));
        let oauth = registry.hooks_for_canonical(HookEventName::StartOauthFlow);
        assert_eq!(oauth.len(), 1);
        assert_eq!(oauth[0].handler_type, HandlerType::Plugin);
        assert_eq!(oauth[0].name, "plugin/council/sidecar:start_oauth_flow");
        assert_eq!(oauth[0].source_dir, tmp.path().join("council"));

        // Every other canonical sidecar event is reachable too, so no seam is
        // left dead by an append that only half-populated the registry.
        for &event in SIDECAR_HOOK_EVENTS {
            assert!(
                registry.has_enabled_hooks_for_canonical(event),
                "sidecar event {event} is unreachable"
            );
        }
    }

    /// `credential_seam::registry_for_plugin` narrows to one plugin's sign-in by
    /// filtering `spec.plugin == Some(<name>)`, so specs that arrived without
    /// attribution would either vanish from that filter or run the wrong flow.
    #[test]
    fn appended_specs_are_attributed_to_their_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = plugin_registry(
            vec![
                discovered_plugin(tmp.path(), "council", true, true),
                discovered_plugin(tmp.path(), "notary", true, true),
            ],
            &["council", "notary"],
        );

        let registry = registry_with_plugin_specs(None, Some(&plugins)).expect("registry built");
        let oauth = registry.hooks_for_canonical(HookEventName::StartOauthFlow);
        assert_eq!(oauth.len(), 2);
        assert!(oauth.iter().all(|s| s.plugin.is_some()));

        // The exact narrowing `registry_for_plugin` performs.
        let targeted: Vec<&str> = oauth
            .iter()
            .filter(|s| s.plugin.as_deref() == Some("council"))
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(targeted, ["plugin/council/sidecar:start_oauth_flow"]);
    }

    /// The append must merge, not replace: a session that already loaded hooks
    /// from its config layer keeps them once a sidecar plugin joins.
    #[test]
    fn pre_existing_hooks_survive_the_append() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = plugin_registry(
            vec![discovered_plugin(tmp.path(), "council", true, true)],
            &["council"],
        );

        let existing = Arc::new(registry_with_config_layer_hook());
        let merged =
            registry_with_plugin_specs(Some(existing), Some(&plugins)).expect("registry built");

        // Both hooks live on `pre_tool_use`; neither displaces the other.
        let pre = merged.hooks_for_canonical(HookEventName::PreToolUse);
        assert!(
            pre.iter()
                .any(|s| s.name == "global/audit" && s.handler_type == HandlerType::Command),
            "config-layer hook was dropped by the plugin append"
        );
        assert!(pre.iter().any(
            |s| s.plugin.as_deref() == Some("council") && s.handler_type == HandlerType::Plugin
        ));
        assert!(merged.has_enabled_hooks_for_canonical(HookEventName::StartOauthFlow));
    }

    /// A subagent session spawns with its parent's registry as the hook
    /// override, and that registry already went through this append. Appending
    /// again on top of it must not stack a second copy of every spec: the
    /// dispatcher runs one spec per registered hook, so a duplicated set means
    /// every plugin hook fires twice for the whole life of the child — twice the
    /// sidecar round-trips, and two runs to render where the plugin acted once.
    #[test]
    fn inheriting_a_parents_registry_does_not_stack_a_second_plugin_spec_set() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = plugin_registry(
            vec![with_hooks_file(
                discovered_plugin(tmp.path(), "council", true, true),
                hooks_json("./audit.sh"),
            )],
            &["council"],
        );

        // The parent session's registry: a config-layer hook plus the plugin
        // contribution.
        let parent = registry_with_plugin_specs(
            Some(Arc::new(registry_with_config_layer_hook())),
            Some(&plugins),
        )
        .expect("parent registry built");
        // The subagent spawn: the parent's registry handed straight back in.
        let child = registry_with_plugin_specs(Some(parent.clone()), Some(&plugins))
            .expect("child registry built");

        assert_eq!(
            child.len(),
            parent.len(),
            "the child stacked a second copy of the plugin specs"
        );
        for &event in SIDECAR_HOOK_EVENTS {
            let sidecar: Vec<&str> = child
                .hooks_for_canonical(event)
                .iter()
                .filter(|s| s.handler_type == HandlerType::Plugin)
                .map(|s| s.name.as_str())
                .collect();
            assert_eq!(
                sidecar.len(),
                1,
                "{event} would fire {} times in a subagent",
                sidecar.len()
            );
        }
        // The plugin's file hooks are re-cleaned by the same prefix, so they too
        // survive exactly once.
        let post: Vec<&str> = child
            .hooks_for_canonical(HookEventName::PostToolUse)
            .iter()
            .filter(|s| s.handler_type == HandlerType::Command)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(post.len(), 1, "plugin file hook duplicated: {post:?}");
        // Nothing outside the `plugin/` namespace was taken by the re-clean.
        assert!(
            child
                .hooks_for_canonical(HookEventName::PreToolUse)
                .iter()
                .any(|s| s.name == "global/audit"),
            "the re-clean removed a config-layer hook"
        );
    }

    /// A session with no plugins keeps its registry byte-for-byte — including
    /// staying `None`, so no plugin machinery is armed for a plain session.
    #[test]
    fn no_plugin_registry_returns_the_input_unchanged() {
        assert!(registry_with_plugin_specs(None, None).is_none());

        let existing = Arc::new(registry_with_config_layer_hook());
        let same = registry_with_plugin_specs(Some(existing.clone()), None).expect("kept");
        assert!(Arc::ptr_eq(&existing, &same), "registry was rebuilt");
    }

    /// Plugins that ship skills/commands but declare no hooks and no sidecar
    /// entry add no specs: the dispatcher has nothing to route to them.
    #[test]
    fn plugins_without_hooks_return_the_input_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = plugin_registry(
            vec![discovered_plugin(tmp.path(), "skills-only", false, true)],
            &["skills-only"],
        );
        assert!(plugins.active_plugins().len() == 1, "plugin is active");
        assert!(registry_with_plugin_specs(None, Some(&plugins)).is_none());

        let existing = Arc::new(registry_with_config_layer_hook());
        let same =
            registry_with_plugin_specs(Some(existing.clone()), Some(&plugins)).expect("kept");
        assert!(Arc::ptr_eq(&existing, &same), "registry was rebuilt");
    }

    /// A plugin whose hooks live in a `hooks.json` is just as dark as a sidecar
    /// when the spawn path skips it — the file kind flows through the very same
    /// [`plugin_hook_specs`] collection, so spawn must pick it up.
    #[test]
    fn file_hooks_reach_the_dispatcher_from_a_plugin_registry_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = plugin_registry(
            vec![with_hooks_file(
                discovered_plugin(tmp.path(), "linter", false, true),
                hooks_json("./lint.sh"),
            )],
            &["linter"],
        );

        let registry = registry_with_plugin_specs(None, Some(&plugins))
            .expect("a file-hooks plugin must produce a registry");
        let post = registry.hooks_for_canonical(HookEventName::PostToolUse);
        assert_eq!(post.len(), 1);
        assert_eq!(post[0].handler_type, HandlerType::Command);
        // Namespaced `plugin/<name>/…` so the snapshot path's
        // `remove_by_prefix("plugin/")` can re-clean it.
        assert!(
            post[0].name.starts_with("plugin/linter/"),
            "unexpected hook name: {}",
            post[0].name
        );
        assert_eq!(
            post[0].layer,
            xai_grok_hooks::config::HookProvenance::Plugin
        );
        // The adapter's env injection rode along, so `$GROK_PLUGIN_ROOT` in the
        // hook's command resolves at spawn exactly as it does after a reload.
        assert_eq!(
            post[0]
                .extra_env
                .get("GROK_PLUGIN_ROOT")
                .map(String::as_str),
            Some(tmp.path().join("linter").to_string_lossy().as_ref())
        );
    }

    /// Same for hooks declared inline in the manifest (`"hooks": { … }`): no
    /// file on disk, and previously invisible to a freshly spawned session.
    #[test]
    fn inline_hooks_reach_the_dispatcher_from_a_plugin_registry_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = plugin_registry(
            vec![with_inline_hooks(
                discovered_plugin(tmp.path(), "inliner", false, true),
                hooks_json("echo inline"),
            )],
            &["inliner"],
        );
        assert!(
            plugins.get("inliner").unwrap().hooks_path.is_none(),
            "inline hooks must not resolve a file path"
        );

        let registry = registry_with_plugin_specs(None, Some(&plugins))
            .expect("an inline-hooks plugin must produce a registry");
        let post = registry.hooks_for_canonical(HookEventName::PostToolUse);
        assert_eq!(post.len(), 1);
        assert_eq!(post[0].handler_type, HandlerType::Command);
        assert!(
            post[0].name.starts_with("plugin/inliner/"),
            "unexpected hook name: {}",
            post[0].name
        );
        assert_eq!(
            post[0].layer,
            xai_grok_hooks::config::HookProvenance::Plugin
        );
    }

    /// All three kinds from one plugin, in the order the reload paths append
    /// them: file hooks, inline hooks, then the synthetic sidecar specs. The
    /// shared collection is what keeps spawn and the two reload paths identical.
    #[test]
    fn plugin_hook_specs_collects_file_inline_and_sidecar_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = plugin_registry(
            vec![with_inline_hooks(
                with_hooks_file(
                    discovered_plugin(tmp.path(), "everything", true, true),
                    hooks_json("./from-file.sh"),
                ),
                hooks_json("echo from-inline"),
            )],
            &["everything"],
        );

        let (specs, warnings) = plugin_hook_specs(&plugins);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(specs.len(), 2 + SIDECAR_HOOK_EVENTS.len());
        // File first, inline second (the inline parser labels specs from the
        // synthetic `plugin.json` path), sidecar specs last.
        assert!(specs[0].name.starts_with("plugin/everything/hooks:"));
        assert!(specs[1].name.starts_with("plugin/everything/plugin:"));
        assert!(
            specs[2..]
                .iter()
                .all(|s| s.handler_type == HandlerType::Plugin)
        );
    }

    /// Adapter warnings come back to the caller instead of being swallowed, and
    /// a plugin that produced only warnings adds nothing: the registry is
    /// returned untouched rather than rebuilt around an empty spec list.
    #[test]
    fn unreadable_hook_files_warn_without_contributing_specs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut dp = discovered_plugin(tmp.path(), "broken", false, true);
        // Points at a file that was never written — what a plugin whose
        // `hooks.json` vanished (or is unreadable) looks like at load time.
        dp.hooks_path = Some(dp.root.join("hooks.json"));
        let plugins = plugin_registry(vec![dp], &["broken"]);

        let (specs, warnings) = plugin_hook_specs(&plugins);
        assert!(specs.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("broken") && warnings[0].contains("hooks file"),
            "unexpected warning: {}",
            warnings[0]
        );

        let existing = Arc::new(registry_with_config_layer_hook());
        let same =
            registry_with_plugin_specs(Some(existing.clone()), Some(&plugins)).expect("kept");
        assert!(Arc::ptr_eq(&existing, &same), "registry was rebuilt");
    }

    /// The original bug was not in this function — it was that *nothing called
    /// it at spawn*, so every session started with an unpopulated registry. That
    /// wiring has no unit-testable seam: it lives inside `spawn_session_actor`, a
    /// ~100-argument async fn, and the crate allows `dead_code`, so dropping the
    /// call compiles clean and silent. A source-level assertion is the only cheap
    /// guard; without it the regression this test file exists for can come back
    /// with every behavioural test still green.
    #[test]
    fn session_spawn_still_calls_the_append() {
        let spawn_src = include_str!("acp_session_impl/spawn.rs");
        assert!(
            spawn_src.contains("registry_with_plugin_specs("),
            "spawn_session_actor no longer appends plugin hook specs: every \
             plugin seam (session_start, provider_request, the credential \
             events, permission_ask) and every plugin-declared hooks.json or \
             inline hook is dead until a /hooks reload"
        );
    }

    /// `active_plugins()` is `enabled && trusted`; both halves must keep a
    /// sidecar off the dispatcher, driven through the registry's own rules
    /// rather than a hand-filtered plugin list.
    #[test]
    fn inactive_sidecar_plugins_contribute_nothing() {
        let tmp = tempfile::tempdir().unwrap();

        // Absent from the config `enabled` list → disabled (the default).
        let disabled = plugin_registry(
            vec![discovered_plugin(tmp.path(), "off-council", true, true)],
            &[],
        );
        assert_eq!(disabled.list().len(), 1, "plugin is present, just disabled");
        assert!(!disabled.get("off-council").unwrap().enabled);
        assert!(disabled.active_plugins().is_empty());
        assert!(registry_with_plugin_specs(None, Some(&disabled)).is_none());

        // Enabled but untrusted → still not active, so its sidecar stays unwired.
        let untrusted = plugin_registry(
            vec![discovered_plugin(tmp.path(), "shady-council", true, false)],
            &["shady-council"],
        );
        assert!(untrusted.get("shady-council").unwrap().enabled);
        assert!(untrusted.active_plugins().is_empty());
        assert!(registry_with_plugin_specs(None, Some(&untrusted)).is_none());
    }

    #[test]
    fn merge_plugin_config_user_overrides_manifest_defaults() {
        let defaults = serde_json::json!({
            "participants": ["default"],
            "rounds": 1,
            "keep": true,
        });
        let user = serde_json::json!({
            "participants": ["grok", "claude"],
            "rounds": 3,
        });
        let merged = merge_plugin_config(&defaults, Some(&user));
        // User keys win wholesale (the array is replaced, not concatenated).
        assert_eq!(
            merged["participants"],
            serde_json::json!(["grok", "claude"])
        );
        assert_eq!(merged["rounds"], 3);
        // Manifest-only keys survive.
        assert_eq!(merged["keep"], true);
    }

    #[test]
    fn merge_plugin_config_is_shallow_not_deep() {
        // Nested objects are replaced wholesale (depth = 1), not deep-merged:
        // `a` is taken entirely from the user, so the default `a.y` is dropped.
        let defaults = serde_json::json!({ "a": { "x": 1, "y": 2 } });
        let user = serde_json::json!({ "a": { "x": 9 } });
        let merged = merge_plugin_config(&defaults, Some(&user));
        assert_eq!(merged["a"], serde_json::json!({ "x": 9 }));
    }

    #[test]
    fn merge_plugin_config_no_user_returns_defaults() {
        let defaults = serde_json::json!({ "rounds": 2 });
        assert_eq!(merge_plugin_config(&defaults, None), defaults);
    }

    #[test]
    fn merge_plugin_config_always_yields_object() {
        // Non-object defaults / non-object user both coerce to `{}` so the
        // sidecar's `ctx.config()` never sees a non-object.
        assert_eq!(
            merge_plugin_config(&serde_json::json!([1, 2]), None),
            serde_json::json!({})
        );
        assert_eq!(
            merge_plugin_config(&serde_json::json!({}), Some(&serde_json::json!("nope"))),
            serde_json::json!({})
        );
    }

    /// End-to-end wiring: a registry's active sidecar plugin gets its manifest
    /// `config` defaults merged with the user's `[plugins.<name>]` map, and the
    /// result lands on `RegisteredPlugin.config` (the value `config_get` returns).
    #[test]
    fn registered_sidecar_plugins_stamp_merged_config() {
        use xai_grok_agent::plugins::PluginRegistry;
        use xai_grok_agent::plugins::discovery::{DiscoveredPlugin, PluginId};
        use xai_grok_agent::plugins::{PluginOrigin, PluginScope};

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("council");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.ts"), "export default {};").unwrap();

        let manifest = xai_grok_agent::plugins::PluginManifest {
            name: "council".to_string(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: None,
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: None,
            lsp_servers: None,
            plugin: Some("./index.ts".to_string()),
            runtime: None,
            exec: None,
            network: None,
            tools: None,
            config: Some(serde_json::json!({ "participants": ["default"], "rounds": 1 })),
            oauth_label: None,
            oauth_accounts: None,
        };

        let dp = DiscoveredPlugin {
            manifest,
            id: PluginId::new(PluginScope::User, &root, "council"),
            root: root.clone(),
            canonical_root: root.clone(),
            scope: PluginScope::User,
            origin: PluginOrigin::UserGrok,
            trusted: true,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: None,
            lsp_config_path: None,
            conflict: None,
        };
        let registry = PluginRegistry::from_discovered(vec![dp], &[], &["council".to_string()]);

        let mut user_config = std::collections::BTreeMap::new();
        user_config.insert(
            "council".to_string(),
            serde_json::json!({ "participants": ["grok", "claude"] }),
        );

        let registered = registered_sidecar_plugins(
            &registry,
            "sess-1",
            std::path::Path::new("/ws"),
            &user_config,
            None,
        );
        assert_eq!(registered.len(), 1);
        let cfg = &registered[0].config;
        // User overrides participants; manifest default `rounds` survives.
        assert_eq!(cfg["participants"], serde_json::json!(["grok", "claude"]));
        assert_eq!(cfg["rounds"], 1);
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
        Arc::new(PluginHost::new(
            std::env::temp_dir().join("plugin-tool-test"),
        ))
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

    /// A subagent must run on the host its parent already owns. Building one
    /// per child multiplies sidecar processes by the fan-out, and each child's
    /// host loads its own copy of a plugin's storage map and then writes the
    /// whole map back — so two children that both touch storage overwrite each
    /// other's keys and the parent's.
    #[test]
    fn a_subagent_borrows_its_parents_host_instead_of_building_one() {
        let parent = test_host();
        let other = test_host();

        let mut built = false;
        let child = session_plugin_host(true, Some(parent.clone()), || {
            built = true;
            Some(other.clone())
        });
        assert!(!built, "a subagent built a second host");
        assert!(
            Arc::ptr_eq(&child.expect("child runs on a host"), &parent),
            "the child is not on its parent's host"
        );

        // A top-level session builds and owns its own, ignoring any inherited
        // value: only a subagent spawn ever supplies one.
        let top = session_plugin_host(false, Some(parent.clone()), || Some(other.clone()));
        assert!(Arc::ptr_eq(&top.expect("top-level host"), &other));

        // A parent with no sidecar plugins gives the child none — the child
        // must not start a host the parent itself decided not to have.
        assert!(session_plugin_host(true, None, || Some(other.clone())).is_none());
    }
}
