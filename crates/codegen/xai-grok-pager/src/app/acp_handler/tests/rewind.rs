#![cfg_attr(rustfmt, rustfmt::skip)]
    //! What this pane does when somebody else rewinds the session it is watching.
    use super::*;
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::blocks::UserPromptBlock;
    use crate::views::rewind::{RewindPhase, RewindState};

    fn user_block(text: &str, prompt_index: usize) -> RenderBlock {
        let mut b = UserPromptBlock::new(text);
        b.prompt_index = Some(prompt_index);
        RenderBlock::UserPrompt(b)
    }

    fn rewind_marker_ext(session_id: &str, target_prompt_index: usize) -> acp::ExtNotification {
        let notif = SessionNotification {
            session_id: acp::SessionId::new(session_id),
            update: XaiSessionUpdate::RewindMarker {
                target_prompt_index,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            meta: None,
        };
        let raw = serde_json::value::to_raw_value(&notif).unwrap();
        acp::ExtNotification::new("x.ai/session_notification", std::sync::Arc::from(raw))
    }

    /// Two turns on screen, a rewind to prompt 1 elsewhere: the second turn goes, and the pane says why.
    /// Before the marker reached any client but the one that asked, this pane kept rendering a turn the session no
    /// longer had, and only corrected on the next reload.
    #[test]
    fn a_peers_rewind_drops_the_turns_it_discarded() {
        let mut app = make_app_with_agent("sess-rw");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.scrollback.push_block(user_block("first", 0));
            agent.scrollback.push_block(RenderBlock::agent_message("r0"));
            agent.scrollback.push_block(user_block("second", 1));
            agent.scrollback.push_block(RenderBlock::agent_message("r1"));
            agent.set_last_turn_summary(Some("described the discarded turn".into()));
        }

        let changed = handle_session_notification(&rewind_marker_ext("sess-rw", 1), &mut app);
        assert!(changed, "turns leaving the screen must redraw it");

        let agent = &app.agents[&AgentId(0)];
        let texts: Vec<String> = (0..agent.scrollback.len())
            .filter_map(|i| match &agent.scrollback.get(i).unwrap().block {
                RenderBlock::UserPrompt(b) => Some(b.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["first".to_string()],
            "the prompt the rewind cut to and everything after it must go"
        );
        assert!(
            agent.last_turn_summary.is_none(),
            "the summary described turns that are gone"
        );
        assert!(agent.rewind_points.is_none(), "the cached points are stale");
        assert!(
            matches!(
                &agent.scrollback.get(agent.scrollback.len() - 1).unwrap().block,
                RenderBlock::System(s) if s.text.contains("Another client")
            ),
            "the pane must say the transcript was cut by someone else"
        );
    }

    /// The marker goes out before the `x.ai/rewind/execute` response, so the client that asked for the rewind sees its
    /// own. Acting on it here would truncate ahead of the response that still has to restore the draft and confirm,
    /// and would tell the user a peer did what they just did themselves.
    #[test]
    fn this_clients_own_rewind_is_left_to_its_own_response() {
        let mut app = make_app_with_agent("sess-rw");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.scrollback.push_block(user_block("first", 0));
            agent.scrollback.push_block(user_block("second", 1));
            agent.rewind_state = Some(RewindState {
                phase: RewindPhase::Executing {
                    target_prompt_index: 1,
                },
                anchor_entry_idx: 1,
                stashed_draft: None,
                selected_prompt_index: None,
            });
        }

        let changed = handle_session_notification(&rewind_marker_ext("sess-rw", 1), &mut app);
        assert!(!changed, "the in-flight rewind's own response owns this");
        let agent = &app.agents[&AgentId(0)];
        assert_eq!(agent.scrollback.len(), 2, "nothing may be truncated twice");
        assert!(
            agent.rewind_state.is_some(),
            "the executing state belongs to the response still to come"
        );
    }

    /// An open picker lists prompts by index; after a peer's cut those indices name turns the session no longer has.
    /// It closes the way every other exit from that flow does, with the stashed draft going back to the composer.
    #[test]
    fn a_peers_rewind_closes_an_open_picker_and_returns_the_draft() {
        let mut app = make_app_with_agent("sess-rw");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.scrollback.push_block(user_block("first", 0));
            agent.scrollback.push_block(user_block("second", 1));
            agent.prompt.set_text("half-typed");
            let stashed = agent.prompt.stash();
            agent.rewind_state = Some(RewindState {
                phase: RewindPhase::Picker {
                    points: Vec::new(),
                    selected: 0,
                },
                anchor_entry_idx: 0,
                stashed_draft: Some(stashed),
                selected_prompt_index: None,
            });
        }

        assert!(handle_session_notification(&rewind_marker_ext("sess-rw", 1), &mut app));
        let agent = &app.agents[&AgentId(0)];
        assert!(agent.rewind_state.is_none(), "the picker is listing a timeline that is gone");
        assert_eq!(agent.prompt.text(), "half-typed", "the stashed draft comes back");
    }
