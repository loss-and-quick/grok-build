//! Turning a registered plugin into the `Command` that starts its sidecar.
//!
//! A plugin is an executable that speaks the wire contract. The manifest names
//! the program and its argv; this module adds the working directory and the
//! environment every sidecar is entitled to, and nothing else. No runtime is
//! discovered here and no argv is invented: knowledge about *running
//! TypeScript* lives in the SDK's `sdk/plugin/src/run` launcher, next to the
//! TypeScript authors, and reaches the host as an ordinary program to exec.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::RegisteredPlugin;

/// How a sidecar is launched: a program and its arguments, as the manifest
/// layer resolved them.
///
/// `program` is either an absolute path the manifest layer already checked
/// against the plugin root, or a bare name looked up on `PATH` at spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLaunch {
    /// Program to execute.
    pub program: PathBuf,
    /// Arguments after `argv[0]`.
    pub args: Vec<String>,
}

/// Env var carrying the manifest's `network` flag to the sidecar: `"1"` when
/// the plugin may reach the network, `"0"` when it may not.
///
/// Set on every sidecar spawn, with both values — see [`build_command`] for why
/// it is never simply omitted. It is *information* for the child, not the
/// enforcement: what `network: false` is worth is decided by the platform
/// confinement the [`crate::SpawnHardener`] installs on the spawn, which the
/// child cannot see, undo, or escape by ignoring this variable.
pub const NETWORK_ENV: &str = "GROK_PLUGIN_NETWORK";

/// Env var naming the plugin's own directory.
pub const PLUGIN_ROOT_ENV: &str = "GROK_PLUGIN_ROOT";

/// Env var naming the plugin's per-plugin data directory.
pub const PLUGIN_DATA_ENV: &str = "GROK_PLUGIN_DATA";

