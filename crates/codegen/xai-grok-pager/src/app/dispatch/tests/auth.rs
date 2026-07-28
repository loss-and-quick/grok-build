//! Tests for login, logout, account switching, and auth-code dispatchers.

use super::*;

#[test]
fn cta_mcps_loaded_needs_auth_opens_modal_and_seeds() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::extensions_modal::{ExtensionsTab, TabDataState};
    use crate::views::mcps_modal::{McpSectionId, McpServerDisplayStatus, section_key};
    let mut app = test_app_with_agent();
    app.team_id = Some("team-uuid".into());
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::AwaitingMcps {
        name: "figma".into(),
    };
    let servers = vec![
        cta_mcp_server("grok_com_managed", None, McpServerDisplayStatus::Ready),
        cta_mcp_server("local-srv", None, McpServerDisplayStatus::Ready),
        cta_mcp_server("other-srv", Some("slack"), McpServerDisplayStatus::Ready),
        cta_mcp_server(
            "figma-srv",
            Some("figma"),
            McpServerDisplayStatus::NeedsAuth,
        ),
    ];
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(servers),
        }),
        &mut app,
    );
    // Handoff complete: CTA settles to Hidden.
    assert_eq!(app.agents[&id].plugin_cta.phase, CtaPhase::Hidden);
    // Modal opened to the MCP Servers tab.
    let modal = app.agents[&id]
        .extensions_modal
        .as_ref()
        .expect("extensions modal should be open");
    assert_eq!(modal.active_tab, ExtensionsTab::McpServers);
    // Session team id seeded so the Managed subtitle deep link matches Ctrl+O.
    assert_eq!(modal.session_team_id.as_deref(), Some("team-uuid"));
    // MCP tab seeded directly from the read we already have (no flash).
    match &modal.mcps_data {
        TabDataState::Loaded(servers) => assert_eq!(servers.len(), 4),
        other => panic!("expected mcps_data Loaded, got {other:?}"),
    }
    // Managed + Local + other plugins collapsed; only target expanded.
    let collapsed = &modal.mcps_collapsed_sections;
    assert!(collapsed.contains(&section_key(&McpSectionId::Managed)));
    assert!(collapsed.contains(&section_key(&McpSectionId::Local)));
    assert!(collapsed.contains(&section_key(&McpSectionId::Plugin("slack".into()))));
    assert!(!collapsed.contains(&section_key(&McpSectionId::Plugin("figma".into()))));
    assert!(modal.mcps_section_collapse_initialized);
    // Emits the SAME full tab fetch-set as a manual open so no tab is stuck
    // Loading, plus the candidate refresh.
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchHooksList { .. }))
            .count(),
        1
    );
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchPluginsList { .. }))
            .count(),
        1
    );
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchMarketplaceList { .. }))
            .count(),
        1
    );
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchMcpsList { .. }))
            .count(),
        1
    );
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchSkillsList { .. }))
            .count(),
        1
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchPluginCtaCatalog { .. }))
    );
}

#[test]
fn cta_mcps_loaded_no_needs_auth_terminal_sets_installed() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
    }
    // Plugin server present and Ready (terminal, no auth) -> settle now.
    let servers = vec![cta_mcp_server(
        "figma-srv",
        Some("figma"),
        McpServerDisplayStatus::Ready,
    )];
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(servers),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Installed {
            name: "figma".into()
        }
    );
    assert!(app.agents[&id].extensions_modal.is_none());
    // No modal repopulation; settle emits the auto-dismiss timer + candidate
    // refresh, and never re-probes.
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::FetchMcpsList { .. }))
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::RetryPluginCtaMcps { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DismissCtaInstalled { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchPluginCtaCatalog { .. }))
    );
}

