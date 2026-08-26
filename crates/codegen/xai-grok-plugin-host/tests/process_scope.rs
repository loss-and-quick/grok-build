//! Sidecar enrollment in a [`ProcessScope`]: the reap path that covers an owner
//! which never runs teardown.
//!
//! `PluginHost::dispose` and `PluginSidecar::Drop` both need something of the
//! host's to still be running. A session whose worker wedges offers neither, and
//! its sidecar — plus whatever workers that sidecar spawned — would survive it.
//! These drive the real spawn path with the `fake_sidecar` fixture binary and
//! assert the scope closes that hole, without ever depending on a system
//! `sleep`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use xai_grok_hooks::invoker::{PluginHookInvoker, PluginHookRequest};
use xai_grok_plugin_host::{PluginHost, PluginLaunch, PluginState, RegisteredPlugin, RuntimeKind};
use xai_tty_utils::ProcessScope;

/// A host over the fixture binary, with a plugin `p` registered.
fn host_with(env: &[(&'static str, String)]) -> (PluginHost, TempDir, TempDir) {
    let data_dir = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_fake_sidecar");
    let env: Vec<(&'static str, String)> = env.to_vec();
    let factory = Box::new(move |_spec: &RegisteredPlugin| {
        let mut cmd = tokio::process::Command::new(bin);
        for (k, v) in &env {
            cmd.env(k, v);
        }
        Ok(cmd)
    });
    let host = PluginHost::new_for_test(
        data_dir.path().to_path_buf(),
        factory,
        Duration::from_millis(10),
    );
    host.register_plugin(RegisteredPlugin {
        name: "p".to_string(),
        launch: PluginLaunch::Runtime {
            entry: PathBuf::from("/does/not/matter.ts"),
            runtime: RuntimeKind::Auto,
        },
        network: false,
        config: serde_json::json!({}),
        declared_tools: vec![],
        workspace_root: ws.path().to_path_buf(),
        session_id: "sess-1".to_string(),
        leader_socket: None,
    });
    (host, data_dir, ws)
}

fn req(event: &str) -> PluginHookRequest {
    PluginHookRequest {
        plugin: "p".to_string(),
        handler: event.to_string(),
        event: event.to_string(),
        payload: serde_json::json!({}),
        timeout_ms: 5_000,
    }
}

/// Grace for an in-flight heartbeat write to land after the kill.
const SETTLE: Duration = Duration::from_millis(250);
/// Window over which the heartbeat file must grow (live tree) or not grow
/// (reaped tree). Many times the fixture's 25 ms beat period.
const STALL_WINDOW: Duration = Duration::from_millis(750);

/// Bytes written so far by the heartbeat grandchild (0 before it starts).
fn beats(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Wait until `f` holds, or fail after `within`.
async fn until(within: Duration, what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

/// `Running` is exactly "a sidecar the supervisor holds and whose transport is
/// still up", so it is the host-level readout of the child being alive.
async fn is_running(host: &PluginHost) -> bool {
    host.status()
        .await
        .into_iter()
        .find(|s| s.name == "p")
        .expect("plugin p is registered")
        .state
        == PluginState::Running
}

/// Wait for the sidecar's transport to go down, or fail after `within`.
async fn until_transport_closed(host: &PluginHost, within: Duration) {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !is_running(host).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for the sidecar transport to close");
}

/// The core guarantee: a scope's teardown reaps a live sidecar's whole process
/// group, not just its leader. The grandchild here stands in for the bun/node
/// workers a real sidecar spawns — nothing in the host holds a handle to it, so
/// only a group kill can stop its heartbeat.
#[tokio::test]
async fn scope_teardown_reaps_the_sidecar_and_its_group() {
    let beat_dir = tempfile::tempdir().unwrap();
    let beat_file = beat_dir.path().join("beats");
    let (host, _data, _ws) = host_with(&[
        ("FAKE_MODE", "normal".to_string()),
        ("FAKE_SUBSCRIPTIONS", "pre_tool_use".to_string()),
        (
            "FAKE_GRANDCHILD_HEARTBEAT",
            beat_file.to_string_lossy().into_owned(),
        ),
    ]);
    let scope = ProcessScope::new();
    host.set_process_scope(scope.clone());

    // Drives the lazy spawn + handshake, so the sidecar is live from here on.
    let _ = host.invoke(req("pre_tool_use")).await;
    assert_eq!(
        scope.live_count(),
        1,
        "a live sidecar must hold its group Arc so the scope's weak upgrades"
    );
    until(
        Duration::from_secs(10),
        "the grandchild's first beat",
        || beats(&beat_file) > 0,
    )
    .await;
    // Calibrate the detector on the live tree over the same window the stall
    // check below uses, so "stopped growing" cannot pass by simply never having
    // grown.
    let before = beats(&beat_file);
    tokio::time::sleep(STALL_WINDOW).await;
    assert!(
        beats(&beat_file) > before,
        "the grandchild must be beating while the sidecar is alive"
    );

    // The wedged-owner case: nothing disposes the host, the scope reclaims.
    scope.kill_all();

    // The leader died: its stdout closed, so the transport is down and the
    // supervisor reports the plugin unavailable rather than hanging.
    until_transport_closed(&host, Duration::from_secs(10)).await;

    // The grandchild died with it: give the heartbeat a window several periods
    // wide and assert the file stopped growing.
    tokio::time::sleep(SETTLE).await;
    let settled = beats(&beat_file);
    tokio::time::sleep(STALL_WINDOW).await;
    assert_eq!(
        beats(&beat_file),
        settled,
        "the grandchild outlived the group kill: only the leader was reaped"
    );
}

/// A clean reap must hand ownership back: once `dispose` has waited on the
/// leader, the scope no longer references the group, so a later `kill_all`
/// cannot `killpg` whatever now owns that pid.
#[tokio::test]
async fn dispose_releases_the_group_so_kill_all_cannot_reach_a_reused_pid() {
    let (host, _data, _ws) = host_with(&[
        ("FAKE_MODE", "normal".to_string()),
        ("FAKE_SUBSCRIPTIONS", "pre_tool_use".to_string()),
    ]);
    let scope = ProcessScope::new();
    host.set_process_scope(scope.clone());

    let _ = host.invoke(req("pre_tool_use")).await;
    assert_eq!(scope.live_count(), 1);

    host.dispose().await;
    assert_eq!(
        scope.live_count(),
        0,
        "a disposed sidecar must drop its group Arc, killing the scope's weak"
    );
}

/// The close/spawn race: a sidecar starting after its scope was reclaimed has
/// already been killed by `register`, so the start must fail rather than
/// handshake a dead child. The host fails open — the invoke returns an error
/// instead of blocking the operation behind it.
#[tokio::test]
async fn a_sidecar_that_starts_after_the_scope_closed_fails_open() {
    let beat_dir = tempfile::tempdir().unwrap();
    let beat_file = beat_dir.path().join("beats");
    let (host, _data, _ws) = host_with(&[
        ("FAKE_MODE", "normal".to_string()),
        ("FAKE_SUBSCRIPTIONS", "pre_tool_use".to_string()),
        (
            "FAKE_GRANDCHILD_HEARTBEAT",
            beat_file.to_string_lossy().into_owned(),
        ),
    ]);
    let scope = ProcessScope::new();
    host.set_process_scope(scope.clone());
    scope.kill_all();

    let err = host
        .invoke(req("pre_tool_use"))
        .await
        .expect_err("a sidecar cannot start in a closed scope");
    assert!(
        err.to_string().contains("unavailable") || err.to_string().contains("plugin 'p'"),
        "expected a fail-open plugin error, got: {err}"
    );
    assert!(
        !is_running(&host).await,
        "no sidecar may be published from a closed scope"
    );
    assert_eq!(
        scope.live_count(),
        0,
        "a post-close register must not enroll the group"
    );

    // The child was killed on the spot, so its grandchild never got to beat for
    // long: sample twice and require the file to be stalled.
    tokio::time::sleep(SETTLE).await;
    let settled = beats(&beat_file);
    tokio::time::sleep(STALL_WINDOW).await;
    assert_eq!(
        beats(&beat_file),
        settled,
        "the child that lost the close/spawn race left a live process tree"
    );
}
