//! Home-directory resolution generally: USERPROFILE-first `home_dir`, plus
//! grok-home (`$GROK_HOME` or `<home>/.grok`). Shared by `xai-grok-config`
//! and `xai-fast-worktree`.
//!
//! Which function to call:
//! - [`grok_home`]: the usual choice, a cached, created path to build on.
//! - [`user_grok_home`]: `None` instead of a cwd fallback when no home resolves.
//! - [`default_grok_home`]: the `<home>/.grok` default, ignoring `$GROK_HOME`, so callers can detect an override.
//! - [`resolve_grok_home`]: a fresh, uncached resolve.
//! - [`resolve_grok_home_with_source`]: [`resolve_grok_home`] plus where the path came from.
//! - [`home_dir`]: the home directory itself, for sibling dot dirs (`~/.claude`, `~/.agents`, ...).
//! - [`redirect_grok_home_for_tests`]: pre-main pin that keeps a test binary off the real `~/.grok`.
//!
//! TODO: collapse these getters by threading the path through config as an
//! explicit value.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where a resolved grok home came from, so "why did grok pick this
/// directory?" is answerable in diagnostics without re-reading the
/// environment at the asking site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokHomeSource {
    /// A non-empty `$GROK_HOME` override.
    EnvOverride,
    /// `<home>/.grok` derived from the home directory.
    HomeDefault,
}

/// The user's home directory via [`std::env::home_dir`]: `HOME` on Unix (with
/// a passwd fallback), `USERPROFILE` on Windows.
///
/// Deliberately not `dirs::home_dir()`: on Windows `dirs` asks the
/// known-folder API and ignores a redirected `USERPROFILE`, while this crate
/// resolves `~/.grok` from the profile variable — mixing the two sources puts
/// the grok directory and other home-anchored dot directories in different
/// trees. Every home-anchored path must come from this one function.
#[allow(deprecated, clippy::disallowed_methods)] // the one sanctioned std::env::home_dir call
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir()
}

/// `<home>/.grok`, canonicalized via `dunce` (not `std::fs::canonicalize`,
/// which yields Windows `\\?\` verbatim paths).
fn grok_home_in(home: &Path) -> PathBuf {
    dunce::canonicalize(home)
        .unwrap_or_else(|_| home.to_path_buf())
        .join(".grok")
}

/// `$GROK_HOME` verbatim when non-empty, else `<home>/.grok`. The env value is
/// used as-is (not canonicalized) so it stays stable and comparable: callers do
/// literal prefix checks against it, and downstream symlink guards must still see
/// its original components.
fn resolve_grok_home_from(
    grok_home_env: Option<&OsStr>,
    os_home: Option<&Path>,
) -> Option<(PathBuf, GrokHomeSource)> {
    if let Some(env) = grok_home_env.filter(|env| !env.is_empty()) {
        return Some((PathBuf::from(env), GrokHomeSource::EnvOverride));
    }
    os_home.map(|home| (grok_home_in(home), GrokHomeSource::HomeDefault))
}

/// Resolve the grok home from the environment (fresh, no cache); `None` if neither resolves.
pub fn resolve_grok_home() -> Option<PathBuf> {
    resolve_grok_home_with_source().map(|(home, _)| home)
}

/// [`resolve_grok_home`] plus the [`GrokHomeSource`] the path came from.
pub fn resolve_grok_home_with_source() -> Option<(PathBuf, GrokHomeSource)> {
    resolve_grok_home_from(
        std::env::var_os("GROK_HOME").as_deref(),
        home_dir().as_deref(),
    )
}

/// The default `<home>/.grok`, used when `$GROK_HOME` is unset.
pub fn default_grok_home() -> PathBuf {
    grok_home_in(&home_dir().unwrap_or_else(|| PathBuf::from(".")))
}

/// The process's grok home, resolved at most once (see [`grok_home`]).
///
/// Module-scoped rather than function-local so [`redirect_grok_home_for_tests`]
/// can claim it before anything else resolves it.
static GROK_HOME: OnceLock<PathBuf> = OnceLock::new();

/// The grok home, created if missing and cached for the process; falls back to
/// [`default_grok_home`] when neither `$GROK_HOME` nor a home resolves.
///
/// The cache is process-wide, so in a test binary the first caller fixes the
/// path for every later one and a per-test `$GROK_HOME` override does nothing.
/// Test binaries install [`redirect_grok_home_for_tests`] pre-main instead.
pub fn grok_home() -> PathBuf {
    GROK_HOME
        .get_or_init(|| {
            let home = resolve_grok_home().unwrap_or_else(default_grok_home);
            if let Err(err) = std::fs::create_dir_all(&home) {
                tracing::warn!(path = %home.display(), %err, "failed to create grok home");
            }
            home
        })
        .clone()
}