#[test]
fn cta_mcps_loaded_later_needs_auth_opens_handoff() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
        // Several polls already elapsed before the server reached NeedsAuth.
        cta.mcp_attempt = 5;
    }
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(vec![cta_mcp_server(
                "figma-srv",
                Some("figma"),
                McpServerDisplayStatus::NeedsAuth,
            )]),
        }),
        &mut app,
    );
    // NeedsAuth is terminal: hand off immediately even mid-poll.
    assert_eq!(app.agents[&id].plugin_cta.phase, CtaPhase::Hidden);
    assert!(app.agents[&id].extensions_modal.is_some());
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::RetryPluginCtaMcps { .. }))
    );
}

// ── agent-bound kinds (bash) ─────────

/// A bash command typed while a turn is RUNNING takes the
/// server-authoritative immediate path (Effect + optimistic echo, no local
/// queue entry).
#[test]
fn bash_while_running_is_server_authoritative() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let effects = dispatch(Action::SendBashCommand("ls -la".into()), &mut app);
    let pid = match &effects[0] {
        Effect::SendBashCommand {
            command, prompt_id, ..
        } => {
            assert_eq!(command, "ls -la");
            prompt_id.clone()
        }
        other => panic!("expected immediate SendBashCommand, got {other:?}"),
    };
    // Not in the local queue.
    assert_eq!(app.agents[&id].session.queue_len(), 0);
    // Optimistic echo present with kind="bash".
    let q = app
        .shared_prompt_queue("test-session")
        .expect("echo present");
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].id, pid);
    assert_eq!(q[0].kind, "bash");
    assert_eq!(q[0].text, "ls -la");
}

#[test]
fn auth_complete_triggers_bundle_status_fetch() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: None,
        }),
        &mut app,
    );

    assert!(matches!(app.auth_state, AuthState::Done));
    // Pager only refreshes the on-disk catalog snapshot; the actual
    // bundle download now runs inside the shell post-auth.
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchBundleStatus))
    );
}

#[test]
fn auth_complete_with_deferred_load_also_fetches_status() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    app.deferred_startup.session =
        Some(crate::app::session_startup::DeferredSessionStartup::Load {
            session_id: "test-session".into(),
            session_cwd: None,
            chat_kind: false,
        });

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: None,
        }),
        &mut app,
    );

    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchBundleStatus))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadSession { .. }))
    );
    assert!(app.deferred_startup.session.is_none());
}

/// `/login` from the welcome screen (startup / logged-out) must NOT
/// stash a return view — the normal login-then-load flow is preserved.
#[test]
fn login_from_welcome_does_not_stash_return_view() {
    let mut app = test_app();
    assert_eq!(app.active_view, ActiveView::Welcome);

    dispatch(Action::Login, &mut app);

    assert_eq!(app.active_view, ActiveView::Welcome);
    assert_eq!(app.auth_return_view, None);
}