/// Build the spawn `Command` for a plugin: the manifest's argv, the workspace
/// as the working directory, and the sidecar's environment.
///
/// Network confinement for `network: false` sidecars is not applied here: the
/// child runs under whatever `xai-grok-sandbox::child_network` offers on this
/// platform, installed by the [`crate::SpawnHardener`] the shell injects — the
/// shell owns the `xai-grok-sandbox` dependency and the trust flow, so this
/// crate stays sandbox-free. Landlock/Seatbelt filesystem confinement is
/// inherited automatically (plugins are children of the sandboxed process).
///
/// That confinement is a property of the child process, not of the program it
/// runs: the hardener is keyed on `spec.network` alone, and both mechanisms (a
/// Linux seccomp filter, a macOS Seatbelt profile) survive `exec` and are
/// inherited by descendants. So a plugin that execs its way into a JS runtime
/// is confined exactly as a compiled one is, without this crate learning
/// anything about the program it runs.
pub fn build_command(spec: &RegisteredPlugin) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&spec.launch.program);
    cmd.args(spec.launch.args.iter().map(OsString::from))
        .current_dir(&spec.workspace_root);
    // Tier 1 orchestration: hand the leader socket to the sidecar under the
    // same env var the leader client honors, so an SDK-less plugin (or any
    // headless ACP client library) connects with zero extra plumbing. Also
    // advertised in `HostCapabilities::leader_socket` at `initialize`.
    if let Some(socket) = &spec.leader_socket {
        cmd.env("GROK_LEADER_SOCKET", socket);
    }
    // The plugin's own two directories. A sidecar's cwd is the workspace root,
    // so neither is otherwise derivable: a plugin whose manifest did not
    // happen to substitute `${GROK_PLUGIN_DATA}` into its argv could not find
    // its own data directory at all, and a launcher had to reconstruct the
    // plugin root from `argv[0]`. Command hooks have had both since they
    // existed; a sidecar is the same plugin's other half.
    cmd.env(PLUGIN_ROOT_ENV, &spec.plugin_root);
    cmd.env(PLUGIN_DATA_ENV, &spec.plugin_data);
    // Hand the manifest's `network` flag down as `NETWORK_ENV`, so a launcher on
    // the far side of an `exec` can align a runtime's own permission model with
    // it (deno's `--allow-net`) instead of being told the same fact a second
    // time in its argv, where the two spellings can drift apart.
    //
    // Env and not argv: argv is the manifest's, verbatim, and a program that
    // never heard of this flag must not have to parse one. Env is inert to a
    // child that ignores it, survives the `exec` a launcher performs into the
    // real runtime, and is already how `GROK_LEADER_SOCKET` reaches a sidecar.
    // `initialize` cannot carry it at all: the handshake happens long after
    // argv is fixed.
    //
    // Always set, with both values, rather than present-when-allowed. Absence
    // then means only "nothing that knows this variable spawned me", which a
    // child reads as denied — the manifest default — without mistaking an old
    // host for a decision. An unconditional `env` also overwrites any
    // `GROK_PLUGIN_NETWORK` this process inherited, which a conditional one
    // would leak into a plugin that was denied.
    //
    // Nothing is enforced here. The child's network confinement is applied to
    // this spawn by the `SpawnHardener` regardless of what the child reads from
    // this variable or does about it.
    cmd.env(NETWORK_ENV, if spec.network { "1" } else { "0" });
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn spec() -> RegisteredPlugin {
        RegisteredPlugin {
            name: "p".into(),
            launch: PluginLaunch {
                program: PathBuf::from("/ws/p/_sdk/run"),
                args: vec!["index.ts".into(), "--flag=x y".into()],
            },
            plugin_root: PathBuf::from("/ws/p"),
            plugin_data: PathBuf::from("/home/u/.grok/plugin-data/user/p"),
            network: false,
            config: serde_json::Value::Null,
            declared_tools: Vec::new(),
            workspace_root: PathBuf::from("/ws"),
            session_id: "s".into(),
            leader_socket: None,
        }
    }

    fn env_of(cmd: &tokio::process::Command, key: &str) -> Option<OsString> {
        cmd.as_std()
            .get_envs()
            .find(|(k, _)| *k == OsStr::new(key))
            .and_then(|(_, v)| v.map(|v| v.to_os_string()))
    }

    #[test]
    fn argv_reaches_the_child_verbatim() {
        // The point of the single launch form: nothing is probed, and the argv
        // reaches the child exactly as the manifest resolved it. Works on a box
        // with no bun/node/deno at all.
        let cmd = build_command(&spec());
        let std = cmd.as_std();
        assert_eq!(std.get_program(), OsStr::new("/ws/p/_sdk/run"));
        assert_eq!(
            std.get_args().collect::<Vec<_>>(),
            vec![OsStr::new("index.ts"), OsStr::new("--flag=x y")]
        );
        assert_eq!(std.get_current_dir(), Some(std::path::Path::new("/ws")));
    }

    #[test]
    fn exports_the_plugins_own_directories() {
        // Without these a sidecar cannot name a file it ships with: its cwd is
        // the workspace, not the plugin.
        let cmd = build_command(&spec());
        assert_eq!(env_of(&cmd, PLUGIN_ROOT_ENV), Some(OsString::from("/ws/p")));
        assert_eq!(
            env_of(&cmd, PLUGIN_DATA_ENV),
            Some(OsString::from("/home/u/.grok/plugin-data/user/p"))
        );
    }

    #[test]
    fn exports_the_leader_socket_only_when_there_is_one() {
        let mut with = spec();
        with.leader_socket = Some("/tmp/leader.sock".into());
        assert_eq!(
            env_of(&build_command(&with), "GROK_LEADER_SOCKET"),
            Some(OsString::from("/tmp/leader.sock"))
        );
        assert_eq!(env_of(&build_command(&spec()), "GROK_LEADER_SOCKET"), None);
    }

    #[test]
    fn states_the_network_flag_both_ways() {
        let mut allowed = spec();
        allowed.network = true;
        assert_eq!(
            env_of(&build_command(&allowed), NETWORK_ENV),
            Some(OsString::from("1"))
        );

        // Denied is stated, not left absent: a child must be able to tell a
        // denial from a host that never heard of the variable.
        let denied = build_command(&spec());
        assert_eq!(env_of(&denied, NETWORK_ENV), Some(OsString::from("0")));

        // And it stays out of argv, which belongs to the manifest.
        assert_eq!(
            denied.as_std().get_args().collect::<Vec<_>>(),
            vec![OsStr::new("index.ts"), OsStr::new("--flag=x y")]
        );
    }
}
