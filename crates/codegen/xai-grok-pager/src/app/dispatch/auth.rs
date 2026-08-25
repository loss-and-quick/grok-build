//! Login, logout, account switching, and auth-code submission dispatchers.

use super::ctx::{restore_auth_return_view, show_welcome};
use super::queue::{maybe_drain_queue, note_peek_page_flip};
use super::router::dispatch;
use super::session::lifecycle::{clear_startup_actions, drain_startup_actions};
use crate::app::actions::{Action, Effect};
use crate::app::agent::AgentId;
use crate::app::agent_view::AgentView;
use crate::app::app_view::{ActiveView, AppView, AuthMode, AuthState};
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::SessionEvent;
use agent_client_protocol as acp;

// ---------------------------------------------------------------------------
// Auth dispatch
// ---------------------------------------------------------------------------

/// `/logout` -- ask the shell to clear auth, then return to the login screen.
pub(super) fn dispatch_logout(_app: &mut AppView) -> Vec<Effect> {
    vec![Effect::Logout]
}

/// Ensure `login_method_id` is populated from stored auth methods.
/// On the eager-auth path (cached token), login_method_id is never set
/// because the user skipped the login screen.
///
/// Does **not** invent `grok.com` when no interactive method is advertised
/// (e.g. `preferred_method=api_key` with no key — empty `auth_methods`).
/// Callers already surface "No login method available" when this leaves
/// `login_method_id` unset.
pub(super) fn ensure_login_method(app: &mut AppView) {
    if app.login_method_id.is_some() {
        return;
    }
    // No interactive method: leave login_method_id unset (fail-closed).
    let Some(method) = app
        .auth_methods
        .iter()
        .find(|method| is_interactive(method))
        .cloned()
    else {
        return;
    };
    adopt_login_method(app, &method);
}

/// Whether `method` can be started from the login screen at all.
fn is_interactive(method: &acp::AuthMethod) -> bool {
    xai_grok_shell::agent::auth_method::AuthMethodKind::from_id(method.id())
        .needs_interactive_login()
}

/// Adopt `method` as the login method: label, id, and start mode.
///
/// The single source of truth for that assignment — shared by the implicit
/// single-method path ([`ensure_login_method`]) and the explicit
/// picker-chosen path ([`dispatch_choose_auth_method`]) so the two cannot
/// drift. External auth providers launch a command; everything else starts in
/// the pending flow.
fn adopt_login_method(app: &mut AppView, method: &acp::AuthMethod) {
    app.login_label = Some(method.name().to_string());
    app.login_method_id = Some(method.id().clone());
    app.auth_start_mode = auth_start_mode_for(method);
}

