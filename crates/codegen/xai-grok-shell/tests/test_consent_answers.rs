//! `set_consent_answer` monotonicity, in its own test binary.
//!
//! It lived in the lib's unit tests, where `EnvGuard::set("GROK_HOME", …)`
//! cannot isolate it: `grok_home()` caches in a process-wide `OnceLock`, so
//! whichever test in the shared binary resolves it first decides for all of
//! them. Losing that race pointed these writes at the developer's real
//! `~/.grok/config.toml` — the test passed by overwriting it. A dedicated
//! binary is the only place the env guard actually holds.

use std::sync::OnceLock;

use xai_grok_shell::util::config::{load_config_from_toml, set_consent_answer};

/// `GROK_HOME` for this binary, claimed before anything resolves it.
fn test_home() -> &'static std::path::PathBuf {
    static HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let path = tempfile::TempDir::new().unwrap().keep();
        // SAFETY: set once at init, before any other thread reads the env.
        unsafe { std::env::set_var("GROK_HOME", &path) };
        path
    })
}

#[tokio::test]
async fn set_consent_answer_is_monotonic_per_account() {
    let _home = test_home();

    let answers = || {
        let root = xai_grok_shell::config::load_from_disk().expect("read config");
        load_config_from_toml(&root).consent.answers
    };

    set_consent_answer(Some("a@example.com".into()), "tos".into(), 3, false)
        .await
        .expect("first answer");
    set_consent_answer(Some("a@example.com".into()), "tos".into(), 1, false)
        .await
        .expect("replayed answer");
    assert_eq!(
        answers()["tos"].version,
        3,
        "a stale replay must not lower the record",
    );

    set_consent_answer(Some("a@example.com".into()), "tos".into(), 4, true)
        .await
        .expect("server ack");
    assert!(answers()["tos"].acked, "the ack must reach the record");

    set_consent_answer(Some("a@example.com".into()), "tos".into(), 1, false)
        .await
        .expect("replay after the ack");
    let entry = answers()["tos"].clone();
    assert_eq!(entry.version, 4);
    assert!(
        entry.acked,
        "a replay must not unset the ack it did not make"
    );

    // The local write and the server ack race for one version, and the local one carries `false`.
    set_consent_answer(Some("a@example.com".into()), "tos".into(), 4, false)
        .await
        .expect("the slower local write");
    assert!(
        answers()["tos"].acked,
        "the slower writer must not retract the ack"
    );

    set_consent_answer(Some("b@example.com".into()), "tos".into(), 1, false)
        .await
        .expect("second account");
    let entry = answers()["tos"].clone();
    assert_eq!(entry.version, 1, "a different account starts over");
    assert_eq!(entry.account.as_deref(), Some("b@example.com"));
    assert!(
        !entry.acked,
        "the ack belongs to the answer it was made for"
    );

    set_consent_answer(None, "tos".into(), 2, false)
        .await
        .expect("signed-out answer");
    assert_eq!(
        answers()["tos"].account,
        None,
        "a signed-out answer must not read back as the previous account",
    );

    set_consent_answer(Some("b@example.com".into()), "aup".into(), 2, false)
        .await
        .expect("second notice");
    assert_eq!(
        answers().len(),
        2,
        "answering a second notice must not evict the first",
    );
}
