//! Extension methods that resolve a path must answer about the root of the
//! session they were given, not the directory the leader happens to run in.
//!
//! One leader hosts sessions rooted in several trees. A request that leaves the
//! root to the leader — a client-supplied `cwd`, or `std::env::current_dir()` —
//! reads and writes against a tree the user never named.

use agent_client_protocol as acp;
use xai_grok_test_support::EnvGuard;

use super::{build_minimal_agent_for_tests, make_test_handle};
use crate::session::info::Info;

/// Plant `<root>/.grok/skills/<name>/SKILL.md` and return the root.
fn plant_skill(root: &std::path::Path, name: &str) {
    let dir = root.join(".grok").join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: A skill in this root only\n---\n\nBody.\n"),
    )
    .unwrap();
}

async fn ext_result(
    agent: &crate::agent::mvp_agent::MvpAgent,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    use acp::Agent as _;
    let raw = serde_json::value::to_raw_value(&params).unwrap();
    let resp = agent
        .ext_method(acp::ExtRequest::new(method, raw.into()))
        .await
        .unwrap_or_else(|e| panic!("{method} must be answered: {e:?}"));
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    wrapper
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("{method} returned no result: {wrapper}"))
}

/// A `skills/list` naming a session must answer about that session's root even
/// when the request's `cwd` names another one.
///
/// `cwd` is a claim the shell cannot check, and `skills/toggle` validates the
/// skill name against the set it produces before writing the global
/// `[skills].disabled` list. Every other list-style extension method
/// (`hooks/list`, `plugins/list`, `mcp/list`) already resolves the root itself.
///
/// Serial because skill discovery reads `HOME`/`GROK_HOME` and this test has to
/// point them at an empty tree.
#[tokio::test]
#[serial_test::serial]
async fn skills_list_prefers_the_named_session_over_the_requests_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvGuard::set("HOME", &home);
    let _userprofile = EnvGuard::set("USERPROFILE", &home);
    let _grok = EnvGuard::set("GROK_HOME", &home);

    let root_a = tmp.path().join("a");
    let root_b = tmp.path().join("b");
    plant_skill(&root_a, "alpha-skill");
    plant_skill(&root_b, "beta-skill");

    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("skills-root-sess");
    let mut handle = make_test_handle("test-model", false, None);
    handle.info = Info {
        id: sid.clone(),
        cwd: root_b.to_string_lossy().into_owned(),
    };
    agent.insert_resident(&sid, handle);

    // The request disagrees with the session on purpose: the session wins.
    let result = ext_result(
        &agent,
        "x.ai/skills/list",
        serde_json::json!({
            "sessionId": sid.0.as_ref(),
            "cwd": root_a.to_string_lossy(),
        }),
    )
    .await;

    let names: Vec<String> = result["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .filter_map(|s| s["name"].as_str().map(str::to_string))
        .collect();
    assert!(
        names.iter().any(|n| n == "beta-skill"),
        "skills/list must answer about the session's root, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "alpha-skill"),
        "the request's cwd must not be able to redirect the answer, got {names:?}"
    );
}

/// A session named but not resident is refused rather than answered from the
/// leader's directory: guessing is how a toggle lands on the wrong root.
#[tokio::test]
async fn skills_list_refuses_an_unknown_session() {
    let agent = build_minimal_agent_for_tests();
    use acp::Agent as _;
    let params = serde_json::json!({ "sessionId": "no-such-session", "cwd": "/roots/a" });
    let raw = serde_json::value::to_raw_value(&params).unwrap();
    let err = agent
        .ext_method(acp::ExtRequest::new("x.ai/skills/list", raw.into()))
        .await
        .expect_err("an unknown session must not be answered from the leader's cwd");
    assert_eq!(
        err.code,
        acp::Error::resource_not_found(None::<String>).code
    );
}

