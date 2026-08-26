//! Decline writes to a `config.toml` that grok was not given to rewrite.
//!
//! [`super::persist::update_config`] replaces the user config with a temp file
//! plus `rename`. `rename` is permission-checked against the *directory*, not
//! the file, so a `-r--r--r--` `config.toml` inside a writable `~/.grok` is
//! replaced without an error — and because the replacement inherits the old
//! file's mode, the result looks byte-for-byte as read-only as before. A
//! configuration manager that installs the file declaratively (home-manager,
//! chezmoi, ansible) reports it as modified out from under it, while the user
//! sees a file whose permissions never changed and concludes they changed
//! nothing.
//!
//! The atomic write is not the bug — dropping it would let a crash mid-write
//! truncate the config. The bug is starting a write we have no business
//! making, so writes ask here first and are declined before anything is
//! serialized.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Forces the read-only treatment on (`1`/`true`/`yes`/`on`) or off
/// (`0`/`false`/`no`/`off`), overriding the file-mode probe. `1` opts in a
/// declarative setup that installs a *writable* config; `0` is the escape
/// hatch when the probe is not what the user wants.
pub const CONFIG_READONLY_ENV: &str = "GROK_CONFIG_READONLY";

/// Why grok treats the user `config.toml` as not its to rewrite.
///
/// Deliberately unrelated to `managed_config.toml` — that is the
/// server-synced enterprise policy layer, this is a local file mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigReadOnly {
    /// The file carries no owner-write bit. This is what a declarative
    /// installer leaves behind, and what `rename` silently ignores.
    FileMode,
    /// [`CONFIG_READONLY_ENV`] asked for it.
    Env,
}

/// Read [`CONFIG_READONLY_ENV`]. `None` = unset/unrecognized, so a typo falls
/// back to the file probe instead of silently disabling it.
fn readonly_from_env_value(raw: Option<&str>) -> Option<bool> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Whether `path` denies its owner a write.
///
/// Follows symlinks on purpose: home-manager installs `config.toml` as a link
/// into `/nix/store`, and it is the store file (mode `444`) that says the
/// config is generated — `symlink_metadata` would report the link's own
/// `lrwxrwxrwx` and miss every nix user. A missing file is not read-only:
/// the first write of a fresh config must still work.
fn path_denies_owner_write(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o200 == 0
    }
    #[cfg(not(unix))]
    {
        meta.permissions().readonly()
    }
}

/// Path to the user config, for probes and messages.
fn config_path() -> PathBuf {
    super::mcp::user_config_path()
}

/// Live probe: whether the user `config.toml` is off-limits for writing.
///
/// One `stat` per call. Writes use this rather than [`user_config_readonly_cached`]
/// so a config that becomes managed while grok runs is honored on the next
/// write instead of at the next launch.
pub fn user_config_readonly() -> Option<ConfigReadOnly> {
    config_readonly_at(&config_path())
}

/// [`user_config_readonly`] for one config file by path.
///
/// Path-scoped so a project `.grok/config.toml` answers for itself and a test
/// fixture is never judged by the developer's own home config. Only the user
/// config honors [`CONFIG_READONLY_ENV`]: the env var is about the file grok
/// installs into `$GROK_HOME`, not about every TOML it ever writes.
pub fn config_readonly_at(path: &Path) -> Option<ConfigReadOnly> {
    if path == config_path() {
        match readonly_from_env_value(std::env::var(CONFIG_READONLY_ENV).ok().as_deref()) {
            Some(true) => return Some(ConfigReadOnly::Env),
            Some(false) => return None,
            None => {}
        }
    }
    path_denies_owner_write(path).then_some(ConfigReadOnly::FileMode)
}

/// [`user_config_readonly`] resolved once per process.
///
/// For render paths that ask per row per frame, where a `stat` per ask is
/// waste and a decision that lags a mid-session `home-manager switch` by one
/// launch costs nothing: the write path re-probes live and refuses anyway.
pub fn user_config_readonly_cached() -> Option<ConfigReadOnly> {
    static CACHED: OnceLock<Option<ConfigReadOnly>> = OnceLock::new();
    *CACHED.get_or_init(user_config_readonly)
}

/// `~`-relative rendering of `path`, so messages fit a toast.
fn display_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The file and why it is off-limits, as a sentence fragment the callers
/// below build on.
fn readonly_config_clause(source: ConfigReadOnly) -> String {
    let path = display_path(&config_path());
    match source {
        ConfigReadOnly::FileMode => format!("{path}, which is read-only"),
        ConfigReadOnly::Env => format!("{path}, held read-only by {CONFIG_READONLY_ENV}"),
    }
}

/// What a surface that knows its subject tells the user: where the value
/// actually lives, and that grok declined rather than failed.
///
/// `None` when the config is writable. Sized to survive the pager's 120-byte
/// toast scrub — past that the message is replaced with a generic error, which
/// is exactly the "something broke" reading this wording exists to avoid.
pub fn readonly_config_notice(subject: &str) -> Option<String> {
    let source = user_config_readonly()?;
    Some(format!(
        "{subject} is set in {} — change it there",
        readonly_config_clause(source)
    ))
}

/// [`readonly_config_notice`] against the process-wide cached probe, for
/// render paths that ask per row per frame.
pub fn readonly_config_notice_cached(subject: &str) -> Option<String> {
    let source = user_config_readonly_cached()?;
    Some(format!(
        "{subject} is set in {} — change it there",
        readonly_config_clause(source)
    ))
}

/// Why a file-level write was refused, naming the file it was aimed at
/// (which is not always the user config).
pub fn readonly_config_refusal(path: &Path, source: ConfigReadOnly) -> String {
    let path = display_path(path);
    match source {
        ConfigReadOnly::FileMode => format!("refusing to rewrite {path}: it is read-only"),
        ConfigReadOnly::Env => {
            format!("refusing to rewrite {path}: {CONFIG_READONLY_ENV} holds it read-only")
        }
    }
}

/// `Err` when the user `config.toml` is not grok's to rewrite.
///
/// The subject is left out because the sole settings chokepoint
/// ([`super::persist::update_config`]) serves settings, consent answers,
/// skills and the model pick alike; surfaces that know their subject use
/// [`readonly_config_notice`].
pub fn refuse_readonly_config() -> anyhow::Result<()> {
    refuse_readonly_config_for("This value")
}

/// [`refuse_readonly_config`] naming what the caller was asked to change.
pub fn refuse_readonly_config_for(subject: &str) -> anyhow::Result<()> {
    match readonly_config_notice(subject) {
        Some(notice) => Err(anyhow::anyhow!("{notice}")),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "readonly_tests.rs"]
mod tests;
