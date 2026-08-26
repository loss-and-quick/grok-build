//! What this platform can do about a *child* process's network access.
//!
//! [`child_net`](crate::child_net) holds the Linux primitive. This module is
//! the question one layer up: given a child we are about to spawn, is there a
//! mechanism here at all, and what does the caller have to do with it? The
//! answer is data ([`ChildNetworkDenial`]), not an action, because the two
//! mechanisms attach at different points of a spawn — one inside the child
//! between fork and exec, the other by rewriting the argv — and only the
//! caller owns the `Command`.
//!
//! The two mechanisms are deliberately matched in what they deny:
//!
//! - **Linux** installs a seccomp filter that fails `connect`/`bind`/`sendto`/
//!   `sendmsg`/`listen`/`accept`/`accept4` with `EPERM`, for every address
//!   family — `AF_UNIX` included. `socket(2)` still succeeds; an already
//!   connected descriptor the child inherited still reads and writes.
//! - **macOS** re-execs the child through Seatbelt with `(deny network*)`.
//!   Seatbelt classifies `connect(2)` on a Unix socket as `network-outbound`,
//!   so that denies `AF_UNIX` too, and `system-socket` stays allowed so
//!   `socket(2)` still succeeds. DNS goes with it on both (macOS resolves
//!   through the `mDNSResponder` Unix socket, which the profile deliberately
//!   does *not* re-allow — a resolver that still worked would be a difference
//!   from Linux, where the seccomp filter kills it).
//!
//! Both survive `exec` and are inherited by descendants, so neither can be
//! shed by the child spawning something else.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The Seatbelt profile that denies a child the network and nothing else.
///
/// `(allow default)` is not a hole. Seatbelt has no un-apply — no
/// `sandbox_remove`, no `sandbox_expand` — so a profile applied to an already
/// sandboxed process can only add restrictions, never lift the ones already on
/// it. A permissive rule here therefore cannot re-open what the agent's own
/// profile closed; it is what keeps this wrapper a *network* denial instead of
/// a second, accidental filesystem policy.
pub const SEATBELT_DENY_NETWORK_PROFILE: &str = "(version 1)\n(allow default)\n(deny network*)\n";

/// The system Seatbelt launcher. Applies its `-p` profile and then `execvp`s
/// the command in the same process, so the child's pid, stdio and process group
/// are the ones the caller spawned.
///
/// Apple has marked it deprecated for years without removing it, and it is the
/// only way to hand a Seatbelt profile to a program that does not apply one
/// itself: `sandbox_init` would have to run between fork and exec, where it is
/// not async-signal-safe. If a macOS release ever does remove it,
/// [`child_network_denial`] starts returning `Err` and callers fail closed
/// instead of silently launching an unconfined child.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// How this platform denies a child process the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildNetworkDenial {
    /// Install [`crate::child_net::install_child_network_filter`] in the child
    /// between fork and exec (a `pre_exec` hook).
    Seccomp,
    /// Prepend `program` + `args` to the child's own argv. The wrapper applies
    /// the profile and execs through to the real program.
    Seatbelt {
        program: PathBuf,
        args: Vec<OsString>,
    },
}

/// Why a child's network cannot be denied here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NoChildNetworkDenial(String);

impl NoChildNetworkDenial {
    /// Public so a caller can construct the unavailable case in its own tests:
    /// the branch that matters most is the one the host it is built on cannot
    /// reach.
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

/// The `sandbox-exec` invocation that wraps a child in
/// [`SEATBELT_DENY_NETWORK_PROFILE`].
///
/// Pure and platform-independent so the argv is compiled and tested on every
/// host, not only the one that can run it.
pub fn seatbelt_denial(sandbox_exec: &Path) -> ChildNetworkDenial {
    ChildNetworkDenial::Seatbelt {
        program: sandbox_exec.to_path_buf(),
        args: vec![
            OsString::from("-p"),
            OsString::from(SEATBELT_DENY_NETWORK_PROFILE),
            // Everything after `--` is the command, so a plugin argument that
            // looks like a sandbox-exec flag cannot be read as one.
            OsString::from("--"),
        ],
    }
}

/// The mechanism to use for a child that must not reach the network, or why
/// there is none.
///
/// Resolved per call rather than cached: the answer depends on a file existing,
/// and a caller that fails closed on `Err` should not be held to an answer from
/// process startup.
#[cfg(target_os = "linux")]
pub fn child_network_denial() -> Result<ChildNetworkDenial, NoChildNetworkDenial> {
    Ok(ChildNetworkDenial::Seccomp)
}

/// See the Linux variant. macOS goes through the system Seatbelt launcher; a
/// custom sandbox profile that denies the agent itself read/exec on
/// [`SANDBOX_EXEC`] makes the wrapped spawn fail rather than silently run
/// unconfined (every built-in profile grants `/usr` read).
#[cfg(target_os = "macos")]
pub fn child_network_denial() -> Result<ChildNetworkDenial, NoChildNetworkDenial> {
    let sandbox_exec = Path::new(SANDBOX_EXEC);
    if !sandbox_exec.is_file() {
        return Err(NoChildNetworkDenial::new(format!(
            "{SANDBOX_EXEC} is missing, so a child's network cannot be denied"
        )));
    }
    Ok(seatbelt_denial(sandbox_exec))
}

/// See the Linux variant. Nothing here confines a child's network: Windows
/// would need an AppContainer (which also rewrites the child's filesystem
/// access) or an administrator-installed firewall rule, and the other Unixes
/// have neither Landlock nor Seatbelt.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn child_network_denial() -> Result<ChildNetworkDenial, NoChildNetworkDenial> {
    Err(NoChildNetworkDenial::new(
        "this platform has no per-child network confinement (seccomp is Linux-only, \
         Seatbelt is macOS-only)",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seatbelt_profile_denies_every_network_operation_and_nothing_else() {
        // `network*` (not `network-outbound`) so a bind/listen is denied too,
        // matching the syscalls the Linux filter blocks.
        assert!(SEATBELT_DENY_NETWORK_PROFILE.contains("(deny network*)"));
        assert!(SEATBELT_DENY_NETWORK_PROFILE.contains("(allow default)"));
        // No DNS escape hatch: macOS resolves through a Unix socket that
        // `(deny network*)` covers, and re-allowing it would resolve names on
        // macOS that do not resolve on Linux.
        assert!(!SEATBELT_DENY_NETWORK_PROFILE.contains("mDNSResponder"));
    }

    #[test]
    fn seatbelt_argv_ends_at_a_double_dash() {
        let ChildNetworkDenial::Seatbelt { program, args } =
            seatbelt_denial(Path::new(SANDBOX_EXEC))
        else {
            panic!("expected a Seatbelt denial");
        };
        assert_eq!(program, Path::new(SANDBOX_EXEC));
        assert_eq!(args.first().unwrap(), "-p");
        assert_eq!(args[1], OsString::from(SEATBELT_DENY_NETWORK_PROFILE));
        assert_eq!(
            args.last().unwrap(),
            "--",
            "the plugin's own argv must start after `--`"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_denies_with_seccomp() {
        assert_eq!(child_network_denial().unwrap(), ChildNetworkDenial::Seccomp);
    }
}