/// Compact-auth recovery: hold prompt across auto-compact 401, stash on
/// PromptResponse, resubmit on mid-session AuthComplete.
#[test]
fn e2e_compact_auth_failure_holds_prompt_and_resubmits_after_login() {
    use crate::app::acp_handler::apply_session_event_for_test;
    use crate::app::agent::{AgentState, InFlightPrompt};
    use crate::scrollback::EntryId;
    use crate::scrollback::block::RenderBlock;
    use xai_grok_shell::extensions::notification::{RetryState, SessionUpdate as XaiSessionUpdate};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.turn_started_at = Some(std::time::Instant::now());
        agent.session.session_id = Some(acp::SessionId::new("sess-compact-auth-e2e"));
        agent.session.current_prompt_id = Some("prompt-1".into());
        agent.session.in_flight_prompt = Some(InFlightPrompt {
            text: "please continue after login".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(1),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });

        apply_session_event_for_test(
            &XaiSessionUpdate::AutoCompactStarted {
                tokens_used: 180_000,
                context_window: 200_000,
                percentage: 90,
                reason: "threshold".into(),
            },
            &mut agent.session,
            &mut agent.scrollback,
        );
        assert!(
            agent.session.in_flight_prompt.is_none(),
            "cancel rewind must still be blocked mid-compact"
        );
        assert_eq!(
            agent
                .session
                .compact_held_prompt
                .as_ref()
                .map(|p| p.text.as_str()),
            Some("please continue after login"),
            "must hold the prompt text for reauth auto-resubmit"
        );

        apply_session_event_for_test(
            &XaiSessionUpdate::AutoCompactFailed {
                error: "authentication problem — re-authenticate using /login and retry.".into(),
            },
            &mut agent.session,
            &mut agent.scrollback,
        );
        assert!(agent.session.compact_held_prompt.is_some());

        apply_session_event_for_test(
            &XaiSessionUpdate::RetryState(RetryState::Failed {
                error_type: "auth".into(),
                message: "Unauthorized (401): compaction failed".into(),
            }),
            &mut agent.session,
            &mut agent.scrollback,
        );
        let has_reauth = (0..agent.scrollback.len()).any(|i| {
            matches!(
                agent.scrollback.entry(i).map(|e| &e.block),
                Some(RenderBlock::SessionEvent(ev))
                    if matches!(ev.event, SessionEvent::ReAuthRequired)
            )
        });
        assert!(has_reauth, "RetryState auth must show ReAuthRequired");
    }

    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("Unauthorized (401)".to_string()),
            http_status: Some(401),
            prompt_id: Some("prompt-1".into()),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id]
            .reauth_stashed_prompt
            .as_ref()
            .map(|p| p.text.as_str()),
        Some("please continue after login"),
        "PromptResponse must stash the compact-held prompt for AuthComplete"
    );

    dispatch(Action::Login, &mut app);
    let seq = authenticating_seq(&app);
    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: seq,
            meta: None,
        }),
        &mut app,
    );
    assert!(
        app.agents[&id].reauth_stashed_prompt.is_none(),
        "stash consumed on AuthComplete"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendPrompt { .. } | Effect::SendPromptBlocks { .. }
        )),
        "AuthComplete must resubmit the prompt so compact runs again with valid auth, got: {effects:?}"
    );
}

/// Without compact_held, clearing in_flight on compact start leaves reauth empty.
#[test]
fn pre_fix_compact_start_without_hold_cannot_stash_for_reauth() {
    use crate::app::agent::AgentState;
    use crate::scrollback::block::RenderBlock;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.turn_started_at = Some(std::time::Instant::now());
        agent.session.session_id = Some(acp::SessionId::new("sess-pre-fix"));
        agent.session.current_prompt_id = Some("p1".into());
        agent.session.in_flight_prompt = None;
        agent.session.compact_held_prompt = None;
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::ReAuthRequired));
    }
    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("Unauthorized (401)".to_string()),
            http_status: Some(401),
            prompt_id: Some("p1".into()),
        }),
        &mut app,
    );
    assert!(
        app.agents[&id].reauth_stashed_prompt.is_none(),
        "without compact_held / in_flight, reauth cannot stash — the pre-fix bug"
    );
}

/// A second auth-failed turn with no rewindable prompt
/// (`in_flight_prompt == None`) must not clobber the stash from an
/// earlier 401.
#[test]
fn second_auth_failure_does_not_clobber_reauth_stash() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.reauth_stashed_prompt = Some(crate::app::agent::InFlightPrompt {
            text: "first prompt".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::ReAuthRequired));
        agent.session.state = AgentState::TurnRunning;
        agent.turn_started_at = Some(std::time::Instant::now());
        agent.session.in_flight_prompt = None;
    }

    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("Unauthorized (401)".to_string()),
            http_status: Some(401),
            prompt_id: None,
        }),
        &mut app,
    );

    assert_eq!(
        app.agents[&id]
            .reauth_stashed_prompt
            .as_ref()
            .map(|prompt| prompt.text.as_str()),
        Some("first prompt"),
        "a None in_flight_prompt must not wipe an earlier stash"
    );
}

/// Cancelling a mid-session re-auth drops the stashed prompt so it is
/// not silently resubmitted on a later, unrelated login.
#[test]
fn cancel_login_drops_reauth_stashed_prompt() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().reauth_stashed_prompt =
        Some(crate::app::agent::InFlightPrompt {
            text: "stale".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });

    dispatch(Action::Login, &mut app);
    dispatch(Action::CancelLogin, &mut app);

    assert!(
        app.agents[&id].reauth_stashed_prompt.is_none(),
        "cancelling re-auth must drop the stashed prompt"
    );
}

