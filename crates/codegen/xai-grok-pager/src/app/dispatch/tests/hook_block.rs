//! Tests for the blocked-prompt card's three answers (`Action::PromptBlockAnswered`).
//!
//! A `UserPromptSubmit` hook denial requeues the blocked prompt at the LOCAL queue front
//! (`turn_completion::note_hook_blocked_turn`), because a fixed resubmission must run ahead of
//! the followers it was holding up. Where those followers live decides who can start the next
//! turn: the local drip-feed drain, or the shell.

use super::*;
use crate::app::actions::PromptBlockChoice;
use crate::app::agent::InFlightPrompt;
use crate::app::turn_completion::{HOOK_DENIED_CATEGORY, note_hook_blocked_turn};

const BLOCKED_TEXT: &str = "deploy hihi to prod";

/// Run the hook-denied finalize rail over a self-originated running turn, then land in the
/// post-turn Idle window the card is answered from.
fn deny_running_prompt(app: &mut AppView, id: AgentId) -> u64 {
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.session.current_prompt_id = Some("p1".into());
    agent.note_self_originated_prompt("p1");
    let entry = agent
        .scrollback
        .push_block(RenderBlock::user_prompt(BLOCKED_TEXT));
    agent.session.in_flight_prompt = Some(InFlightPrompt {
        text: BLOCKED_TEXT.into(),
        images: vec![],
        scrollback_entry: entry,
        combined_scrollback_entries: vec![],
        chip_elements: vec![],
    });

    note_hook_blocked_turn(agent, Some("p1"), Some(HOOK_DENIED_CATEGORY), None);

    // The blocked turn is over: the pane is Idle again with nothing running.
    agent.session.state = AgentState::Idle;
    agent.session.current_prompt_id = None;
    agent.session.pending_prompts.front().unwrap().id
}

/// Default single-client shape: the blocked prompt and its followers are all local rows.
fn blocked_with_local_followers() -> (AppView, u64) {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    enqueue_local(&mut app, id, "follower");
    let row_id = deny_running_prompt(&mut app, id);
    (app, row_id)
}

/// Leader mode: the followers were queued server-side while the blocked prompt was the running
/// turn, so they are in `shared_queue` and only the shell can promote them.
fn blocked_with_server_followers() -> (AppView, u64) {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.leader_mode = true;
    app.push_optimistic_prompt_echo("test-session", "q-follower", "follower", "prompt");
    let snapshot = app.shared_prompt_queue("test-session").cloned().unwrap();
    app.agents.get_mut(&id).unwrap().shared_queue = snapshot;
    let row_id = deny_running_prompt(&mut app, id);
    (app, row_id)
}

fn local_texts(app: &AppView) -> Vec<&str> {
    app.agents[&AgentId(0)]
        .session
        .pending_prompts
        .iter()
        .map(|p| p.text.as_str())
        .collect()
}