/// A client that predates `sessionId` still gets its `cwd` honoured; the wire
/// change must not turn a resolvable request into a hard failure.
#[tokio::test]
#[serial_test::serial]
async fn skills_list_without_a_session_still_honours_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvGuard::set("HOME", &home);
    let _userprofile = EnvGuard::set("USERPROFILE", &home);
    let _grok = EnvGuard::set("GROK_HOME", &home);

    let root_a = tmp.path().join("a");
    plant_skill(&root_a, "legacy-skill");

    let agent = build_minimal_agent_for_tests();
    let result = ext_result(
        &agent,
        "x.ai/skills/list",
        serde_json::json!({ "cwd": root_a.to_string_lossy() }),
    )
    .await;

    let names: Vec<String> = result["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .filter_map(|s| s["name"].as_str().map(str::to_string))
        .collect();
    assert!(
        names.iter().any(|n| n == "legacy-skill"),
        "a sessionless request keeps the old contract, got {names:?}"
    );
}
/// `marketplace add ./x` names a directory in the user's session, not in the
/// leader's launch directory. The assertion is on the rejected path rather than
/// on a successful add so the test never writes the global source list.
///
/// Serial because the add consults the managed-settings tier, which resolves
/// `~/.claude` from `HOME` on every call; this test points it at an empty tree.
/// `GROK_HOME` is set for the same reason but does not carry the config probe:
/// `readonly_config_notice` resolves `config.toml` through the process-cached
/// `grok_home()`, so before the pre-main pin in `test_support` this case read
/// whichever home won the race — on a machine with a declaratively generated
/// `~/.grok/config.toml` that is a read-only file, and the add came back
/// `unsupported` instead of the `validation_error` asserted below.
#[tokio::test]
#[serial_test::serial]
async fn marketplace_add_resolves_a_relative_source_against_the_session_root() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvGuard::set("HOME", &home);
    let _userprofile = EnvGuard::set("USERPROFILE", &home);
    let _grok = EnvGuard::set("GROK_HOME", &home);

    let root_b = tmp.path().join("b");
    std::fs::create_dir_all(&root_b).unwrap();

    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("marketplace-root-sess");
    let mut handle = make_test_handle("test-model", false, None);
    handle.info = Info {
        id: sid.clone(),
        cwd: root_b.to_string_lossy().into_owned(),
    };
    agent.insert_resident(&sid, handle);

    let result = ext_result(
        &agent,
        "x.ai/marketplace/action",
        serde_json::json!({
            "sessionId": sid.0.as_ref(),
            "action": { "type": "add_source", "url": "./no-such-marketplace" },
        }),
    )
    .await;

    assert_eq!(result["status"], "validation_error", "{result}");
    let expected = root_b.join("no-such-marketplace");
    let message = result["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&expected.to_string_lossy().into_owned()),
        "the source must resolve against the session's root, got {message:?}"
    );
}

/// Names of the entries `x.ai/fs/list` returned, for order-insensitive asserts.
fn listed_names(result: &serde_json::Value) -> Vec<String> {
    result["nodes"]
        .as_array()
        .unwrap_or_else(|| panic!("fs/list must return nodes: {result}"))
        .iter()
        .filter_map(|n| n["name"].as_str().map(str::to_owned))
        .collect()
}

/// A client can enumerate a directory the leader was not launched in.
///
/// This is what lets a client offer a directory to start a session in: without
/// it the only roots reachable are the ones some earlier terminal already
/// opened. The walk is a plain local walk over the resolved absolute path
/// (`session::file_system::list`); the process-wide `WorkspaceOps` is consulted
/// only by `extensions::fs::confine_local`, whose confinement is off in local
/// mode (`WorkspaceHandle::new_minimal` sets `confine_fs_to_workspace_root:
/// false`), so it returns the path untouched and no walk root.
///
/// Serial for the same reason as the cases above: the first `fs` call builds
/// the local workspace handle, which resolves launch-dir trust out of `HOME`.
#[tokio::test]
#[serial_test::serial]
async fn fs_list_enumerates_a_directory_outside_the_leaders_launch_root() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvGuard::set("HOME", &home);
    let _userprofile = EnvGuard::set("USERPROFILE", &home);
    let _grok = EnvGuard::set("GROK_HOME", &home);

    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("src")).unwrap();
    std::fs::write(elsewhere.join("Cargo.toml"), "[package]\n").unwrap();

    let launch_cwd = std::env::current_dir().unwrap();
    assert!(
        !elsewhere.starts_with(&launch_cwd),
        "the fixture must lie outside the launch dir to prove anything"
    );

    let agent = build_minimal_agent_for_tests();
    let result = ext_result(
        &agent,
        "x.ai/fs/list",
        serde_json::json!({ "path": elsewhere.to_string_lossy() }),
    )
    .await;

    let names = listed_names(&result);
    assert!(
        names.iter().any(|n| n == "Cargo.toml") && names.iter().any(|n| n == "src"),
        "a directory outside the launch root must enumerate, got {names:?}"
    );
}

