//! End-to-end supervisor tests over real stdio, using the `fake_sidecar` fixture
//! binary (`CARGO_BIN_EXE_fake_sidecar`) so no real bun/node/deno is required.
//!
//! These cover the full spawn → handshake → JSON-RPC → response-mapping path plus
//! the restart/disable policy; pure-logic pieces (argv, storage, mapping, gates)
//! are unit-tested inside the crate.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use xai_grok_hooks::invoker::{PluginHookInvoker, PluginHookRequest, PluginHookResponse};
use xai_grok_plugin_host::{PluginHost, PluginLaunch, PluginState, RegisteredPlugin, RuntimeKind};

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
        launch: PluginLaunch::Runtime {
            entry: PathBuf::from("/does/not/matter.ts"),
            runtime: RuntimeKind::Auto,
        },
        network: false,
        config: serde_json::json!({ "k": "v" }),
        declared_tools: vec!["echo".to_string()],
        workspace_root: ws.path().to_path_buf(),
        session_id: "sess-1".to_string(),
        leader_socket: None,
    });
    (host, data_dir, ws)
}

fn test_ctx() -> xai_grok_plugin_protocol::ToolCallContextDto {
    xai_grok_plugin_protocol::ToolCallContextDto {
        session_id: "sess-1".into(),
        cwd: "/ws".into(),
        agent: "main".into(),
    }
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
async fn spawn_hardener_runs_with_the_plugin_network_flag() {
    // The seccomp network confinement for `network: false` sidecars is applied
    // through the injected SpawnHardener. Assert it actually runs on the real
    // spawn path, carrying the plugin's network flag. The test hardener only
    // records the flag — it never installs seccomp — so it stays host-agnostic.
    assert_hardener_saw_no_network(PluginLaunch::Runtime {
        entry: PathBuf::from("/does/not/matter.ts"),
        runtime: RuntimeKind::Auto,
    })
    .await;
}

#[tokio::test]
async fn spawn_hardener_runs_for_a_directly_executed_plugin_too() {
    // `network: false` is a manifest-level guarantee, and the manifest does not
    // know what language the plugin is written in. The hardener is keyed on the
    // network flag alone, so a directly-executed program must be confined on
    // exactly the same path a TS sidecar is — otherwise the command form would
    // silently downgrade a promise the manifest already makes.
    assert_hardener_saw_no_network(PluginLaunch::Command {
        program: PathBuf::from("/does/not/matter"),
        args: vec!["--serve".to_string()],
    })
    .await;
}

/// Drive one plugin through spawn + handshake and assert the injected hardener
/// ran, seeing `network == false`. The command factory ignores the launch form
/// (it always spawns the fake sidecar), which is the point: the assertion is
/// about the hardener seam, not about argv construction.
async fn assert_hardener_saw_no_network(launch: PluginLaunch) {
    let data_dir = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_fake_sidecar");
    let factory = Box::new(move |_spec: &RegisteredPlugin| {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.env("FAKE_MODE", "normal");
        cmd.env("FAKE_SUBSCRIPTIONS", "pre_tool_use");
        Ok(cmd)
    });
    let seen = Arc::new(Mutex::new(Vec::<bool>::new()));
    let rec = Arc::clone(&seen);
    let mut host = PluginHost::new_for_test(
        data_dir.path().to_path_buf(),
        factory,
        Duration::from_millis(10),
    );
    host.set_spawn_hardener(Arc::new(move |_cmd, network| {
        rec.lock().unwrap().push(network);
        Ok(())
    }));
    host.register_plugin(RegisteredPlugin {
        name: "p".to_string(),
        launch,
        network: false,
        config: serde_json::json!({}),
        declared_tools: vec![],
        workspace_root: ws.path().to_path_buf(),
        session_id: "sess-1".to_string(),
        leader_socket: None,
    });
    // Drive the spawn → handshake path so the hardener is exercised.
    let _ = host.invoke(req("pre_tool_use", 5000)).await;
    host.dispose().await;
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "spawn hardener never ran");
    assert!(
        seen.iter().all(|&network| !network),
        "hardener saw network=true for a network:false plugin: {seen:?}"
    );
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
async fn replace_gate_maps_replace_payload() {
    // A Replace-gate event (`provider_request`) whose sidecar returns a `replace`
    // result must map to `PluginHookResponse::Replace` with the substitute payload
    // (and the payload the host forwarded round-trips back inside it).
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "replace_payload".into()),
            ("FAKE_SUBSCRIPTIONS", "provider_request".into()),
        ],
        Duration::from_millis(10),
    );

    let resp = host.invoke(req("provider_request", 5000)).await.unwrap();
    match resp {
        PluginHookResponse::Replace { payload } => {
            let payload = payload.expect("substitute payload present");
            assert_eq!(payload["replaced"], serde_json::json!(true));
            // The fixture echoes the forwarded request payload back.
            assert_eq!(payload["saw"], serde_json::json!({ "tool": "bash" }));
        }
        other => panic!("expected Replace, got {other:?}"),
    }

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
    // NotSubscribed proves the host short-circuited before sending one.
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "crash_on_invoke".into()),
            ("FAKE_SUBSCRIPTIONS", "session_start".into()),
        ],
        Duration::from_millis(10),
    );

    let resp = host.invoke(req("pre_tool_use", 5000)).await.unwrap();
    assert!(matches!(resp, PluginHookResponse::NotSubscribed));

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
async fn tool_invoke_round_trips_with_call_context() {
    let (host, _d, _w) = host_with(&[("FAKE_MODE", "normal".into())], Duration::from_millis(10));

    let result = host
        .invoke_tool(
            "p",
            "echo",
            serde_json::json!({ "text": "hi" }),
            xai_grok_plugin_protocol::ToolCallContextDto {
                session_id: "sess-9".into(),
                cwd: "/work/dir".into(),
                agent: "main".into(),
            },
            5_000,
        )
        .await
        .unwrap();

    assert!(!result.is_error);
    // The fixture echoes every per-call context field back into the content.
    assert!(result.content.contains("tool=echo"), "{}", result.content);
    assert!(
        result.content.contains(r#""text":"hi""#),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("session=sess-9"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("cwd=/work/dir"),
        "{}",
        result.content
    );
    assert!(result.content.contains("agent=main"), "{}", result.content);

    host.dispose().await;
}

#[tokio::test]
async fn tool_invoke_maps_is_error_through() {
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "normal".into()),
            ("FAKE_TOOL_MODE", "error".into()),
        ],
        Duration::from_millis(10),
    );

    let result = host
        .invoke_tool("p", "echo", serde_json::json!({}), test_ctx(), 5_000)
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("failed on purpose"));

    host.dispose().await;
}