fn sent_prompt_texts(effects: &[Effect]) -> Vec<&str> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::SendPrompt { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The default path: with every follower local, Resend drops the pager's hold and the local
/// drip-feed drain sends the blocked prompt itself.
#[test]
fn resend_drains_locally_when_the_followers_are_local_rows() {
    let (mut app, row_id) = blocked_with_local_followers();

    let effects = dispatch(
        Action::PromptBlockAnswered {
            row_id,
            choice: PromptBlockChoice::Resend,
        },
        &mut app,
    );

    assert_eq!(
        sent_prompt_texts(&effects),
        vec![BLOCKED_TEXT],
        "the blocked prompt goes out on the local path, got {effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::SendPromptNow { .. })),
        "a purely local queue must not be rerouted through the server"
    );
    assert_eq!(local_texts(&app), vec!["follower"], "the follower waits");
    let agent = &app.agents[&AgentId(0)];
    assert!(!agent.session.hook_block_hold, "the hold is released");
    assert!(agent.session.blocked_prompt.is_none(), "the card is gone");
    assert!(agent.session.state.is_turn_running());
}

/// Discard drops the row and starts nothing; the follower is left for the ordinary drain.
#[test]
fn discard_removes_the_blocked_row_and_leaves_the_followers_queued() {
    let (mut app, row_id) = blocked_with_local_followers();

    let effects = dispatch(
        Action::PromptBlockAnswered {
            row_id,
            choice: PromptBlockChoice::Discard,
        },
        &mut app,
    );

    assert_eq!(
        sent_prompt_texts(&effects),
        vec!["follower"],
        "discarding unparks the queue and the follower takes the turn, got {effects:?}"
    );
    assert!(
        local_texts(&app).is_empty(),
        "the blocked row is gone, got {:?}",
        local_texts(&app)
    );
}

/// Edit only opens the row; nothing is sent until the user saves.
#[test]
fn edit_opens_the_blocked_row_and_sends_nothing() {
    let (mut app, row_id) = blocked_with_local_followers();

    let effects = dispatch(
        Action::PromptBlockAnswered {
            row_id,
            choice: PromptBlockChoice::Edit,
        },
        &mut app,
    );

    assert!(effects.is_empty(), "Edit sends nothing, got {effects:?}");
    let agent = &app.agents[&AgentId(0)];
    assert!(
        matches!(agent.prompt_mode, PromptMode::EditingQueued { id, .. } if id == row_id),
        "the blocked row is open for editing, got {:?}",
        agent.prompt_mode
    );
    assert_eq!(local_texts(&app), vec![BLOCKED_TEXT, "follower"]);
}

/// The drain barrier itself: an ordinary local row must keep waiting while the shell owns the
/// next turn, or it would optimistically promote a turn the shell is only going to queue
/// (`maybe_drain_queue`'s `server_queue_owns_next_turn` arm).
#[test]
fn an_ordinary_local_row_still_waits_while_the_server_owns_the_next_turn() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.leader_mode = true;
    app.push_optimistic_prompt_echo("test-session", "q1", "server row", "prompt");
    let snapshot = app.shared_prompt_queue("test-session").cloned().unwrap();
    app.agents.get_mut(&id).unwrap().shared_queue = snapshot;
    enqueue_local(&mut app, id, "typed while starting");

    let effects = dispatch(Action::DrainQueue, &mut app);

    assert!(
        effects.is_empty(),
        "the local row must not overtake the server queue, got {effects:?}"
    );
    assert_eq!(local_texts(&app), vec!["typed while starting"]);
}