/// Cancelling a mid-session re-auth strips the stale `ReAuthRequired`
/// prompt from scrollback so a later `PromptResponse` cannot re-detect
/// it and re-stash the prompt for silent resubmission.
#[test]
fn cancel_login_strips_reauth_prompt_from_scrollback() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.reauth_stashed_prompt = Some(crate::app::agent::InFlightPrompt {
            text: "stale".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::ReAuthRequired));
    }

    dispatch(Action::Login, &mut app);
    dispatch(Action::CancelLogin, &mut app);

    let sb = &app.agents[&id].scrollback;
    let has_reauth = (0..sb.len()).any(|i| {
        matches!(
            sb.entry(i).map(|e| &e.block),
            Some(RenderBlock::SessionEvent(ev)) if matches!(ev.event, SessionEvent::ReAuthRequired)
        )
    });
    assert!(
        !has_reauth,
        "cancelling re-auth must strip the stale re-auth prompt from scrollback"
    );
}

/// Empty `auth_methods` (preferred_method pin unavailable) must not invent
/// `grok.com` or start an OIDC flow the agent did not advertise.
#[test]
fn login_with_empty_auth_methods_fails_closed() {
    let mut app = test_app_with_agent();
    app.auth_methods.clear();
    app.login_method_id = None;

    let effects = dispatch(Action::Login, &mut app);

    assert!(
        effects.is_empty(),
        "must not start Authenticate without an advertised method"
    );
    assert_eq!(
        app.active_view,
        ActiveView::Agent(AgentId(0)),
        "must stay on the session view"
    );
    assert!(
        matches!(
            &app.auth_state,
            AuthState::Pending { error: Some(msg) }
                if msg.contains("preferred_method=api_key")
        ),
        "must surface pin-unavailable error, got {:?}",
        app.auth_state
    );
    assert!(app.login_method_id.is_none());
}

/// Puts the app in `Authenticating` with a live task's abort handle installed
/// (as the event loop would), returning the task's JoinHandle and the seq.
/// Callers assert the task actually gets aborted (`unwrap_err().is_cancelled()`),
/// not merely that the handle slot was cleared.
fn install_live_auth_task(
    app: &mut AppView,
    rt: &tokio::runtime::Runtime,
) -> (tokio::task::JoinHandle<()>, u64) {
    dispatch(Action::Login, app);
    let task = rt.spawn(std::future::pending::<()>());
    match &mut app.auth_state {
        AuthState::Authenticating {
            handle,
            request_seq,
            ..
        } => {
            *handle = Some(task.abort_handle());
            (task, *request_seq)
        }
        other => panic!("expected Authenticating after Login, got {other:?}"),
    }
}

fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

/// A second `/login` while already authenticating must abort the prior auth
/// task and bump the seq (single-flight: no stacked device-code mints).
#[test]
fn login_while_authenticating_aborts_prior_task() {
    let rt = test_runtime();
    let mut app = test_app_with_agent();
    let (prior_task, first_seq) = install_live_auth_task(&mut app, &rt);

    let effects = dispatch(Action::Login, &mut app);

    rt.block_on(async {
        assert!(
            prior_task.await.unwrap_err().is_cancelled(),
            "prior auth task must be aborted"
        );
    });
    match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => {
            assert!(
                *request_seq > first_seq,
                "re-login must bump request_seq for single-flight"
            );
        }
        other => panic!("expected Authenticating after re-Login, got {other:?}"),
    }
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Authenticate { .. })),
        "re-login must emit a new Authenticate"
    );
}