#[tokio::test]
async fn tool_invoke_times_out_without_counting_a_crash() {
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "normal".into()),
            ("FAKE_TOOL_MODE", "hang".into()),
        ],
        Duration::from_millis(10),
    );

    let err = host
        .invoke_tool("p", "echo", serde_json::json!({}), test_ctx(), 150)
        .await
        .unwrap_err();
    assert!(err.message.contains("timed out"), "got: {}", err.message);

    // A slow tool is not a crash: no restart/backoff pressure.
    let status = host.status().await;
    assert_eq!(status[0].state, PluginState::Running);
    assert_eq!(status[0].consecutive_crashes, 0);

    host.dispose().await;
}

/// Abandon-on-abort: when the `invoke_tool` future is dropped before the call
/// resolves (the parent turn was aborted mid-tool-call), the host fires a
/// `tool_cancel` notification to the plugin so its handler can wind down. The
/// fixture records the cancelled invocation via storage; assert it lands.
#[tokio::test]
async fn dropping_tool_invoke_notifies_plugin_tool_cancel() {
    let (host, data_dir, _w) = host_with(
        &[
            ("FAKE_MODE", "normal".into()),
            ("FAKE_TOOL_MODE", "hang_record_cancel".into()),
        ],
        Duration::from_millis(10),
    );

    // The fixture never replies; drop the future (as the aborted turn task
    // would) by letting a short timeout elapse. That fires the abort guard.
    let dropped = tokio::time::timeout(
        Duration::from_millis(400),
        host.invoke_tool("p", "echo", serde_json::json!({}), test_ctx(), 5_000),
    )
    .await;
    assert!(
        dropped.is_err(),
        "hang fixture should keep the call pending"
    );

    // The plugin recorded the cancelled invocation (plugin→core storage_set,
    // served by the still-live host). Poll the on-disk store briefly.
    let store = data_dir.path().join("p.json");
    let mut seen = None;
    for _ in 0..50 {
        if let Ok(bytes) = std::fs::read(&store)
            && let Ok(map) =
                serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&bytes)
            && let Some(v) = map.get("tool_cancel_seen")
        {
            seen = Some(v.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen = seen.expect("plugin should record the tool_cancel invocation id");
    assert!(
        seen.as_str().is_some_and(|s| s.starts_with("tinv-")),
        "recorded invocation id should be the host's tool_invoke id: {seen}"
    );

    // A cancelled (abandoned) call is not a crash.
    let status = host.status().await;
    assert_eq!(status[0].consecutive_crashes, 0);

    host.dispose().await;
}

#[tokio::test]
async fn tool_invoke_sidecar_crash_is_an_error_not_a_hang() {
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "normal".into()),
            ("FAKE_TOOL_MODE", "crash".into()),
        ],
        Duration::from_millis(10),
    );

    let err = host
        .invoke_tool("p", "echo", serde_json::json!({}), test_ctx(), 5_000)
        .await
        .unwrap_err();
    assert!(err.message.contains("crashed"), "got: {}", err.message);

    // The crash feeds the normal supervisor policy.
    let status = host.status().await;
    assert_eq!(status[0].consecutive_crashes, 1);
}

