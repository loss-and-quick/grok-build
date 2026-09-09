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
