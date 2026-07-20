//! End-to-end supervisor tests over real stdio, using the `fake_sidecar` fixture
//! binary (`CARGO_BIN_EXE_fake_sidecar`) so no real bun/node/deno is required.
//!
//! These cover the full spawn → handshake → JSON-RPC → response-mapping path plus
//! the restart/disable policy; pure-logic pieces (argv, storage, mapping, gates)
//! are unit-tested inside the crate.

use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;
use xai_grok_hooks::invoker::{PluginHookInvoker, PluginHookRequest, PluginHookResponse};
use xai_grok_plugin_host::{PluginHost, PluginState, RegisteredPlugin, RuntimeKind};

/// Build a host whose sidecars are the fixture binary configured via env, plus a
/// registered plugin named `p`. Returns the host and the temp dirs (kept alive).
fn host_with(env: &[(&'static str, String)], backoff: Duration) -> (PluginHost, TempDir, TempDir) {
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
    let host = PluginHost::new_for_test(data_dir.path().to_path_buf(), factory, backoff);
    host.register_plugin(RegisteredPlugin {
        name: "p".to_string(),
        entry: PathBuf::from("/does/not/matter.ts"),
        runtime: RuntimeKind::Auto,
        network: false,
        config: serde_json::json!({ "k": "v" }),
        workspace_root: ws.path().to_path_buf(),
        session_id: "sess-1".to_string(),
    });
    (host, data_dir, ws)
}

fn req(event: &str, timeout_ms: u64) -> PluginHookRequest {
    PluginHookRequest {
        plugin: "p".to_string(),
        handler: event.to_string(),
        event: event.to_string(),
        payload: serde_json::json!({ "tool": "bash" }),
        timeout_ms,
    }
}

#[tokio::test]
async fn handshake_ok_routes_and_maps_results() {
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "normal".into()),
            ("FAKE_SUBSCRIPTIONS", "pre_tool_use,stop".into()),
            ("FAKE_PLUGIN_VERSION", "0.9.1".into()),
        ],
        Duration::from_millis(10),
    );

    // Tool gate -> Decision (deny + reason).
    let resp = host.invoke(req("pre_tool_use", 5000)).await.unwrap();
    match resp {
        PluginHookResponse::Decision { allow, reason } => {
            assert!(!allow);
            assert_eq!(reason.as_deref(), Some("fixture-deny"));
        }
        other => panic!("expected Decision, got {other:?}"),
    }

    // Stop gate -> Stop (block + additional_context).
    let resp = host.invoke(req("stop", 5000)).await.unwrap();
    match resp {
        PluginHookResponse::Stop {
            block,
            additional_context,
            ..
        } => {
            assert!(block);
            assert_eq!(additional_context.as_deref(), Some("fixture-ctx"));
        }
        other => panic!("expected Stop, got {other:?}"),
    }

    let status = host.status().await;
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].state, PluginState::Running);
    assert_eq!(status[0].plugin_version.as_deref(), Some("0.9.1"));
    assert_eq!(status[0].consecutive_crashes, 0);

    host.dispose().await;
}

#[tokio::test]
async fn protocol_version_mismatch_disables_plugin() {
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "normal".into()),
            ("FAKE_PROTOCOL_VERSION", "2".into()),
            ("FAKE_SUBSCRIPTIONS", "pre_tool_use".into()),
        ],
        Duration::from_millis(10),
    );

    let err = host.invoke(req("pre_tool_use", 5000)).await.unwrap_err();
    assert!(
        err.message.contains("version mismatch"),
        "got: {}",
        err.message
    );

    let status = host.status().await;
    assert_eq!(status[0].state, PluginState::Disabled);

    // Subsequent invokes stay disabled (no retry).
    let err = host.invoke(req("pre_tool_use", 5000)).await.unwrap_err();
    assert!(err.message.contains("disabled"), "got: {}", err.message);
}