/// A stale `AuthComplete` (from an attempt whose abort lost the race because
/// the task had already finished) must not complete the new attempt: the
/// request-seq guard is the only protection here.
#[test]
fn stale_auth_complete_after_relogin_is_ignored() {
    let mut app = test_app_with_agent();
    dispatch(Action::Login, &mut app);
    let first_seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        other => panic!("expected Authenticating after Login, got {other:?}"),
    };
    dispatch(Action::Login, &mut app); // re-login bumps to seq2

    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: first_seq,
            meta: None,
        }),
        &mut app,
    );

    match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => {
            assert!(
                *request_seq > first_seq,
                "stale AuthComplete must leave the new attempt authenticating"
            );
        }
        other => panic!("stale AuthComplete must be ignored, got {other:?}"),
    }
}

/// Switch-account while authenticating goes through the same single-flight
/// abort as `/login` (sibling entry point).
#[test]
fn switch_account_while_authenticating_aborts_prior_task() {
    let rt = test_runtime();
    let mut app = test_app_with_agent();
    let (prior_task, first_seq) = install_live_auth_task(&mut app, &rt);

    dispatch(Action::SwitchAccount, &mut app);

    rt.block_on(async {
        assert!(
            prior_task.await.unwrap_err().is_cancelled(),
            "prior auth task must be aborted on switch-account"
        );
    });
    match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => {
            assert!(*request_seq > first_seq, "switch must bump request_seq");
        }
        other => panic!("expected Authenticating after SwitchAccount, got {other:?}"),
    }
}

/// Cancelling a mid-session login aborts the in-flight auth task (not just
/// restores the view) so a retry cannot race a still-polling prior mint.
#[test]
fn cancel_login_aborts_prior_task() {
    let rt = test_runtime();
    let mut app = test_app_with_agent();
    // Login from a session view stashes `auth_return_view`, making CancelLogin live.
    let (prior_task, _) = install_live_auth_task(&mut app, &rt);

    dispatch(Action::CancelLogin, &mut app);

    rt.block_on(async {
        assert!(
            prior_task.await.unwrap_err().is_cancelled(),
            "cancel must abort the in-flight auth task"
        );
    });
}

/// Cancelling a mid-session login returns to the session rather than
/// quitting the app, and clears the stashed view + auth state.
#[test]
fn cancel_login_restores_view() {
    let mut app = test_app_with_agent();
    dispatch(Action::Login, &mut app);
    assert_eq!(app.active_view, ActiveView::Welcome);
    let prior_seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        other => panic!("expected Authenticating after Login, got {other:?}"),
    };

    let effects = dispatch(Action::CancelLogin, &mut app);

    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelAuth { request_seq }] if *request_seq == prior_seq
        ),
        "cancel must tell the shell to stop the in-flight auth poll for this attempt"
    );
    assert_eq!(app.active_view, ActiveView::Agent(AgentId(0)));
    assert_eq!(app.auth_return_view, None);
    assert!(matches!(app.auth_state, AuthState::Done));
}

/// `CancelLogin` outside a mid-session login is a no-op (must not move
/// off the welcome screen or panic).
#[test]
fn cancel_login_noop_without_stashed_view() {
    let mut app = test_app();
    let effects = dispatch(Action::CancelLogin, &mut app);
    assert!(effects.is_empty());
    assert_eq!(app.active_view, ActiveView::Welcome);
    assert_eq!(app.auth_return_view, None);
}

#[test]
fn auth_complete_extracts_show_resolved_model_from_meta() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    assert!(app.show_resolved_model);

    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: Some(serde_json::json!({ "show_resolved_model": false })),
        }),
        &mut app,
    );

    assert!(!app.show_resolved_model);
}

#[test]
fn auth_complete_preserves_show_resolved_model_when_absent() {
    let mut app = test_app();
    app.show_resolved_model = false;
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };

    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: Some(serde_json::to_value(xai_grok_shell::auth::AuthMeta::default()).unwrap()),
        }),
        &mut app,
    );

    assert!(!app.show_resolved_model);
}

// ---------------------------------------------------------------------------
// Login-method picker
// ---------------------------------------------------------------------------

fn picker_auth_method(id: &str, name: &str) -> acp::AuthMethod {
    acp::AuthMethod::Agent(acp::AuthMethodAgent::new(
        acp::AuthMethodId::new(id),
        name.to_string(),
    ))
}