/// The start mode a login with `method` runs in. External auth providers launch
/// a command; everything else — including a plugin-provided sign-in, which
/// drives this same screen through the shell's URL/code channels — starts
/// pending and is refined by the auth-URL response.
fn auth_start_mode_for(method: &acp::AuthMethod) -> AuthMode {
    let is_external_provider = method
        .meta()
        .as_ref()
        .and_then(|meta| meta.get("external_provider"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_external_provider {
        AuthMode::Command
    } else {
        AuthMode::Pending
    }
}

/// Open the login-method picker when there is a real choice to make.
///
/// Returns `true` when the picker took over and the caller must not start an
/// auth flow. With zero or one interactive method there is nothing to pick, so
/// this is a no-op and the caller behaves exactly as before.
///
/// Deliberately does **not** touch `auth_state`: an in-flight
/// `Authenticating` holds the abort handle for the running auth task, and
/// dropping it here would orphan that task past [`abort_prior_auth`].
fn open_auth_method_picker(app: &mut AppView, switch_account: bool) -> bool {
    if app
        .auth_methods
        .iter()
        .filter(|m| is_interactive(m))
        .count()
        < 2
    {
        return false;
    }
    app.auth_method_picker = Some(crate::views::auth_method_modal::AuthMethodPickerState::new(
        &app.auth_methods,
        // The credential in use, reported by the agent. Distinct from
        // `login_method_id`, which is only the default a login would start and
        // would mis-badge a machine that has never logged in.
        app.current_auth_method_id.as_ref(),
        app.login_method_id.as_ref(),
        switch_account,
    ));
    // Only the welcome view renders the picker, so surface it the same way
    // `dispatch_login` surfaces the auth UI. `auth_return_view` is what
    // `Action::CloseAuthMethodPicker` restores from.
    if !matches!(app.active_view, ActiveView::Welcome) {
        app.auth_return_view = Some(app.active_view);
        show_welcome(app);
    }
    true
}

/// Error when no interactive login method is available (empty auth_methods,
/// e.g. `preferred_method=api_key` with no credentials). Prefer the shell's
/// pin-unavailable copy when the list is empty.
fn no_login_method_error(app: &AppView) -> String {
    if app.auth_methods.is_empty() {
        xai_grok_shell::agent::auth_method::PREFERRED_API_KEY_UNAVAILABLE.to_string()
    } else {
        "No login method available".to_string()
    }
}

/// Abort any in-flight Authenticate/SwitchAccount task *and* its URL poll so a
/// new login cannot stack device-code mints or have a stale poll steal the
/// successor's URL (single-flight). No-op when not authenticating or when the
/// abort handles have not been installed yet.
fn abort_prior_auth(app: &mut AppView) {
    if let AuthState::Authenticating {
        handle,
        request_seq,
        ..
    } = &mut app.auth_state
        && let Some(h) = handle.take()
    {
        tracing::debug!(
            request_seq,
            "aborting prior in-flight auth task for single-flight"
        );
        h.abort();
    }
    if let Some((seq, h)) = app.auth_url_poll_handle.take() {
        tracing::debug!(
            request_seq = seq,
            "aborting prior auth URL poll for single-flight"
        );
        h.abort();
    }
}

/// Start the auth flow for `method_id`, aborting any prior in-flight attempt.
///
/// `switch_account` selects the flow: `true` logs out first
/// ([`Effect::SwitchAccount`]), `false` authenticates in place
/// ([`Effect::Authenticate`]). It is always passed in by the caller, never
/// inferred from `auth_state`.
fn start_auth_flow(
    app: &mut AppView,
    method_id: acp::AuthMethodId,
    switch_account: bool,
) -> Vec<Effect> {
    abort_prior_auth(app);

    let request_seq = app.next_auth_request_seq;
    app.next_auth_request_seq += 1;
    app.auth_code_input.reset();
    app.auth_state = AuthState::Authenticating {
        request_seq,
        handle: None,
        auth_url: None,
        mode: app.auth_start_mode,
    };

    let start = if switch_account {
        Effect::SwitchAccount {
            request_seq,
            method_id,
            use_oauth: app.auth_use_oauth,
        }
    } else {
        Effect::Authenticate {
            request_seq,
            method_id,
            use_oauth: app.auth_use_oauth,
            force_interactive: true,
        }
    };
    // Every interactive method polls for the URL, plugin sign-ins included: the
    // shell registers the same single-flight attempt for them, and the plugin
    // publishes its authorize URL into it (`auth_publish_url`), so this screen
    // is what renders the URL and collects the code.
    vec![start, Effect::PollAuthUrl { request_seq }]
}

/// Log out, then start a new login flow in a single sequential task.
pub(super) fn dispatch_switch_account(app: &mut AppView) -> Vec<Effect> {
    ensure_login_method(app);

    if open_auth_method_picker(app, true) {
        return vec![];
    }

    let Some(method_id) = app.login_method_id.clone() else {
        app.auth_state = AuthState::Pending {
            error: Some(no_login_method_error(app)),
        };
        return vec![];
    };

    start_auth_flow(app, method_id, true)
}

/// User picked a method in the login-method picker. Adopts it, then runs the
/// same flow the direct `/login` and `/switch-account` paths run.
pub(super) fn dispatch_choose_auth_method(
    app: &mut AppView,
    method_id: acp::AuthMethodId,
    switch_account: bool,
) -> Vec<Effect> {
    app.auth_method_picker = None;

    let Some(method) = app
        .auth_methods
        .iter()
        .find(|method| method.id() == &method_id)
        .cloned()
    else {
        app.auth_state = AuthState::Pending {
            error: Some("Login method is no longer available".to_string()),
        };
        return vec![];
    };
    adopt_login_method(app, &method);

    // The picker already stashed `auth_return_view` and switched to welcome if
    // it was opened from a session, so the auth UI is visible either way —
    // including for a plugin sign-in, which drives that same screen.
    surface_auth_ui(app);

    start_auth_flow(app, method_id, switch_account)
}

/// Dismiss the login-method picker without choosing. Returns the user to
/// whatever view the picker was opened from by reusing the cancel-login
/// restore, so `auth_return_view` can never be left stale (a stale value also
/// disables foreign-session polling).
pub(super) fn dispatch_close_auth_method_picker(app: &mut AppView) -> Vec<Effect> {
    app.auth_method_picker = None;
    dispatch_cancel_login(app)
}

/// Scan the trailing run of session-event / system blocks for a
/// [`SessionEvent::ReAuthRequired`] prompt. Used by the `PromptResponse`
/// handler to suppress the redundant "Turn failed" block after a 401 — the
/// re-auth prompt is pushed by the `RetryState` handler, which runs first.
pub(super) fn scrollback_has_recent_reauth_prompt(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback)
        .any(|(_, ev)| matches!(ev, SessionEvent::ReAuthRequired { .. }))
}

/// True if the trailing run of session/system blocks contains a terminal
/// context-overflow block ([`SessionEvent::ContextTooLarge`] or `CompactionFailed`).
/// Lets `PromptResponse` suppress the redundant `TurnFailed`, mirroring reauth.
pub(super) fn scrollback_has_recent_context_too_large(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback).any(|(_, ev)| {
        matches!(
            ev,
            SessionEvent::ContextTooLarge | SessionEvent::CompactionFailed { .. }
        )
    })
}