#[tokio::test]
async fn unsubscribed_event_short_circuits_without_rpc() {
    // The fixture would crash if it ever received a hook_invoke, so a returned
    // Observed proves the host short-circuited before sending one.
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "crash_on_invoke".into()),
            ("FAKE_SUBSCRIPTIONS", "session_start".into()),
        ],
        Duration::from_millis(10),
    );

    let resp = host.invoke(req("pre_tool_use", 5000)).await.unwrap();
    assert!(matches!(resp, PluginHookResponse::Observed));

    // Still alive: the sidecar never got the crashing invoke.
    let status = host.status().await;
    assert_eq!(status[0].state, PluginState::Running);
    assert_eq!(status[0].consecutive_crashes, 0);

    host.dispose().await;
}

#[tokio::test]
async fn subagent_end_alias_still_receives_subagent_stop() {
    // The plugin subscribes under the wire alias `subagent_end`; the runner fires
    // the canonical `subagent_stop`. The event must still be delivered (a Stop
    // reply), not short-circuited to Observed.
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "normal".into()),
            ("FAKE_SUBSCRIPTIONS", "subagent_end".into()),
        ],
        Duration::from_millis(10),
    );

    let resp = host.invoke(req("subagent_stop", 5000)).await.unwrap();
    assert!(
        matches!(resp, PluginHookResponse::Stop { block: true, .. }),
        "alias subscription should deliver the event, got {resp:?}"
    );

    // Status shows the declared spelling, not the canonicalized one.
    let status = host.status().await;
    assert_eq!(status[0].subscriptions, vec!["subagent_end".to_string()]);

    host.dispose().await;
}

#[tokio::test]
async fn slow_plugin_times_out_without_counting_a_crash() {
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "hang_on_invoke".into()),
            ("FAKE_SUBSCRIPTIONS", "pre_tool_use".into()),
        ],
        Duration::from_millis(10),
    );

    let err = host.invoke(req("pre_tool_use", 120)).await.unwrap_err();
    assert!(err.message.contains("timed out"), "got: {}", err.message);

    // A timeout is not a crash: the sidecar stays alive and undisabled.
    let status = host.status().await;
    assert_eq!(status[0].state, PluginState::Running);
    assert_eq!(status[0].consecutive_crashes, 0);

    host.dispose().await;
}

#[tokio::test]
async fn crash_restarts_then_disables_after_three() {
    let backoff = Duration::from_millis(10);
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "crash_on_invoke".into()),
            ("FAKE_SUBSCRIPTIONS", "pre_tool_use".into()),
        ],
        backoff,
    );

    for expected_crashes in 1..=3 {
        // Each invoke starts a fresh sidecar, handshakes, then the fixture exits
        // on receiving hook_invoke -> transport closes -> counted as a crash.
        let err = host.invoke(req("pre_tool_use", 5000)).await.unwrap_err();
        assert!(
            err.message.contains("transport closed") || err.message.contains("disabled"),
            "crash {expected_crashes}: {}",
            err.message
        );
        let status = host.status().await;
        assert_eq!(
            status[0].consecutive_crashes, expected_crashes,
            "crash count after invoke {expected_crashes}"
        );
        // Wait out the backoff before the next restart attempt.
        tokio::time::sleep(backoff * 4).await;
    }

    // Third crash trips the disable threshold.
    let status = host.status().await;
    assert_eq!(status[0].state, PluginState::Disabled);

    // Further invokes are refused outright (disabled), never restarting.
    let err = host.invoke(req("pre_tool_use", 5000)).await.unwrap_err();
    assert!(err.message.contains("disabled"), "got: {}", err.message);
}

#[tokio::test]
async fn plugin_to_core_storage_round_trips_over_the_wire() {
    // The fixture drives storage_set/get/list/delete + log_emit against the host's
    // capability server during the invoke, asserting internally; if any step
    // failed it would panic and close the transport, so a mapped Observed proves
    // the whole plugin->core path worked end to end.
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "storage_probe".into()),
            ("FAKE_SUBSCRIPTIONS", "pre_tool_use".into()),
        ],
        Duration::from_millis(10),
    );

    let resp = host.invoke(req("pre_tool_use", 5000)).await.unwrap();
    assert!(matches!(resp, PluginHookResponse::Observed));

    let status = host.status().await;
    assert_eq!(status[0].state, PluginState::Running);

    host.dispose().await;
}
