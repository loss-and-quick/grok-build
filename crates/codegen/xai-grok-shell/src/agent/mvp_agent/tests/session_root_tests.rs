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