pub(crate) fn scrollback_has_recent_disk_full(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback).any(|(_, ev)| matches!(ev, SessionEvent::DiskFull))
}

/// True if the trailing run already has a dedicated terminal error banner that
/// replaces `TurnFailed` (re-auth, context overflow, disk-full, formatted
/// request failure). `CompactionFailed` is deliberately excluded — it can
/// appear mid-turn, and a stale one must not swallow an unrelated error's
/// only surface on the reconcile/viewer rails.
pub(in crate::app) fn scrollback_has_recent_error_banner(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback).any(|(_, ev)| {
        matches!(
            ev,
            SessionEvent::ReAuthRequired { .. }
                | SessionEvent::ContextTooLarge
                | SessionEvent::DiskFull
                | SessionEvent::RequestFailed { .. }
        )
    })
}

/// True if the trailing run already has a formatted [`SessionEvent::RequestFailed`]
/// banner. Lets `PromptResponse` skip the redundant `TurnFailed`. Deliberately
/// does NOT match `RetryFailed`: the special cases that keep it (legacy_auth,
/// encrypted_content_mismatch) keep their pre-existing marker behavior.
pub(super) fn scrollback_has_recent_request_failed(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    trailing_session_events(scrollback)
        .any(|(_, ev)| matches!(ev, SessionEvent::RequestFailed { .. }))
}