/// A relative `fs/list` answers about the named session's root, not the
/// leader's launch dir — the same rule the rest of this file pins.
#[tokio::test]
#[serial_test::serial]
async fn fs_list_resolves_a_relative_path_against_the_named_session() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvGuard::set("HOME", &home);
    let _userprofile = EnvGuard::set("USERPROFILE", &home);
    let _grok = EnvGuard::set("GROK_HOME", &home);

    let root_b = tmp.path().join("b");
    std::fs::create_dir_all(&root_b).unwrap();
    std::fs::write(root_b.join("only-in-b.txt"), "b\n").unwrap();

    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("fs-list-root-sess");
    let mut handle = make_test_handle("test-model", false, None);
    handle.info = Info {
        id: sid.clone(),
        cwd: root_b.to_string_lossy().into_owned(),
    };
    agent.insert_resident(&sid, handle);

    let result = ext_result(
        &agent,
        "x.ai/fs/list",
        serde_json::json!({ "sessionId": sid.0.as_ref(), "path": "." }),
    )
    .await;

    let names = listed_names(&result);
    assert!(
        names.iter().any(|n| n == "only-in-b.txt"),
        "a relative list must walk the session's root, got {names:?}"
    );
}

/// An agent whose plugin discovery finds one plugin from any root, so a
/// memoized per-root registry is `Some` and can be compared by pointer.
#[cfg(unix)]
fn agent_with_cli_plugin(plugin_dir: &std::path::Path) -> crate::agent::mvp_agent::MvpAgent {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, GrokComConfig};
    let auth_home = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(auth_home.path(), GrokComConfig::default()));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = xai_acp_lib::AcpAgentGatewaySender::new(tx);
    let mut cfg = AgentConfig::default();
    cfg.plugins.cli_plugin_dirs = vec![plugin_dir.to_path_buf()];
    crate::agent::mvp_agent::MvpAgent::new(gateway, &cfg, auth_manager, None)
        .expect("valid test config")
}

/// Regression: a client may open a tree under a symlink while another session
/// holds it under its real path. Those are two roots to the config watcher,
/// which watches the path it was handed, and one root to the plugin registry,
/// which canonicalizes its key — so closing one of them must hand back that
/// spelling's watches while leaving the registry the other session still reads.
///
/// Comparing the raw cwds for both, as the release path did, dropped the shared
/// registry on the first close and the surviving session silently rebuilt a
/// different one.
///
/// Serial because plugin discovery and folder trust resolve out of `HOME`.
#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn closing_one_spelling_keeps_the_root_a_live_session_still_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvGuard::set("HOME", &home);
    let _userprofile = EnvGuard::set("USERPROFILE", &home);
    let _grok = EnvGuard::set("GROK_HOME", &home);

    let real = tmp.path().join("tree");
    std::fs::create_dir_all(&real).unwrap();
    let link = tmp.path().join("tree-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let plugin_dir = tmp.path().join("plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.json"),
        r#"{"name": "root-release-plugin"}"#,
    )
    .unwrap();

    let mut agent = agent_with_cli_plugin(&plugin_dir);
    let (watch_tx, mut watch_rx) = tokio::sync::mpsc::unbounded_channel();
    agent.set_config_watcher_path_tx(watch_tx);

    for (id, cwd) in [("root-real", &real), ("root-link", &link)] {
        let sid = acp::SessionId::new(id);
        let mut handle = make_test_handle("test-model", false, None);
        handle.info = Info {
            id: sid.clone(),
            cwd: cwd.to_string_lossy().into_owned(),
        };
        agent.insert_resident(&sid, handle);
    }

    let held = agent
        .plugin_registry_for_root(&real)
        .expect("the cli plugin dir must give this root a registry");

    agent.take_session(&acp::SessionId::new("root-link"));

    assert!(
        agent
            .plugin_registry_for_root(&real)
            .is_some_and(|now| std::sync::Arc::ptr_eq(&held, &now)),
        "the real-path session still holds this tree, so its registry must survive"
    );
    assert_eq!(
        watch_rx.try_recv().ok(),
        Some(crate::config::watcher::ConfigWatchRequest::Unwatch(
            link.clone()
        )),
        "the closed spelling's own watch pair must still be handed back"
    );

    agent.take_session(&acp::SessionId::new("root-real"));
    assert!(
        agent
            .plugin_registry_for_root(&real)
            .is_some_and(|rebuilt| !std::sync::Arc::ptr_eq(&held, &rebuilt)),
        "the last session leaving the tree must release the memoized registry"
    );
}
