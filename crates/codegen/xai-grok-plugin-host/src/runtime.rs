//! Plugin-runtime discovery and argv construction.
//!
//! A TS plugin's entry file is executed by one of three JS runtimes. The
//! manifest may name one explicitly (`bun`/`node`/`deno`) or ask for `auto`, in
//! which case we probe `PATH` in preference order **bun → node (>=22) → deno**
//! and pick the first available.
//!
//! Discovery (a `which`-style `PATH` search plus, for node, a `--version` probe)
//! is cached process-wide: the toolchain doesn't change under a running session,
//! and re-`exec`ing `node --version` on every sidecar spawn is wasteful. Argv
//! construction is a pure function of the *resolved* runtime, so it is
//! unit-tested directly without touching any real binary.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::RegisteredPlugin;

/// Runtime selection as declared in the plugin manifest.
///
/// `Deserialize` accepts the manifest's `"runtime": "auto|bun|node|deno"` string
/// so the manifest-parsing consumer maps it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    /// bun → node (>=22) → deno, first found wins.
    #[default]
    Auto,
    Bun,
    Node,
    Deno,
}

impl RuntimeKind {
    fn label(self) -> &'static str {
        match self {
            RuntimeKind::Auto => "auto",
            RuntimeKind::Bun => "bun",
            RuntimeKind::Node => "node",
            RuntimeKind::Deno => "deno",
        }
    }
}

/// A discovery failure: no suitable runtime on `PATH`.
#[derive(Debug, thiserror::Error)]
#[error("no plugin runtime available for `{requested}`: {detail}")]
pub struct RuntimeDiscoveryError {
    pub requested: &'static str,
    pub detail: String,
}

/// A concrete runtime resolved to an on-disk binary. Argv construction consumes
/// this; it never re-probes `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedRuntime {
    Bun {
        path: PathBuf,
    },
    Node {
        path: PathBuf,
        /// Node strips TS types unflagged from 23.6; older (but >=22) needs the
        /// `--experimental-strip-types` flag.
        needs_strip_flag: bool,
    },
    Deno {
        path: PathBuf,
    },
}

/// Minimum node major we run under (TS type-stripping landed in the 22 line).
const MIN_NODE_MAJOR: u32 = 22;

/// Whether node `major.minor` still needs `--experimental-strip-types`.
///
/// Type-stripping is unflagged from 23.6.0 onwards; 22.x and 23.0–23.5 need the
/// flag. Pure so the decision is unit-tested without a real node.
fn node_needs_strip_flag(major: u32, minor: u32) -> bool {
    major < 23 || (major == 23 && minor < 6)
}

/// Whether a detected node major is new enough to run plugins at all.
fn node_major_supported(major: u32) -> bool {
    major >= MIN_NODE_MAJOR
}

/// Parse `node --version` output (`v23.6.1`) into `(major, minor)`.
fn parse_node_version(raw: &str) -> Option<(u32, u32)> {
    let v = raw.trim().trim_start_matches('v');
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Some((major, minor))
}

/// Build the `(program, args)` argv for a resolved runtime + entry file.
///
/// Pure and side-effect-free — the unit tests drive it with synthetic
/// `ResolvedRuntime`s. `network` only affects deno today: without it we still
/// scope read/write to the workspace, and only add `--allow-net` when the
/// manifest opts in. (bun/node network confinement is the seccomp filter's job;
/// see [`build_command`].)
fn build_argv(
    resolved: &ResolvedRuntime,
    entry: &Path,
    workspace_root: &Path,
    network: bool,
) -> (PathBuf, Vec<OsString>) {
    match resolved {
        ResolvedRuntime::Bun { path } => (path.clone(), vec![entry.into()]),
        ResolvedRuntime::Node {
            path,
            needs_strip_flag,
        } => {
            let mut args: Vec<OsString> = Vec::new();
            if *needs_strip_flag {
                args.push("--experimental-strip-types".into());
            }
            args.push(entry.into());
            (path.clone(), args)
        }
        ResolvedRuntime::Deno { path } => {
            let mut allow_read = OsString::from("--allow-read=");
            allow_read.push(workspace_root);
            let mut allow_write = OsString::from("--allow-write=");
            allow_write.push(workspace_root);
            let mut args: Vec<OsString> = vec![
                "run".into(),
                // Never block on an interactive permission prompt in a sidecar.
                "--no-prompt".into(),
                allow_read,
                allow_write,
            ];
            if network {
                args.push("--allow-net".into());
            }
            args.push(entry.into());
            (path.clone(), args)
        }
    }
}

type RuntimeCache = Mutex<HashMap<RuntimeKind, ResolvedRuntime>>;

fn cache() -> &'static RuntimeCache {
    static CACHE: OnceLock<RuntimeCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Locate a runtime binary on `PATH`, caching the result process-wide.