#[tokio::test]
async fn tool_invoke_serves_counter_storage_rpcs_mid_call() {
    // The fixture issues storage_set + storage_get (plugin→core) while its
    // tool_invoke reply (core→plugin) is still pending — the round trip only
    // completes if the host's rpc loop serves both directions concurrently.
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "normal".into()),
            ("FAKE_TOOL_MODE", "storage_probe".into()),
        ],
        Duration::from_millis(10),
    );

    let result = host
        .invoke_tool(
            "p",
            "echo",
            serde_json::json!({ "n": 42 }),
            test_ctx(),
            5_000,
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(
        result.content.contains(r#"stored-and-loaded: {"n":42}"#),
        "{}",
        result.content
    );

    host.dispose().await;
}

#[tokio::test]
async fn concurrent_tool_and_hook_invokes_share_one_sidecar() {
    // Several in-flight core→plugin requests at once: two tool calls plus a
    // hook invoke, all correlated over the same transport. Exercises the
    // pending-map plumbing (distinct ids, out-of-order-safe completion).
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "normal".into()),
            ("FAKE_SUBSCRIPTIONS", "pre_tool_use".into()),
        ],
        Duration::from_millis(10),
    );

    let t1 = host.invoke_tool(
        "p",
        "echo",
        serde_json::json!({ "i": 1 }),
        test_ctx(),
        5_000,
    );
    let t2 = host.invoke_tool(
        "p",
        "echo",
        serde_json::json!({ "i": 2 }),
        test_ctx(),
        5_000,
    );
    let h = host.invoke(req("pre_tool_use", 5_000));
    let (r1, r2, hr) = tokio::join!(t1, t2, h);

    let r1 = r1.unwrap();
    let r2 = r2.unwrap();
    assert!(r1.content.contains(r#""i":1"#), "{}", r1.content);
    assert!(r2.content.contains(r#""i":2"#), "{}", r2.content);
    assert!(matches!(
        hr.unwrap(),
        PluginHookResponse::Decision { allow: false, .. }
    ));

    let status = host.status().await;
    assert_eq!(status[0].state, PluginState::Running);
    assert_eq!(status[0].consecutive_crashes, 0);

    host.dispose().await;
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
    assert!(matches!(resp, PluginHookResponse::Observed { .. }));

    let status = host.status().await;
    assert_eq!(status[0].state, PluginState::Running);

    host.dispose().await;
}

/// An `observed` reply may carry model-facing text: the host must map
/// `additional_context` through instead of flattening it to a bare ack.
#[tokio::test]
async fn observed_reply_carries_additional_context() {
    let (host, _d, _w) = host_with(
        &[
            ("FAKE_MODE", "observe_context".into()),
            ("FAKE_OBSERVE_CONTEXT", "scratch dir: /tmp/s".into()),
            ("FAKE_SUBSCRIPTIONS", "session_start".into()),
        ],
        Duration::from_millis(10),
    );

    let resp = host.invoke(req("session_start", 5000)).await.unwrap();
    assert!(
        matches!(
            &resp,
            PluginHookResponse::Observed { additional_context }
                if additional_context.as_deref() == Some("scratch dir: /tmp/s")
        ),
        "expected observed with context, got {resp:?}"
    );

    host.dispose().await;
}

#[tokio::test]
async fn a_hardener_that_cannot_confine_fails_the_start() {
    // `network: false` is a promise the manifest makes to the user, not a
    // best-effort. When the platform has nothing to enforce it with, the
    // hardener says so and the sidecar must not run: a plugin that quietly
    // gains the network it was declared not to have is the failure mode this
    // seam exists to prevent.
    let data_dir = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_fake_sidecar");
    let factory = Box::new(move |_spec: &RegisteredPlugin| {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.env("FAKE_MODE", "normal");
        cmd.env("FAKE_SUBSCRIPTIONS", "pre_tool_use");
        Ok(cmd)
    });
    let mut host = PluginHost::new_for_test(
        data_dir.path().to_path_buf(),
        factory,
        Duration::from_millis(10),
    );
    host.set_spawn_hardener(Arc::new(|_cmd, network| {
        if network {
            Ok(())
        } else {
            Err("no per-child network confinement here".to_string())
        }
    }));
    host.register_plugin(RegisteredPlugin {
        name: "p".to_string(),
        launch: PluginLaunch::Command {
            program: PathBuf::from("/does/not/matter"),
            args: vec![],
        },
        network: false,
        config: serde_json::json!({}),
        declared_tools: vec![],
        workspace_root: ws.path().to_path_buf(),
        session_id: "sess-1".to_string(),
        leader_socket: None,
    });
    assert!(
        host.invoke(req("pre_tool_use", 5000)).await.is_err(),
        "an unconfinable network:false sidecar must not serve invokes"
    );
    let status = host.status().await;
    assert_ne!(status[0].state, PluginState::Running);
    host.dispose().await;
}

#[test]
fn wrap_command_prepends_the_wrapper_and_keeps_argv_cwd_and_env() {
    // The macOS confinement replaces the program with `sandbox-exec`, so the
    // plugin's own argv has to survive being pushed one level down — including
    // the leader socket env and the workspace cwd `build_command` set.
    let mut cmd = tokio::process::Command::new("/usr/bin/plugin");
    cmd.args(["--serve", "-p"]);
    cmd.current_dir("/ws");
    cmd.env("GROK_LEADER_SOCKET", "/run/leader.sock");

    xai_grok_plugin_host::wrap_command(
        &mut cmd,
        std::path::Path::new("/usr/bin/sandbox-exec"),
        &[
            std::ffi::OsString::from("-p"),
            std::ffi::OsString::from("(deny network*)"),
            std::ffi::OsString::from("--"),
        ],
    );

    let std_cmd = cmd.as_std();
    assert_eq!(std_cmd.get_program(), "/usr/bin/sandbox-exec");
    let args: Vec<_> = std_cmd.get_args().collect();
    assert_eq!(
        args,
        vec![
            "-p",
            "(deny network*)",
            "--",
            "/usr/bin/plugin",
            "--serve",
            "-p"
        ]
    );
    assert_eq!(std_cmd.get_current_dir(), Some(std::path::Path::new("/ws")));
    let envs: Vec<_> = std_cmd.get_envs().collect();
    assert_eq!(
        envs,
        vec![(
            std::ffi::OsStr::new("GROK_LEADER_SOCKET"),
            Some(std::ffi::OsStr::new("/run/leader.sock"))
        )]
    );
}