/// The trailing run of session events, newest first: yields `(index, event)`
/// for each session-event block at the tail of the scrollback, skipping
/// interleaved system messages and stopping at the first substantive block.
/// Banners for the finishing turn live in this run — pushed just before its
/// `PromptResponse` arrived.
pub(super) fn trailing_session_events(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> impl Iterator<Item = (usize, &SessionEvent)> {
    use crate::scrollback::block::RenderBlock;
    (0..scrollback.len())
        .rev()
        .map(|idx| (idx, scrollback.entry(idx).map(|e| &e.block)))
        .take_while(|(_, block)| {
            matches!(
                block,
                Some(RenderBlock::SessionEvent(_) | RenderBlock::System(_))
            )
        })
        .filter_map(|(idx, block)| match block {
            Some(RenderBlock::SessionEvent(ev)) => Some((idx, &ev.event)),
            _ => None,
        })
}

/// Strip the trailing run of auth-error blocks — the `ReAuthRequired`
/// prompt plus any stale `RetryFailed` / `TurnFailed` — from an agent's
/// scrollback. Called after a successful mid-session re-auth so the prompt
/// disappears once the user returns to the session. Mirrors the
/// credit-limit upsell's stale-block strip.
pub(super) fn strip_trailing_auth_error_blocks(agent: &mut AgentView) {
    let to_remove: Vec<usize> = trailing_session_events(&agent.scrollback)
        .filter(|(_, ev)| {
            matches!(
                ev,
                SessionEvent::ReAuthRequired { .. }
                    | SessionEvent::RequestFailed { .. }
                    | SessionEvent::RetryFailed { .. }
                    | SessionEvent::TurnFailed { .. }
            )
        })
        .map(|(idx, _)| idx)
        .collect();
    for idx in to_remove {
        agent.scrollback.remove_from(idx);
    }
}

/// Start an interactive login flow. Triggered by pressing 'l' on the
/// welcome screen or by the `/login` slash command.
///
/// When invoked mid-session (the active view is an agent/dashboard rather
/// than the welcome screen), the auth UI — including the external auth
/// provider's sign-in URL and status — is only rendered by the welcome
/// view. We therefore stash the caller's view in `auth_return_view` and
/// switch to `Welcome` so the flow is actually visible; the prior view is
/// restored once auth completes or is cancelled. Without this, `/login`
/// with an external auth provider configured appeared to do nothing.
pub(super) fn dispatch_login(app: &mut AppView) -> Vec<Effect> {
    ensure_login_method(app);

    // More than one interactive method: let the user pick instead of silently
    // starting the default one.
    if open_auth_method_picker(app, false) {
        return vec![];
    }

    let Some(method_id) = app.login_method_id.clone() else {
        app.auth_state = AuthState::Pending {
            error: Some(no_login_method_error(app)),
        };
        return vec![];
    };

    surface_auth_ui(app);

    start_auth_flow(app, method_id, false)
}

/// Put the surface that will actually drive this login in front of the user:
/// the welcome screen's auth UI, so a mid-session `/login` detours there and
/// stashes `auth_return_view` to come back. Every interactive method lands here,
/// plugin sign-ins included — the plugin publishes its authorize URL onto this
/// screen and reads the code back from it.
fn surface_auth_ui(app: &mut AppView) {
    // `show_welcome` resets ephemeral state here, covering the AuthComplete /
    // cancel-login fallbacks too (`auth_return_view` is only ever set here and
    // by the picker, which routes back through the same restore).
    if !matches!(app.active_view, ActiveView::Welcome) {
        app.auth_return_view = Some(app.active_view);
        show_welcome(app);
    }
}

/// Cancel a login that was started from inside a session and restore the
/// caller's view. Only meaningful when `auth_return_view` is set (a
/// mid-session `/login` or 401 re-auth prompt). Aborts the in-flight auth
/// task and tells the shell to cancel its device/loopback flow so a retry
/// does not race a still-polling prior mint. Bump the seq so a fresh login
/// does not collide with a late `AuthComplete`/`AuthFailed`.
pub(super) fn dispatch_cancel_login(app: &mut AppView) -> Vec<Effect> {
    let Some(return_view) = app.auth_return_view.take() else {
        return vec![];
    };
    // Capture the attempt's request_seq before abort clears Authenticating so
    // the shell cancel is scoped to this attempt only (a delayed RPC must not
    // cancel a fast re-login).
    let cancel_seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => Some(*request_seq),
        _ => None,
    };
    abort_prior_auth(app);
    app.next_auth_request_seq += 1;
    app.auth_state = AuthState::Done;
    app.auth_show_raw_url = false;
    app.auth_code_input.reset();
    restore_auth_return_view(app, return_view);
    // The user bailed out of re-auth — drop stashed prompts and strip the
    // stale re-auth prompt from scrollback (on all agents: the login may
    // have been started from the dashboard). Clearing the stash alone is
    // not enough: a leftover `ReAuthRequired` block would let a later
    // `PromptResponse` re-detect it via `scrollback_has_recent_reauth_prompt`
    // and re-stash the prompt, so a subsequent unrelated login could
    // silently resubmit it. Mirrors the strip in the `AuthComplete` path.
    for agent in app.agents.values_mut() {
        agent.reauth_stashed_prompt = None;
        strip_trailing_auth_error_blocks(agent);
    }
    // Ask the shell to cancel its in-flight interactive auth (device poll /
    // loopback wait). Fire-and-forget: UI state is already restored.
    match cancel_seq {
        Some(request_seq) => vec![Effect::CancelAuth { request_seq }],
        None => vec![],
    }
}

/// User submitted a manually-pasted auth token in loopback mode.
pub(super) fn dispatch_submit_auth_code(app: &mut AppView, code: String) -> Vec<Effect> {
    let request_seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        _ => return vec![],
    };

    vec![Effect::SubmitAuthCode { request_seq, code }]
}