/// Advertise one non-interactive and two interactive methods, so a filter that
/// stopped working would change what the picker shows.
fn advertise_two_interactive_methods(app: &mut AppView) {
    app.auth_methods = vec![
        picker_auth_method("xai.api_key", "API key"),
        picker_auth_method("grok.com", "xAI"),
        picker_auth_method("plugin-oauth:example-auth", "Acme OAuth"),
    ];
    app.login_method_id = None;
    app.login_label = None;
}

fn open_picker(app: &AppView) -> crate::views::auth_method_modal::AuthMethodPickerState {
    app.auth_method_picker
        .clone()
        .expect("picker should be open")
}

/// One interactive method is not a choice: `/login` must start the flow
/// directly, exactly as before the picker existed.
#[test]
fn login_with_one_interactive_method_skips_the_picker() {
    let mut app = test_app_with_agent();
    app.auth_methods = vec![
        picker_auth_method("xai.api_key", "API key"),
        picker_auth_method("grok.com", "xAI"),
    ];
    app.login_method_id = None;

    let effects = dispatch(Action::Login, &mut app);

    assert!(
        app.auth_method_picker.is_none(),
        "one interactive method must not open a picker"
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::Authenticate { method_id, .. }, Effect::PollAuthUrl { .. }]
            if method_id.0.as_ref() == "grok.com"
    ));
}

/// Zero interactive methods stays fail-closed.
#[test]
fn login_with_no_interactive_method_skips_the_picker() {
    let mut app = test_app_with_agent();
    app.auth_methods = vec![picker_auth_method("xai.api_key", "API key")];
    app.login_method_id = None;

    let effects = dispatch(Action::Login, &mut app);

    assert!(app.auth_method_picker.is_none());
    assert!(effects.is_empty());
    assert!(matches!(
        app.auth_state,
        AuthState::Pending { error: Some(_) }
    ));
}

#[test]
fn login_with_two_interactive_methods_opens_the_picker() {
    let mut app = test_app_with_agent();
    advertise_two_interactive_methods(&mut app);

    let effects = dispatch(Action::Login, &mut app);

    assert!(effects.is_empty(), "the picker must not start auth itself");
    let picker = open_picker(&app);
    let ids: Vec<String> = picker
        .entries
        .iter()
        .map(|e| e.method_id.0.to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["grok.com", "plugin-oauth:example-auth"],
        "only interactive methods are offered"
    );
    assert!(!picker.switch_account, "/login authenticates in place");
    // Opened from a session: the welcome view is the only one that renders it.
    assert_eq!(app.active_view, ActiveView::Welcome);
    assert_eq!(app.auth_return_view, Some(ActiveView::Agent(AgentId(0))));
}

/// The picker defaults to the configured method rather than to any particular
/// kind, and badges nothing as current (the pager is not told which credential
/// is in use).
#[test]
fn picker_defaults_to_the_configured_method_and_badges_nothing() {
    let mut app = test_app_with_agent();
    advertise_two_interactive_methods(&mut app);
    app.login_method_id = Some(acp::AuthMethodId::new("plugin-oauth:example-auth"));

    dispatch(Action::Login, &mut app);

    let picker = open_picker(&app);
    assert_eq!(
        picker.selected_method_id().map(|id| id.0.to_string()),
        Some("plugin-oauth:example-auth".to_string())
    );
    assert!(
        picker.entries.iter().all(|e| !e.is_current),
        "a merely-defaulted method must not be badged as the current credential"
    );
}

