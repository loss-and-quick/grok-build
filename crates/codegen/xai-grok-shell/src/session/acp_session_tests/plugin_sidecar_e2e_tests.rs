//! End-to-end parity: the `demo-hooks` TS sidecar plugin, driven through the
//! **real** hook dispatcher + a **real** [`PluginHost`] running a **real** JS
//! runtime, must produce the same dispatcher outcome as an equivalent command
//! hook.
//!
//! Two gates are checked:
//!
//! - **PreToolUse (Tool gate):** a tool input carrying the demo marker is denied
//!   with a specific reason. Assert the plugin's deny `reason` equals the command
//!   hook's deny `reason`.
//! - **Stop (Stop gate):** `additionalContext` is injected without blocking.
//!   Assert the plugin's aggregated `additional_context` equals the command
//!   hook's.
//!
//! # Runtime gating
//!
//! The plugin path spawns a sidecar, which needs a JS runtime (bun / node >=22 /
//! deno) on `PATH`. When none is available (CI without runtimes) the test
//! `eprintln!`s a skip notice and returns green. The nix devshell provides all
//! three, so it exercises the real path locally and in the runtime-provisioned
//! CI lane.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use xai_grok_hooks::config::{HandlerType, HookSpec};
use xai_grok_hooks::discovery::HookRegistry;
use xai_grok_hooks::event::{HookEventEnvelope, HookEventName, HookPayload};
use xai_grok_hooks::invoker::PluginHookInvoker;
use xai_grok_hooks::runner::RunContext;
use xai_grok_plugin_host::{PluginHost, RegisteredPlugin, RuntimeKind};

// These MUST match the constants in `examples/plugins/demo-hooks/index.ts`; the
// whole point of the test is that the plugin and the command hook agree on them.
const DENY_MARKER: &str = "DEMO_DENY_MARKER";
const DENY_REASON: &str = "demo-hooks denied: tool input contained the demo marker";
const STOP_CONTEXT: &str = "demo-hooks: remember to run the demo checklist before stopping";

/// Repo root, derived from this crate's manifest dir (`crates/codegen/xai-grok-shell`).
fn repo_root() -> PathBuf {
    // `dunce::canonicalize` (repo policy; std/tokio canonicalize are clippy-banned
    // for their `\\?\` verbatim paths on Windows).
    dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .expect("repo root resolves")
}

/// The demo plugin's entry file.
fn demo_entry() -> PathBuf {
    repo_root().join("examples/plugins/demo-hooks/index.ts")
}

/// `true` when a JS runtime is available. Uses the host's own probe so the gate
/// matches exactly what a real spawn would resolve.
fn runtime_available() -> bool {
    xai_grok_plugin_host::runtime::resolve_runtime(RuntimeKind::Auto).is_ok()
}

/// Build a real `PluginHost` with the demo plugin registered. `workspace_root`
/// is the repo root so a deno sidecar's `--allow-read` scope reaches the SDK the
/// demo imports (`../../../sdk/plugin/src/index.ts`), which lives outside the
/// plugin dir. No spawn hardener: this test asserts dispatch parity, not the
/// sandbox, and the sidecar needs no network.
fn build_host(data_dir: PathBuf) -> Arc<PluginHost> {
    let host = PluginHost::new(data_dir);
    host.register_plugin(RegisteredPlugin {
        name: "demo-hooks".to_string(),
        entry: demo_entry(),
        runtime: RuntimeKind::Auto,
        network: false,
        config: serde_json::json!({}),
        workspace_root: repo_root(),
        session_id: "e2e-session".to_string(),
        leader_socket: None,
    });
    Arc::new(host)
}

/// A synthetic sidecar `HookSpec` for one event, via the shared production
/// helper so the test exercises the exact specs a live session registers.
fn plugin_specs() -> Vec<HookSpec> {
    crate::session::plugin_host::sidecar_plugin_hook_specs("demo-hooks", &repo_root())
}

/// Registry holding only the demo plugin's sidecar specs.
fn plugin_registry() -> HookRegistry {
    let (mut reg, _) = xai_grok_hooks::discovery::load_hooks_from_sources(&[], &[]);
    reg.append_specs(plugin_specs());
    reg
}

/// An executable shell script echoing `stdout_json`; returns its path (kept
/// alive by `dir`).
fn write_script(dir: &Path, name: &str, stdout_json: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\ncat >/dev/null\nprintf '%s' '{stdout_json}'\n"),
    )
    .expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    path
}

/// A command `HookSpec` for `event` whose script emits `stdout_json`.
fn command_spec(event: HookEventName, script: PathBuf, source_dir: &Path) -> HookSpec {
    HookSpec {
        name: format!("cmd:{event}"),
        event,
        handler_type: HandlerType::Command,
        configured_matcher: None,
        matcher: None,
        enabled: true,
        command: Some(script),
        command_raw: None,
        url: None,
        url_raw: None,
        plugin: None,
        plugin_handler: None,
        timeout_ms: 30_000,
        source_dir: source_dir.to_path_buf(),
        extra_env: HashMap::new(),
    }
}

fn command_registry(spec: HookSpec) -> HookRegistry {
    let (mut reg, _) = xai_grok_hooks::discovery::load_hooks_from_sources(&[], &[]);
    reg.append_specs(vec![spec]);
    reg
}

fn pre_tool_use_envelope(tool_input: serde_json::Value) -> HookEventEnvelope {
    HookEventEnvelope {
        hook_event_name: HookEventName::PreToolUse,
        session_id: "e2e-session".into(),
        cwd: "/tmp".into(),
        workspace_root: "/tmp".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        transcript_path: None,
        client_identifier: None,
        prompt_id: None,
        permission_mode: Some("default".into()),
        payload: HookPayload::PreToolUse {
            tool_name: "run_terminal_command".into(),
            tool_use_id: "call-1".into(),
            tool_input,
            tool_input_truncated: false,
            subagent_type: None,
        },
    }
}