// TaskResult handlers.

pub(super) fn handle_auth_complete(
    app: &mut AppView,
    request_seq: u64,
    meta: Option<serde_json::Value>,
) -> Vec<Effect> {
    if let AuthState::Authenticating {
        request_seq: current_seq,
        ..
    } = &app.auth_state
        && *current_seq == request_seq
    {
        if let Some(meta_val) = meta.as_ref()
            && let Ok(auth_meta) =
                serde_json::from_value::<xai_grok_shell::auth::AuthMeta>(meta_val.clone())
        {
            app.apply_auth_meta(&auth_meta);
        }

        app.auth_state = AuthState::Done;
        app.auth_show_raw_url = false;
        app.welcome_prompt_focused = !app.is_access_blocked();
        app.auth_code_input.reset();

        // Mid-session re-auth (`/login` or a 401 prompt): restore the
        // view the user was on instead of running the startup
        // load-session flow. The session state lives in `app.agents`,
        // independent of `active_view`, so it is preserved across the
        // auth detour.
        if let Some(return_view) = app.auth_return_view.take() {
            restore_auth_return_view(app, return_view);
            // Mid-session re-auth returns to the existing session, NOT
            // the startup flow, so discard any deferred startup stash
            // (e.g. an incidental `Ctrl+N` pressed during /login that the
            // chokepoint deferred) rather than leaving it to fire later.
            clear_startup_actions(app);
            // Re-auth succeeded — hide the now-stale re-auth prompt
            // (and any trailing error blocks) so the user returns to
            // a clean session. Mirrors the credit-limit upsell's
            // stale-block strip.
            // Auth is global, so handle every agent (the login may
            // have been started from the dashboard, not the agent
            // that 401'd).
            let mut retry_effects = Vec::new();
            let mut page_flips = Vec::new();
            for agent in app.agents.values_mut() {
                strip_trailing_auth_error_blocks(agent);
                // Auto-resubmit the prompt that failed on the expired
                // login so the user doesn't have to retype it. The
                // user couldn't have queued another prompt during the
                // auth detour, so a plain front-enqueue + drain is safe.
                if let Some(prompt) = agent.reauth_stashed_prompt.take() {
                    agent.scrollback.push_block(RenderBlock::system(
                        "Re-authenticated. Retrying\u{2026}".to_string(),
                    ));
                    agent.session.enqueue_in_flight_prompt_front(prompt);
                    let drain = maybe_drain_queue(agent);
                    retry_effects.extend(drain.effects);
                    page_flips.push((agent.session.id, drain.page_flip_entry));
                }
            }
            for (id, page_flip_entry) in page_flips {
                note_peek_page_flip(app, id, page_flip_entry);
            }
            let mut effects = dispatch(Action::RequestBundleStatus, app);
            if app.usage_visible {
                effects.push(Effect::FetchAppBilling);
            }
            effects.extend(retry_effects);
            return effects;
        }

        // status only; shell auto-syncs post-auth
        let mut effects = dispatch(Action::RequestBundleStatus, app);

        // Start auto-checking subscription if gated.
        // Check immediately (don't wait 5s) then schedule the timer.
        if !app.has_access() {
            app.paywall_check_started = Some(std::time::Instant::now());
            effects.push(Effect::CheckSubscription { verify: None });
            effects.push(Effect::SchedulePaywallCheck);
        }
        // Fetch billing so the welcome screen can show a credit warning.
        if app.usage_visible {
            effects.push(Effect::FetchAppBilling);
        }
        // Fetch changelog (mirrors startup path for interactive login).
        effects.push(Effect::FetchChangelog);

        // ZDR-blocked users stay on the welcome screen — discard any
        // deferred startup (they cannot start a session).
        if app.is_zdr_blocked() {
            clear_startup_actions(app);
            return effects;
        }

        // Replay deferred session startup once BOTH gates are open. Auth
        // is now Done, so `session_startup_allowed()` here means "is trust
        // also resolved?" -- if trust is still Pending its question renders
        // next and its answer drains instead. Same predicate the trust
        // handlers use, so the deferred startup runs exactly once after
        // whichever gate resolves last.
        if app.session_startup_allowed() {
            effects.extend(drain_startup_actions(app));
        }
        return effects;
    }
    vec![]
}