pub fn resolve_runtime(kind: RuntimeKind) -> Result<ResolvedRuntime, RuntimeDiscoveryError> {
    if let Some(hit) = cache().lock().expect("runtime cache poisoned").get(&kind) {
        return Ok(hit.clone());
    }
    let resolved = discover(kind)?;
    cache()
        .lock()
        .expect("runtime cache poisoned")
        .insert(kind, resolved.clone());
    Ok(resolved)
}

fn discover(kind: RuntimeKind) -> Result<ResolvedRuntime, RuntimeDiscoveryError> {
    match kind {
        RuntimeKind::Bun => discover_bun(),
        RuntimeKind::Node => discover_node(),
        RuntimeKind::Deno => discover_deno(),
        RuntimeKind::Auto => discover_bun()
            .or_else(|_| discover_node())
            .or_else(|_| discover_deno())
            .map_err(|_| RuntimeDiscoveryError {
                requested: "auto",
                detail: "none of bun, node (>=22), or deno were found on PATH".to_string(),
            }),
    }
}

fn discover_bun() -> Result<ResolvedRuntime, RuntimeDiscoveryError> {
    let path = which::which("bun").map_err(|e| RuntimeDiscoveryError {
        requested: RuntimeKind::Bun.label(),
        detail: format!("bun not on PATH: {e}"),
    })?;
    Ok(ResolvedRuntime::Bun { path })
}

fn discover_deno() -> Result<ResolvedRuntime, RuntimeDiscoveryError> {
    let path = which::which("deno").map_err(|e| RuntimeDiscoveryError {
        requested: RuntimeKind::Deno.label(),
        detail: format!("deno not on PATH: {e}"),
    })?;
    Ok(ResolvedRuntime::Deno { path })
}

fn discover_node() -> Result<ResolvedRuntime, RuntimeDiscoveryError> {
    let path = which::which("node").map_err(|e| RuntimeDiscoveryError {
        requested: RuntimeKind::Node.label(),
        detail: format!("node not on PATH: {e}"),
    })?;

    // Probe the version to decide on `--experimental-strip-types` and to reject
    // pre-22 nodes that can't run TS at all. A probe failure is non-fatal: keep
    // the flag (safe on the 22 line, our floor) and let spawn surface real
    // errors.
    let (needs_strip_flag, supported) = match std::process::Command::new(&path)
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            match parse_node_version(&raw) {
                Some((major, minor)) => (
                    node_needs_strip_flag(major, minor),
                    node_major_supported(major),
                ),
                None => {
                    tracing::warn!(version = %raw.trim(), "unparseable node --version; assuming strip flag");
                    (true, true)
                }
            }
        }
        Ok(out) => {
            tracing::warn!(status = %out.status, "node --version failed; assuming strip flag");
            (true, true)
        }
        Err(e) => {
            tracing::warn!("node --version probe error: {e}; assuming strip flag");
            (true, true)
        }
    };

    if !supported {
        return Err(RuntimeDiscoveryError {
            requested: RuntimeKind::Node.label(),
            detail: format!("node is older than v{MIN_NODE_MAJOR}, which cannot run TS plugins"),
        });
    }
    Ok(ResolvedRuntime::Node {
        path,
        needs_strip_flag,
    })
}