/// The wedge: with the followers on the server queue, both sides park and neither can move.
///
/// The blocked prompt sits at the local front, so the local drain is barred by
/// `server_queue_owns_next_turn`; the shell's own hook-block hold
/// (`acp_session_impl/notification_drain.rs:178-194`) bars its promote, and only a prompt or a
/// queue mutation lifts it. Resend released the pager's hold and asked the barred local queue to
/// drain, which sends nothing — so nothing ever lifts the shell's hold either.
#[test]
fn resend_reaches_the_shell_when_the_followers_are_server_rows() {
    let (mut app, row_id) = blocked_with_server_followers();

    let effects = dispatch(
        Action::PromptBlockAnswered {
            row_id,
            choice: PromptBlockChoice::Resend,
        },
        &mut app,
    );

    let sent = effects
        .iter()
        .find_map(|e| match e {
            Effect::SendPromptNow { blocks, .. } => Some(blocks.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("Resend must reach the shell, got {effects:?}"));
    assert!(
        sent.iter().any(|b| matches!(
            b,
            agent_client_protocol::ContentBlock::Text(t) if t.text == BLOCKED_TEXT
        )),
        "the send carries the blocked prompt, got {sent:?}"
    );
    assert!(
        local_texts(&app).is_empty(),
        "the row leaves the local queue with the send, got {:?}",
        local_texts(&app)
    );
    assert!(
        !app.agents[&AgentId(0)].session.state.is_turn_running(),
        "the shell owns the turn start; this client must not promote it locally"
    );
}

/// Saving the card's Edit lands in the same place as Resend and must reach the shell too.
#[test]
fn saving_the_edited_blocked_row_reaches_the_shell_when_the_followers_are_server_rows() {
    let (mut app, row_id) = blocked_with_server_followers();
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.enter_queue_edit(row_id, false, None);
        agent.prompt.set_text("deploy nice to prod");
        assert!(matches!(
            agent.save_edited_queued_row(row_id, None, true),
            crate::app::app_view::InputOutcome::Action(Action::DrainQueue)
        ));
    }

    let effects = dispatch(Action::DrainQueue, &mut app);

    let sent = effects
        .iter()
        .find_map(|e| match e {
            Effect::SendPromptNow { blocks, .. } => Some(blocks.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the saved edit must reach the shell, got {effects:?}"));
    assert!(
        sent.iter().any(|b| matches!(
            b,
            agent_client_protocol::ContentBlock::Text(t) if t.text == "deploy nice to prod"
        )),
        "the send carries the edited text, got {sent:?}"
    );
    assert!(local_texts(&app).is_empty());
}

/// The reroute is the card's resubmission, not a way around it: while the card is still open the
/// hold is armed, and an unrelated drain must not put the blocked prompt on the wire behind the
/// user's back.
#[test]
fn an_unanswered_card_keeps_the_blocked_prompt_off_the_wire() {
    let (mut app, _row_id) = blocked_with_server_followers();
    assert!(app.agents[&AgentId(0)].session.hook_block_hold);

    let effects = dispatch(Action::DrainQueue, &mut app);

    assert!(
        effects.is_empty(),
        "the card is unanswered; nothing may send, got {effects:?}"
    );
    assert_eq!(local_texts(&app), vec![BLOCKED_TEXT], "the row stays put");
    assert!(
        app.agents[&AgentId(0)].session.hook_block_hold,
        "the hold survives an unrelated drain"
    );
}

/// The reroute must not cost the prompt what the composer put on it.
///
/// The send-now that unwedges the queue is a wire send, and a queue row on the wire is text: the
/// row lists the images the session holds but never their bytes, and a collapsed paste has no
/// wire form at all. So the turn-start shim used to rebuild the rewind stash out of the adopted
/// text alone, and Ctrl+C right after Resend handed back a prompt stripped of its picture with
/// its paste blown open. The sender keeps its own copy instead
/// (`AgentView::sent_prompt_attachments`) and the shim claims it by the id it minted.
#[test]
fn a_rerouted_resend_still_rewinds_with_its_images_and_chips() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.leader_mode = true;
    app.push_optimistic_prompt_echo("test-session", "q-follower", "follower", "prompt");
    let snapshot = app.shared_prompt_queue("test-session").cloned().unwrap();
    let row_id = {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.shared_queue = snapshot;
        agent.session.state = AgentState::TurnRunning;
        agent.session.current_prompt_id = Some("p1".into());
        agent.note_self_originated_prompt("p1");
        let entry = agent
            .scrollback
            .push_block(RenderBlock::user_prompt(BLOCKED_TEXT));
        agent.session.in_flight_prompt = Some(InFlightPrompt {
            text: BLOCKED_TEXT.into(),
            images: vec![crate::app::agent_view::test_fixtures::test_pasted_image()],
            scrollback_entry: entry,
            combined_scrollback_entries: vec![],
            chip_elements: vec![crate::app::agent::ChipElement {
                range: 0..6,
                kind: crate::views::prompt_widget::KIND_PASTE,
                display: None,
            }],
        });
        note_hook_blocked_turn(agent, Some("p1"), Some(HOOK_DENIED_CATEGORY), None);
        agent.session.state = AgentState::Idle;
        agent.session.current_prompt_id = None;
        agent.session.pending_prompts.front().unwrap().id
    };

    let effects = dispatch(
        Action::PromptBlockAnswered {
            row_id,
            choice: PromptBlockChoice::Resend,
        },
        &mut app,
    );
    let prompt_id = effects
        .iter()
        .find_map(|e| match e {
            Effect::SendPromptNow { prompt_id, .. } => Some(prompt_id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("Resend must reach the shell, got {effects:?}"));

    // The shell promotes it and broadcasts; this is the adoption that follows.
    let agent = app.agents.get_mut(&id).unwrap();
    crate::app::dispatch::queue::apply_turn_start_shim(
        agent,
        prompt_id,
        Some(BLOCKED_TEXT.to_string()),
        "prompt",
        None,
    );

    let stashed = agent
        .session
        .in_flight_prompt
        .as_ref()
        .expect("the adopted turn is rewindable");
    assert_eq!(stashed.text, BLOCKED_TEXT);
    assert_eq!(
        stashed.images.len(),
        1,
        "the image the composer attached comes back"
    );
    assert_eq!(
        stashed.chip_elements.len(),
        1,
        "the collapsed paste comes back collapsed"
    );
}
