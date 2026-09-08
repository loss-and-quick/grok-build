use std::path::{Path, PathBuf};

use xai_test_utils::git::run_git;

#[cfg(feature = "metadata")]
use crate::git::{Safety, safe_to_delete_worktree};

/// Canonical `WorktreeRecord` fixture. All test builders across the crate
/// delegate here and override only the fields they parameterize, so field
/// defaults stay in one place. Gated on `metadata` since `WorktreeRecord` is.
#[cfg(feature = "metadata")]
pub(crate) fn worktree_record(id: &str, path: impl Into<PathBuf>) -> crate::db::WorktreeRecord {
    crate::db::WorktreeRecord {
        id: id.to_string(),
        path: path.into(),
        source_repo: "/repo".into(),
        repo_name: "repo".to_string(),
        kind: crate::db::WorktreeKind::Session,
        creation_mode: "linked".to_string(),
        git_ref: None,
        head_commit: None,
        session_id: None,
        creator_pid: None,
        created_at: 1,
        last_accessed_at: None,
        status: crate::db::WorktreeStatus::Alive,
        metadata: None,
    }
}

/// `GcOptions` that expire everything immediately (`max_age_secs: 0`).
/// Gated on `metadata` since `GcOptions` is.
#[cfg(feature = "metadata")]
pub(crate) fn expire_now() -> crate::api::gc::GcOptions {
    crate::api::gc::GcOptions {
        max_age_secs: Some(0),
        ..Default::default()
    }
}

/// Like `expire_now` but skips the liveness scan (`force`).
#[cfg(feature = "metadata")]
pub(crate) fn expire_now_forced() -> crate::api::gc::GcOptions {
    crate::api::gc::GcOptions {
        force: true,
        ..expire_now()
    }
}

/// Like `expire_now` but previews without mutating (`dry_run`).
#[cfg(feature = "metadata")]
pub(crate) fn dry_run_opts() -> crate::api::gc::GcOptions {
    crate::api::gc::GcOptions {
        dry_run: true,
        ..expire_now()
    }
}

pub(crate) fn seed_source(source: &Path, ignore_lines: &str) {
    let remote = source.with_file_name("remote.git");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::create_dir_all(source.join("build")).unwrap();
    run_git(
        source.parent().unwrap(),
        &["init", "--bare", remote.to_str().unwrap()],
    );
    xai_test_utils::git::git_init_seed(source);
    run_git(
        source,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    std::fs::write(source.join(".gitignore"), ignore_lines).unwrap();
    std::fs::write(source.join("tracked.txt"), "one\n").unwrap();
    std::fs::write(source.join("nested/tracked.txt"), "one\n").unwrap();
    std::fs::write(source.join("build/tracked.txt"), "one\n").unwrap();
    run_git(source, &["add", "."]);
    run_git(source, &["commit", "-m", "seed"]);
    publish(source, "main");
}

pub(crate) fn add_worktree(source: &Path, at: &Path) -> PathBuf {
    run_git(
        source,
        &["worktree", "add", "--detach", at.to_str().unwrap(), "HEAD"],
    );
    at.to_path_buf()
}

#[cfg(feature = "metadata")]
pub(crate) fn deletable_linked_worktree(root: &Path, name: &str) -> PathBuf {
    let source = root.join("gate-source");
    if !source.is_dir() {
        seed_source(&source, "");
    }
    let worktree = add_worktree(&source, &root.join(name));
    assert_eq!(
        safe_to_delete_worktree(&worktree, Some(&source)),
        Safety::Delete,
        "gc fixtures must clear the gate"
    );
    worktree
}

#[cfg(feature = "metadata")]
pub(crate) fn deletable_standalone_worktree(root: &Path, name: &str) -> (PathBuf, PathBuf) {
    let source = root.join("gate-source");
    if !source.is_dir() {
        seed_source(&source, "");
    }
    let worktree = root.join(name);
    run_git(
        root,
        &[
            "clone",
            "--branch",
            "main",
            root.join("remote.git").to_str().unwrap(),
            worktree.to_str().unwrap(),
        ],
    );
    (worktree, source)
}

pub(crate) fn publish(at: &Path, branch: &str) {
    run_git(
        at,
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
    );
    run_git(at, &["fetch", "origin"]);
}

/// Confine a test to a private grok home, `$HOME` and grove data dir for as
/// long as the returned guard lives.
///
/// For any test whose production path registers, unregisters or garbage-collects
/// a worktree: those resolve the home fresh from the environment on every call,
/// so without this they write the developer's real `~/.grok/worktrees.db` and
/// `~/.local/share/grove`. The pre-main memo in `xai-dirs` cannot cover them —
/// it pins the cached lookup, and these deliberately do not use it.
///
/// Feature-agnostic on purpose: most of these tests also compile without
/// `metadata`, where the database the guard isolates does not exist and there is
/// nothing to confine, so the guard is nothing there rather than a `cfg` at every
/// call site.
#[cfg(feature = "metadata")]
pub(crate) fn isolated_home() -> crate::db::GrokHomeFixture {
    crate::db::GrokHomeFixture::new()
}

/// Stand-in guard for a build with no database: it isolates nothing because
/// there is nothing to isolate. A named type rather than `()` so the call sites
/// read identically in both builds.
#[cfg(not(feature = "metadata"))]
pub(crate) struct NoHomeToIsolate;

/// See the `metadata` counterpart; without the database there is nothing to isolate.
#[cfg(not(feature = "metadata"))]
pub(crate) fn isolated_home() -> NoHomeToIsolate {
    NoHomeToIsolate
}