/// Build the spawn `Command` for a plugin: discover its runtime, construct the
/// argv, and set the working directory + env.
///
/// Network confinement for `network: false` sidecars is not applied here: the
/// child runs under the per-child seccomp network filter
/// (`xai-grok-sandbox::child_net`, an `unsafe pre_exec`) installed by the
/// [`crate::SpawnHardener`] the shell injects — the shell owns the
/// `xai-grok-sandbox` dependency and the trust flow, so this crate stays
/// sandbox-free. Landlock/Seatbelt confinement is inherited automatically
/// (plugins are children of the sandboxed process).
pub fn build_command(
    spec: &RegisteredPlugin,
) -> Result<tokio::process::Command, RuntimeDiscoveryError> {
    let resolved = resolve_runtime(spec.runtime)?;
    let (program, args) = build_argv(&resolved, &spec.entry, &spec.workspace_root, spec.network);

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).current_dir(&spec.workspace_root);
    // Tier 1 orchestration: hand the leader socket to the sidecar under the
    // same env var the leader client honors, so an SDK-less plugin (or any
    // headless ACP client library) connects with zero extra plumbing. Also
    // advertised in `HostCapabilities::leader_socket` at `initialize`.
    if let Some(socket) = &spec.leader_socket {
        cmd.env("GROK_LEADER_SOCKET", socket);
    }
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_strip_flag_boundary() {
        // Needs the flag below 23.6.
        assert!(node_needs_strip_flag(22, 0));
        assert!(node_needs_strip_flag(22, 11));
        assert!(node_needs_strip_flag(23, 0));
        assert!(node_needs_strip_flag(23, 5));
        // Unflagged from 23.6 onwards.
        assert!(!node_needs_strip_flag(23, 6));
        assert!(!node_needs_strip_flag(23, 7));
        assert!(!node_needs_strip_flag(24, 0));
    }

    #[test]
    fn node_support_floor() {
        assert!(!node_major_supported(20));
        assert!(!node_major_supported(21));
        assert!(node_major_supported(22));
        assert!(node_major_supported(24));
    }

    #[test]
    fn runtime_kind_deserializes_from_manifest_string() {
        for (s, want) in [
            ("auto", RuntimeKind::Auto),
            ("bun", RuntimeKind::Bun),
            ("node", RuntimeKind::Node),
            ("deno", RuntimeKind::Deno),
        ] {
            let got: RuntimeKind = serde_json::from_str(&format!("\"{s}\"")).unwrap();
            assert_eq!(got, want);
        }
        assert!(serde_json::from_str::<RuntimeKind>("\"python\"").is_err());
    }

    #[test]
    fn parse_node_version_forms() {
        assert_eq!(parse_node_version("v23.6.1\n"), Some((23, 6)));
        assert_eq!(parse_node_version("v22.11.0"), Some((22, 11)));
        assert_eq!(parse_node_version(" v24.0.0 "), Some((24, 0)));
        assert_eq!(parse_node_version("garbage"), None);
    }

    #[test]
    fn bun_argv_is_just_the_entry() {
        let (program, args) = build_argv(
            &ResolvedRuntime::Bun {
                path: "/usr/bin/bun".into(),
            },
            Path::new("/ws/plugin/index.ts"),
            Path::new("/ws"),
            false,
        );
        assert_eq!(program, PathBuf::from("/usr/bin/bun"));
        assert_eq!(args, vec![OsString::from("/ws/plugin/index.ts")]);
    }

    #[test]
    fn node_argv_adds_strip_flag_when_needed() {
        let entry = Path::new("/ws/p/index.ts");
        let (_, with_flag) = build_argv(
            &ResolvedRuntime::Node {
                path: "/usr/bin/node".into(),
                needs_strip_flag: true,
            },
            entry,
            Path::new("/ws"),
            false,
        );
        assert_eq!(
            with_flag,
            vec![
                OsString::from("--experimental-strip-types"),
                OsString::from("/ws/p/index.ts"),
            ]
        );

        let (_, without_flag) = build_argv(
            &ResolvedRuntime::Node {
                path: "/usr/bin/node".into(),
                needs_strip_flag: false,
            },
            entry,
            Path::new("/ws"),
            false,
        );
        assert_eq!(without_flag, vec![OsString::from("/ws/p/index.ts")]);
    }

    #[test]
    fn deno_argv_scopes_perms_to_workspace() {
        let (_, args) = build_argv(
            &ResolvedRuntime::Deno {
                path: "/usr/bin/deno".into(),
            },
            Path::new("/ws/p/index.ts"),
            Path::new("/ws"),
            false,
        );
        assert_eq!(
            args,
            vec![
                OsString::from("run"),
                OsString::from("--no-prompt"),
                OsString::from("--allow-read=/ws"),
                OsString::from("--allow-write=/ws"),
                OsString::from("/ws/p/index.ts"),
            ]
        );
    }

    #[test]
    fn build_command_exports_leader_socket_env() {
        // Needs a real runtime on PATH to resolve; skip quietly otherwise
        // (same guard as the sidecar e2e tests).
        if resolve_runtime(RuntimeKind::Auto).is_err() {
            return;
        }
        let spec = |leader_socket: Option<String>| RegisteredPlugin {
            name: "p".into(),
            entry: PathBuf::from("/ws/p/index.ts"),
            runtime: RuntimeKind::Auto,
            network: false,
            config: serde_json::Value::Null,
            declared_tools: Vec::new(),
            workspace_root: PathBuf::from("/ws"),
            session_id: "s".into(),
            leader_socket,
        };
        let env_of = |cmd: &tokio::process::Command| -> Option<OsString> {
            cmd.as_std()
                .get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new("GROK_LEADER_SOCKET"))
                .and_then(|(_, v)| v.map(|v| v.to_os_string()))
        };

        let with = build_command(&spec(Some("/tmp/leader.sock".into()))).unwrap();
        assert_eq!(env_of(&with), Some(OsString::from("/tmp/leader.sock")));

        let without = build_command(&spec(None)).unwrap();
        assert_eq!(env_of(&without), None);
    }

    #[test]
    fn deno_argv_adds_net_when_opted_in() {
        let (_, args) = build_argv(
            &ResolvedRuntime::Deno {
                path: "/usr/bin/deno".into(),
            },
            Path::new("/ws/p/index.ts"),
            Path::new("/ws"),
            true,
        );
        assert!(args.contains(&OsString::from("--allow-net")));
    }
}
