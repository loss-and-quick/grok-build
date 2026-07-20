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
];

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
pub(crate) fn build_session_plugin_host(
    plugin_registry: Option<&PluginRegistry>,
    session_id: &str,
    workspace_root: &str,
) -> Option<Arc<PluginHost>> {
    let registry = plugin_registry?;
    let workspace_root = PathBuf::from(workspace_root);

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
                workspace_root: workspace_root.clone(),
                session_id: session_id.to_string(),
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
            let timeout_ms = if event.traits().gate == GateKind::Stop {
                DEFAULT_STOP_GATE_TIMEOUT_MS
            } else {
                DEFAULT_TIMEOUT_MS
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
    }

    #[test]
    fn runtime_mapping_is_total() {
        assert_eq!(runtime_kind(PluginRuntime::Auto), RuntimeKind::Auto);
        assert_eq!(runtime_kind(PluginRuntime::Bun), RuntimeKind::Bun);
        assert_eq!(runtime_kind(PluginRuntime::Node), RuntimeKind::Node);
        assert_eq!(runtime_kind(PluginRuntime::Deno), RuntimeKind::Deno);
    }
}
