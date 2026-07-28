//! The exit affordance on the welcome screen's auth states — one definition of
//! what the user reads and what the key does.
//!
//! These two had drifted: the renderer hardcoded `("q", "Quit")` for
//! `AuthState::Pending` and `ctrl+q  quit` for `AuthState::Authenticating`,
//! while the key handler dispatched [`Action::CancelLogin`] whenever the login
//! came from inside a session. A user mid-session read "Quit" as "lose my
//! session" and never pressed it, so an auth error looked like a dead end
//! offering only retry-or-quit.
//!
//! Everything that describes or performs the exit now comes from
//! [`auth_exit_entry`], so a future change to one is a change to both.

use crate::app::actions::Action;

/// The exit affordance for an auth screen.
pub(crate) struct AuthExitEntry {
    /// Key advertised on the `Pending` menu row, which lists plain letters.
    pub menu_key: &'static str,
    /// Key advertised on the `Authenticating` hint row, which lists chords.
    pub hint_key: &'static str,
    /// What the entry does, in the menu row's title case. The hint row
    /// lowercases it to match its neighbours.
    pub label: &'static str,
    /// The action every bound exit key dispatches.
    pub action: Action,
}

/// The exit entry for an auth screen.
///
/// `mid_session_login` is true when the login was started from inside a session
/// (`auth_return_view` is set), in which case leaving returns to that session
/// instead of quitting the app — which is what the label must say.
pub(crate) fn auth_exit_entry(mid_session_login: bool) -> AuthExitEntry {
    if mid_session_login {
        AuthExitEntry {
            menu_key: "esc",
            hint_key: "esc",
            label: "Back to session",
            action: Action::CancelLogin,
        }
    } else {
        AuthExitEntry {
            menu_key: "q",
            hint_key: if super::welcome_in_vscode_family() {
                "ctrl+d"
            } else {
                "ctrl+q"
            },
            label: "Quit",
            action: Action::QuitConfirmed,
        }
    }
}