/// Opening the picker must not clobber `auth_state`: an in-flight
/// `Authenticating` owns the abort handle for the running task, and dropping
/// it would orphan that task past `abort_prior_auth`.
#[test]
fn opening_the_picker_keeps_an_in_flight_auth_abortable() {
    let rt = test_runtime();
    let mut app = test_app_with_agent();
    let (prior_task, _) = install_live_auth_task(&mut app, &rt);
    app.auth_methods.push(picker_auth_method(
        "plugin-oauth:example-auth",
        "Acme OAuth",
    ));

    dispatch(Action::Login, &mut app);

    assert!(app.auth_method_picker.is_some());
    assert!(
        matches!(
            &app.auth_state,
            AuthState::Authenticating {
                handle: Some(_),
                ..
            }
        ),
        "the in-flight task's abort handle must survive opening the picker, got {:?}",
        app.auth_state
    );

    dispatch(
        Action::ChooseAuthMethod {
            method_id: acp::AuthMethodId::new("plugin-oauth:example-auth"),
            switch_account: false,
        },
        &mut app,
    );
    rt.block_on(async {
        assert!(
            prior_task.await.unwrap_err().is_cancelled(),
            "choosing a method must abort the prior in-flight auth task"
        );
    });
}

#[test]
fn choosing_a_method_authenticates_with_it_and_closes_the_picker() {
    let mut app = test_app_with_agent();
    advertise_two_interactive_methods(&mut app);
    dispatch(Action::Login, &mut app);

    let effects = dispatch(
        Action::ChooseAuthMethod {
            method_id: acp::AuthMethodId::new("plugin-oauth:example-auth"),
            switch_account: false,
        },
        &mut app,
    );

    assert!(app.auth_method_picker.is_none());
    // No `PollAuthUrl`: `x.ai/auth/get_url` is only ever answered by the
    // shell's own flows, so polling it for a plugin sign-in just times out into
    // the paste-a-token screen.
    assert!(matches!(
        effects.as_slice(),
        [Effect::Authenticate { method_id, .. }]
            if method_id.0.as_ref() == "plugin-oauth:example-auth"
    ));
    assert_eq!(
        app.login_method_id.as_ref().map(|id| id.0.to_string()),
        Some("plugin-oauth:example-auth".to_string())
    );
    assert_eq!(app.login_label.as_deref(), Some("Acme OAuth"));
}

/// `switch_account` is carried from the dispatcher that opened the picker, so
/// `/switch-account` still logs out first.
#[test]
fn switch_account_picker_choice_runs_the_switch_account_flow() {
    let mut app = test_app_with_agent();
    advertise_two_interactive_methods(&mut app);

    let effects = dispatch(Action::SwitchAccount, &mut app);
    assert!(effects.is_empty());
    assert!(
        open_picker(&app).switch_account,
        "the picker must remember it was opened to switch accounts"
    );

    let effects = dispatch(
        Action::ChooseAuthMethod {
            method_id: acp::AuthMethodId::new("plugin-oauth:example-auth"),
            switch_account: true,
        },
        &mut app,
    );

    assert!(matches!(
        effects.as_slice(),
        [Effect::SwitchAccount { method_id, .. }]
            if method_id.0.as_ref() == "plugin-oauth:example-auth"
    ));
}

/// Esc closes the picker *and* returns to the view it was opened from,
/// clearing `auth_return_view` (a stale value also disables foreign-session
/// polling).
#[test]
fn closing_the_picker_restores_the_originating_view() {
    let mut app = test_app_with_agent();
    advertise_two_interactive_methods(&mut app);
    dispatch(Action::Login, &mut app);
    assert_eq!(app.active_view, ActiveView::Welcome);

    dispatch(Action::CloseAuthMethodPicker, &mut app);

    assert!(app.auth_method_picker.is_none());
    assert_eq!(app.active_view, ActiveView::Agent(AgentId(0)));
    assert_eq!(app.auth_return_view, None);
}

/// The same Esc from the welcome screen closes the picker without moving the
/// user anywhere.
#[test]
fn closing_the_picker_from_welcome_stays_on_welcome() {
    let mut app = test_app();
    advertise_two_interactive_methods(&mut app);
    dispatch(Action::Login, &mut app);
    assert!(app.auth_method_picker.is_some());
    assert_eq!(app.auth_return_view, None);

    let effects = dispatch(Action::CloseAuthMethodPicker, &mut app);

    assert!(effects.is_empty());
    assert!(app.auth_method_picker.is_none());
    assert_eq!(app.active_view, ActiveView::Welcome);
}