fn stop_envelope() -> HookEventEnvelope {
    HookEventEnvelope {
        hook_event_name: HookEventName::Stop,
        session_id: "e2e-session".into(),
        cwd: "/tmp".into(),
        workspace_root: "/tmp".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        transcript_path: None,
        client_identifier: None,
        prompt_id: None,
        permission_mode: Some("default".into()),
        payload: HookPayload::Stop {
            reason: "end of turn".into(),
            stop_hook_active: false,
            last_assistant_message: None,
            background_tasks: None,
            session_crons: None,
        },
    }
}

/// The deny reason a `dispatch_pre_tool_use` produced (or `None` on allow).
fn deny_reason(result: &xai_grok_hooks::dispatcher::PreToolUseResult) -> Option<String> {
    match &result.decision {
        xai_grok_hooks::result::HookDecision::Deny { reason, .. } => Some(reason.clone()),
        xai_grok_hooks::result::HookDecision::Allow => None,
    }
}

#[tokio::test]
async fn demo_plugin_and_command_hook_reach_identical_outcomes() {
    if !runtime_available() {
        eprintln!(
            "SKIP demo_plugin_and_command_hook_reach_identical_outcomes: \
             no JS runtime (bun/node/deno) on PATH"
        );
        return;
    }
    assert!(
        demo_entry().is_file(),
        "demo plugin entry missing at {}",
        demo_entry().display()
    );

    let data_dir = tempfile::tempdir().expect("tempdir");
    let scripts = tempfile::tempdir().expect("scripts dir");

    let host = build_host(data_dir.path().to_path_buf());
    let invoker: Arc<dyn PluginHookInvoker> = host.clone();

    // ── PreToolUse gate: plugin path ───────────────────────────────────────
    let plugin_reg = plugin_registry();
    let plugin_ctx = RunContext {
        session_id: "e2e-session",
        workspace_root: "/tmp",
        plugin_invoker: Some(invoker.clone()),
    };
    let denied_input = serde_json::json!({ "command": format!("echo {DENY_MARKER}") });
    let plugin_deny = xai_grok_hooks::dispatcher::dispatch_pre_tool_use(
        &plugin_reg,
        &pre_tool_use_envelope(denied_input.clone()),
        &plugin_ctx,
    )
    .await;

    // A benign input must pass (proves the deny is marker-driven, not blanket).
    let plugin_allow = xai_grok_hooks::dispatcher::dispatch_pre_tool_use(
        &plugin_reg,
        &pre_tool_use_envelope(serde_json::json!({ "command": "echo hello" })),
        &plugin_ctx,
    )
    .await;

    // ── PreToolUse gate: command path ──────────────────────────────────────
    let deny_json = format!(r#"{{"decision":"deny","reason":"{DENY_REASON}"}}"#).replace('\n', "");
    let cmd_pre_script = write_script(scripts.path(), "pre.sh", &deny_json);
    let cmd_pre_reg = command_registry(command_spec(
        HookEventName::PreToolUse,
        cmd_pre_script,
        scripts.path(),
    ));
    let cmd_ctx = RunContext {
        session_id: "e2e-session",
        workspace_root: "/tmp",
        plugin_invoker: None,
    };
    let cmd_deny = xai_grok_hooks::dispatcher::dispatch_pre_tool_use(
        &cmd_pre_reg,
        &pre_tool_use_envelope(denied_input),
        &cmd_ctx,
    )
    .await;

    // Parity assertion #1: identical deny reason surfaced to the model.
    assert_eq!(
        deny_reason(&plugin_deny),
        Some(DENY_REASON.to_string()),
        "plugin hook must deny with the demo reason"
    );
    assert_eq!(
        deny_reason(&plugin_deny),
        deny_reason(&cmd_deny),
        "plugin and command deny reasons must be identical"
    );
    assert_eq!(
        deny_reason(&plugin_allow),
        None,
        "benign tool input must not be denied by the plugin"
    );

    // ── Stop gate: plugin path ─────────────────────────────────────────────
    let plugin_stop = xai_grok_hooks::dispatcher::dispatch_stop(
        &plugin_reg,
        HookEventName::Stop,
        &stop_envelope(),
        &plugin_ctx,
    )
    .await;

    // ── Stop gate: command path ────────────────────────────────────────────
    let stop_json = format!(r#"{{"hookSpecificOutput":{{"additionalContext":"{STOP_CONTEXT}"}}}}"#);
    let cmd_stop_script = write_script(scripts.path(), "stop.sh", &stop_json);
    let cmd_stop_reg = command_registry(command_spec(
        HookEventName::Stop,
        cmd_stop_script,
        scripts.path(),
    ));
    let cmd_stop = xai_grok_hooks::dispatcher::dispatch_stop(
        &cmd_stop_reg,
        HookEventName::Stop,
        &stop_envelope(),
        &cmd_ctx,
    )
    .await;

    // Parity assertion #2: identical injected stop context, no block/force-stop.
    assert_eq!(
        plugin_stop.additional_context,
        vec![STOP_CONTEXT.to_string()],
        "plugin stop hook must inject the demo context"
    );
    assert_eq!(
        plugin_stop.additional_context, cmd_stop.additional_context,
        "plugin and command stop contexts must be identical"
    );
    assert!(
        plugin_stop.blocks.is_empty() && plugin_stop.prevent_continuation.is_none(),
        "demo stop hook must not block or force-stop"
    );
    assert_eq!(
        plugin_stop.blocks.is_empty(),
        cmd_stop.blocks.is_empty(),
        "plugin and command block behavior must match"
    );

    host.dispose().await;
}