pub(super) fn handle_auth_url_ready(
    app: &mut AppView,
    request_seq: u64,
    auth_url: Option<String>,
    external: bool,
    mode: Option<String>,
) -> Vec<Effect> {
    if let AuthState::Authenticating {
        request_seq: current_seq,
        auth_url: current_url,
        mode: current_mode,
        ..
    } = &mut app.auth_state
        && *current_seq == request_seq
    {
        *current_url = auth_url;
        // Prefer `mode`; fall back to `external` for older agents. An
        // old-agent device login lands on Loopback (harmless paste box;
        // the background poll still completes).
        *current_mode = match mode.as_deref() {
            Some("device") => AuthMode::Device,
            Some("command") => AuthMode::Command,
            Some("loopback") => AuthMode::Loopback,
            _ if external => AuthMode::Command,
            _ => AuthMode::Loopback,
        };
    }
    vec![]
}

pub(super) fn handle_mcp_auth_trigger_done(
    app: &mut AppView,
    agent_id: AgentId,
    server_name: String,
    result: Result<crate::app::actions::McpAuthTriggerOutcome, String>,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    if let Some(ref mut modal) = agent.extensions_modal {
        modal.pending_action = None;
        modal.pending_entry_index = None;
        match result {
            Ok(crate::app::actions::McpAuthTriggerOutcome::Authenticated) => {}
            Ok(crate::app::actions::McpAuthTriggerOutcome::SetupRequired(setup)) => {
                let setup_values = match &modal.mcps_data {
                    crate::views::extensions_modal::TabDataState::Loaded(servers) => servers
                        .iter()
                        .find(|server| server.name == server_name)
                        .map(|server| server.setup_values.clone())
                        .unwrap_or_default(),
                    _ => std::collections::HashMap::new(),
                };
                if let Some(form) = crate::views::extensions_modal::McpSetupFormState::from_setup(
                    server_name.clone(),
                    setup,
                    setup_values,
                ) {
                    modal.mcp_setup = Some(form);
                } else {
                    modal.modal_message =
                        Some(crate::views::extensions_modal::ModalMessage::Error(
                            format!("{server_name}: setup schema is not supported in this UI"),
                        ));
                }
                return vec![];
            }
            Err(e) => {
                let msg = if e.starts_with("To authenticate") {
                    format!("{server_name}: {e}")
                } else if e.contains(&server_name) {
                    format!("Auth failed: {e}")
                } else {
                    format!("{server_name} auth failed: {e}")
                };
                modal.modal_message =
                    Some(crate::views::extensions_modal::ModalMessage::Error(msg));
                if let Some(session_id) = agent.session.session_id.clone() {
                    return vec![Effect::FetchMcpsList {
                        agent_id,
                        session_id,
                        cache: false,
                    }];
                }
                return vec![];
            }
        }
    }
    // No toast on success: the row transition from the FetchMcpsList
    // refresh below is the confirmation.
    let Some(session_id) = agent.session.session_id.clone() else {
        return vec![];
    };
    vec![Effect::FetchMcpsList {
        agent_id,
        session_id,
        cache: false,
    }]
}

pub(super) fn handle_mcp_setup_submit_done(
    app: &mut AppView,
    agent_id: AgentId,
    server_name: String,
    result: Result<(), String>,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    if let Some(ref mut modal) = agent.extensions_modal {
        if let Err(e) = result {
            modal.pending_action = None;
            modal.pending_entry_index = None;
            modal.modal_message = Some(crate::views::extensions_modal::ModalMessage::Error(
                format!("{server_name} setup failed: {e}"),
            ));
            return vec![];
        }
        modal.pending_action = Some(format!("Authenticating {server_name}..."));
        modal.pending_entry_index = None;
    }
    let Some(session_id) = agent.session.session_id.clone() else {
        if let Some(ref mut modal) = agent.extensions_modal {
            modal.pending_action = None;
            modal.modal_message = Some(crate::views::extensions_modal::ModalMessage::Error(
                format!("{server_name}: no active session for authentication"),
            ));
        }
        return vec![];
    };
    vec![Effect::McpAuthTrigger {
        agent_id,
        session_id,
        server_name,
    }]
}