/// A method that vanished between opening the picker and choosing must not
/// start an auth flow for an unadvertised id.
#[test]
fn choosing_an_unadvertised_method_fails_closed() {
    let mut app = test_app_with_agent();
    advertise_two_interactive_methods(&mut app);

    let effects = dispatch(
        Action::ChooseAuthMethod {
            method_id: acp::AuthMethodId::new("plugin-oauth:gone"),
            switch_account: false,
        },
        &mut app,
    );

    assert!(effects.is_empty());
    assert!(matches!(
        app.auth_state,
        AuthState::Pending { error: Some(_) }
    ));
}

// ── plugin-oauth: the plugin's panel is the whole sign-in UI ─────────────

/// `/login` with a `plugin-oauth:*` method must not enter the native
/// auth-URL wait. `x.ai/auth/get_url` is only ever populated by the shell's own
/// flows, so polling it strands the user on "Waiting for auth URL..." (and then
/// a token paste box that sends the code nowhere) while the real flow sits in
/// the plugin's panel.
#[test]
fn plugin_oauth_login_skips_the_native_auth_url_wait() {
    let mut app = test_app_with_agent();
    app.auth_methods = vec![picker_auth_method(
        "plugin-oauth:example-auth",
        "Acme OAuth",
    )];
    app.login_method_id = None;

    let effects = dispatch(Action::Login, &mut app);

    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::PollAuthUrl { .. })),
        "a plugin sign-in must not poll x.ai/auth/get_url: {effects:?}"
    );
    assert!(matches!(
        app.auth_state,
        AuthState::Authenticating {
            mode: AuthMode::Plugin,
            auth_url: None,
            ..
        }
    ));
}

/// The plugin publishes its panel into the session, so the pager must stay on
/// the session rather than detouring to the welcome screen's auth UI — and the
/// overlay is armed so the panel shows itself when it lands.
#[test]
fn plugin_oauth_login_stays_on_the_session_and_surfaces_the_panel() {
    let mut app = test_app_with_agent();
    app.auth_methods = vec![picker_auth_method(
        "plugin-oauth:example-auth",
        "Acme OAuth",
    )];
    app.login_method_id = None;
    let id = AgentId(0);
    assert_eq!(app.active_view, ActiveView::Agent(id));

    dispatch(Action::Login, &mut app);

    assert_eq!(
        app.active_view,
        ActiveView::Agent(id),
        "the panel renders in the session, so the user must stay there"
    );
    assert!(
        app.auth_return_view.is_none(),
        "nothing to return from: there was no detour"
    );

    // The panel arrives a moment later and opens itself.
    let vm = xai_grok_plugin_protocol::PanelViewModel {
        id: "signin".into(),
        title: "Acme sign-in".into(),
        blocks: vec![],
    };
    app.agents
        .get_mut(&id)
        .unwrap()
        .apply_plugin_panel("example-auth".into(), vm);
    assert!(
        app.agents[&id].plugin_panel_overlay_active(),
        "the sign-in panel must surface itself, not wait behind F6"
    );
}

/// A late `AuthUrlReady` from a superseded attempt must never downgrade a
/// plugin sign-in to Loopback — that would paint a paste box whose code goes
/// nowhere, since the plugin reads its code from the panel's own input.
#[test]
fn late_auth_url_ready_cannot_downgrade_a_plugin_login_to_loopback() {
    let mut app = test_app_with_agent();
    app.auth_methods = vec![picker_auth_method(
        "plugin-oauth:example-auth",
        "Acme OAuth",
    )];
    app.login_method_id = None;
    dispatch(Action::Login, &mut app);
    let AuthState::Authenticating { request_seq, .. } = app.auth_state else {
        panic!("expected Authenticating");
    };

    dispatch(
        Action::TaskComplete(TaskResult::AuthUrlReady {
            request_seq,
            auth_url: None,
            external: false,
            mode: None,
        }),
        &mut app,
    );

    assert!(matches!(
        app.auth_state,
        AuthState::Authenticating {
            mode: AuthMode::Plugin,
            ..
        }
    ));
}
