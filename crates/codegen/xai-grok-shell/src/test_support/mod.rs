pub(crate) mod lsp_runtime;

pub(crate) const TEST_MODEL: &str = "test-model";

/// Permission bits (`mode & 0o777`) of `path`, for owner-only assertions.
#[cfg(unix)]
pub(crate) fn unix_mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Set `path`'s permission bits, e.g. to simulate umask-default dirs.
#[cfg(unix)]
pub(crate) fn set_unix_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Keep this crate's unit-test binary from writing synthetic events into the real unified log.
/// Runs pre-main so the redirect beats the lazily-opened writer.
/// Integration binaries under `tests/` isolate via `TestSandbox` homes instead.
#[ctor::ctor]
fn redirect_unified_log_for_tests() {
    xai_grok_telemetry::unified_log::redirect_to_temp_for_tests();
}

/// Keep this crate's unit-test binary out of the developer's real `~/.grok`.
///
/// `grok_home()` caches the home for the process, so the first case to resolve
/// it fixes the path for every case after it, and a per-case `$GROK_HOME` guard
/// only helps whichever case happens to run first. Losing that race is not a
/// stale assertion: the suite plants `/tmp` grants in the live
/// `trusted_folders.toml` that gates repo-local MCP and LSP spawning, and fills
/// `sessions/` and `worktrees.db` with per-test fixtures.
/// Pre-main is the only point where no test thread can have asked yet.
#[ctor::ctor]
fn redirect_grok_home_for_tests() {
    xai_dirs::redirect_grok_home_for_tests();
}

/// Losing the pin above is invisible from a test run: every case still passes,
/// having written to the developer's home instead of a temp dir. This is the
/// assertion that fails when the `#[ctor]` is dropped or stops running.
#[test]
fn the_unit_test_binary_never_resolves_the_real_grok_home() {
    assert_ne!(
        crate::util::grok_home::grok_home(),
        xai_dirs::default_grok_home(),
        "this binary resolved <home>/.grok: the pre-main grok-home pin is gone"
    );
}

/// Prepend the hermetic git binary (via `GIT_BIN_PATH`) to `PATH`.
/// `Command::new("git")` in test helpers then resolves to the Bazel-provided static binary instead of system-installed git.
///
/// Safe to call multiple times; only the first call mutates `PATH`.
pub(crate) fn ensure_hermetic_git_on_path() {
    use std::path::PathBuf;
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if let Ok(git_bin) = std::env::var("GIT_BIN_PATH") {
            let p = PathBuf::from(&git_bin);
            let p = if p.is_relative() {
                std::env::current_dir().unwrap().join(&p)
            } else {
                p
            };
            if let Some(dir) = p.parent() {
                let cur = std::env::var("PATH").unwrap_or_default();
                unsafe {
                    std::env::set_var("PATH", format!("{}:{}", dir.display(), cur));
                    // git-minimal spawns subcommands (`git stash` invokes `git update-index`) through its exec path
                    // That path is baked to a build-machine prefix
                    // Helpers live next to the binary, so point the exec path there
                    // Skip the host-fallback wrapper: host git must keep its own exec path
                    if p.file_name().is_some_and(|name| name == "git") {
                        std::env::set_var("GIT_EXEC_PATH", dir);
                    }
                }
            }
        }
    });
}