/// Claim [`grok_home`] for a fresh, owner-only directory under the system temp
/// dir, so a test binary cannot read or write the developer's real `~/.grok`.
///
/// A test that sets `$GROK_HOME` is isolated only if nothing in the same binary
/// resolved the home first, which no test can arrange: libtest runs cases in
/// parallel in an unspecified order. Whichever case loses that race gets the
/// real home, and the writers behind it are the live folder-trust store, the
/// session index and the worktree database. Claiming the cache pre-main, before
/// any test thread exists, is the only ordering that holds.
///
/// `$GROK_HOME` is deliberately left alone, so the uncached resolvers
/// ([`resolve_grok_home`], [`default_grok_home`]) keep answering from the
/// environment: a per-test override still means what it says, and a case that
/// simulates "no home resolves" still can. The cost is that callers resolving
/// fresh — the worktree database, grove pins — are not covered here and still
/// need a `$GROK_HOME` guard of their own.
///
/// Runtime-activated rather than feature-gated: Bazel compiles production and
/// test targets with one shared feature set, so a feature would leak into
/// production builds.
///
/// Returns the home now pinned for the process: the new directory, or the
/// already-resolved one if a caller got there first. The non-recursive
/// `create_dir` fails rather than adopting a pre-planted directory or symlink
/// in the world-writable temp dir, and the panic is deliberate — falling back
/// silently would hand the binary the very home this exists to keep it off.
pub fn redirect_grok_home_for_tests() -> PathBuf {
    GROK_HOME
        .get_or_init(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir =
                std::env::temp_dir().join(format!("grok-home-test-{}-{nanos}", std::process::id()));
            std::fs::create_dir(&dir)
                .unwrap_or_else(|err| panic!("test grok home {}: {err}", dir.display()));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            dir
        })
        .clone()
}

/// Like [`grok_home`], but `None` when no home resolves (no cwd fallback).
pub fn user_grok_home() -> Option<PathBuf> {
    resolve_grok_home().is_some().then(grok_home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::ffi::OsString;

    #[test]
    fn env_wins_over_os_home() {
        let resolved =
            resolve_grok_home_from(Some(OsStr::new("/custom/home")), Some(Path::new("/home/u")));
        assert_eq!(
            resolved,
            Some((PathBuf::from("/custom/home"), GrokHomeSource::EnvOverride))
        );
    }

    #[test]
    fn env_used_verbatim_even_when_it_exists() {
        // A real, existing dir whose canonical form differs (macOS symlinks
        // `/var` -> `/private/var`): the env value must come back unchanged.
        let tmp = tempfile::tempdir().unwrap();
        let resolved = resolve_grok_home_from(Some(tmp.path().as_os_str()), None);
        assert_eq!(
            resolved,
            Some((tmp.path().to_path_buf(), GrokHomeSource::EnvOverride))
        );
    }

    #[test]
    fn empty_env_falls_through_to_os_home() {
        let tmp = tempfile::tempdir().unwrap();
        let resolved = resolve_grok_home_from(Some(&OsString::new()), Some(tmp.path()));
        assert_eq!(
            resolved,
            Some((
                dunce::canonicalize(tmp.path()).unwrap().join(".grok"),
                GrokHomeSource::HomeDefault
            ))
        );
    }

    #[test]
    fn default_grok_home_has_no_verbatim_prefix() {
        // The reason we canonicalize via dunce: std::fs::canonicalize yields
        // `\\?\` verbatim paths on Windows that break git and byte-exact
        // comparisons. No-op assertion on Unix.
        let home = default_grok_home();
        assert!(!home.to_string_lossy().starts_with(r"\\?\"));
        assert!(home.ends_with(".grok"));
    }

    /// The redirect has to win over the environment, or a test binary that
    /// installs it still resolves the developer's `~/.grok`.
    ///
    /// Sound as a `#[test]` only because nothing else in this crate's test
    /// binary calls [`grok_home`]: the first caller wins the `OnceLock`, which
    /// is the whole reason the redirect runs pre-main everywhere else.
    #[test]
    fn redirect_claims_the_home_ahead_of_the_environment() {
        let redirected = redirect_grok_home_for_tests();
        assert!(redirected.is_dir());
        assert_ne!(redirected, default_grok_home());
        assert_eq!(grok_home(), redirected);
        assert_eq!(
            redirect_grok_home_for_tests(),
            redirected,
            "a second call must not mint a second home"
        );
        std::fs::remove_dir_all(&redirected).expect("clean up the redirected home");
    }

    #[test]
    fn none_when_nothing_resolves() {
        assert_eq!(
            resolve_grok_home_from(/* grok_home_env */ None, /* os_home */ None),
            None
        );
    }
}
