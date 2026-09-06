//! Regression test: a settings write must be declined, not performed, when
//! `~/.grok/config.toml` is read-only.
//!
//! Bug: `save_config` replaced the file with a temp file plus `rename`.
//! `rename` is permission-checked against the directory, and `~/.grok` is
//! writable, so a `-r--r--r--` config installed by home-manager was replaced
//! anyway — and since the replacement inherited the old mode, the file looked
//! untouched afterwards. The next `home-manager switch` reported a config the
//! user was certain they had not edited.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use serial_test::serial;

/// `GROK_HOME` for this test binary. The path resolver caches in a `OnceLock`,
/// so the whole binary shares one home.
fn test_home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let path = tempfile::TempDir::new().unwrap().keep();
        // SAFETY: set once at init, before any other thread reads the env.
        unsafe { std::env::set_var("GROK_HOME", &path) };
        path
    })
}

const DECLARED: &str = "[ui]\ntheme = \"grokday\"\n";

/// Install a config.toml with `mode`, returning its path.
fn install_config(mode: u32) -> PathBuf {
    let path = test_home().join("config.toml");
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o644));
    fs::write(&path, DECLARED).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[tokio::test]
#[serial]
async fn settings_write_is_declined_when_the_config_is_read_only() {
    let path = install_config(0o444);

    let err = xai_grok_shell::util::config::set_theme("tokyonight".to_string())
        .await
        .expect_err("a read-only config must decline the write");
    let msg = err.to_string();
    assert!(
        msg.contains("config.toml") && msg.contains("read-only"),
        "the refusal must name the file and say why, got: {msg}"
    );
    assert!(
        !msg.to_lowercase().contains("failed"),
        "nothing failed — the write was declined, got: {msg}"
    );

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        DECLARED,
        "the declared config must survive byte for byte"
    );
    assert!(
        fs::read_dir(test_home())
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().contains("toml.tmp")),
        "a declined write must not leave a temp file behind"
    );
}

/// The probe reads the file, not the directory: an ordinary writable config
/// still saves. Without this the refusal would lock every user out of their
/// own settings.
#[tokio::test]
#[serial]
async fn settings_write_still_lands_on_a_writable_config() {
    let path = install_config(0o644);

    xai_grok_shell::util::config::set_theme("tokyonight".to_string())
        .await
        .expect("a writable config must still save");

    assert!(fs::read_to_string(&path).unwrap().contains("tokyonight"));
}

/// A plugin-contributed setting is one more key in the same file, so it is
/// declined by the same lock.
///
/// This is the hole the design had to avoid: a plugin row whose value the
/// sidecar kept for itself would have written happily here, giving a machine
/// whose configuration is declarative exactly the mutable per-plugin state the
/// lock exists to prevent — and grok would never have known it happened.
/// Routing the write through `update_config` is what makes the refusal
/// automatic rather than something the plugin path has to remember.
#[tokio::test]
#[serial]
async fn plugin_setting_write_is_declined_when_the_config_is_read_only() {
    let path = install_config(0o444);

    let err = xai_grok_shell::util::config::set_plugin_setting(
        "council".to_string(),
        "verbose".to_string(),
        serde_json::json!(true),
    )
    .await
    .expect_err("a read-only config must decline a plugin setting write too");
    let msg = err.to_string();
    assert!(
        msg.contains("config.toml") && msg.contains("read-only"),
        "the refusal must name the file and say why, got: {msg}"
    );

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        DECLARED,
        "the declared config must survive byte for byte"
    );
}

/// The same write on a writable config lands in `[plugins.<name>]` — the table
/// the sidecar already reads at `initialize` / `config_get` — without
/// disturbing the rest of the file.
#[tokio::test]
#[serial]
async fn plugin_setting_write_lands_in_the_plugins_table() {
    let path = install_config(0o644);

    xai_grok_shell::util::config::set_plugin_setting(
        "council".to_string(),
        "verbose".to_string(),
        serde_json::json!(true),
    )
    .await
    .expect("a writable config must accept a plugin setting");

    let written: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        written
            .get("plugins")
            .and_then(|p| p.get("council"))
            .and_then(|c| c.get("verbose"))
            .and_then(toml::Value::as_bool),
        Some(true),
    );
    assert_eq!(
        written
            .get("ui")
            .and_then(|u| u.get("theme"))
            .and_then(toml::Value::as_str),
        Some("grokday"),
        "an unrelated section must round-trip untouched"
    );
}
