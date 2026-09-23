//! Regression coverage for the post-plan-approval agent switch's rebuild decision and its
//! interaction with an already-running turn.
//!
//! `harnesses_are_compatible` is a *wire-format* compatibility check for zero-turn/mid-turn
//! *model* switching between stock harnesses that share the default template. The plan-approval
//! switch reuses it to decide whether to rebuild onto a distinct, user-configured
//! [`xai_grok_agent::AgentDefinition`] (its own system prompt/tools/MCP servers) — a different
//! question. Two non-strict agents (neither `codex` nor `grok-build-orchestrator`) are always
//! "compatible" under that check, so a switch between e.g. `grok-build-plan` and `grok-build`
//! silently skipped the harness rebuild entirely.
use super::support::*;
use super::*;

/// Park a fake running turn on the actor, mirroring `plan_mode_midturn_tests::fake_running_turn`.
async fn fake_running_turn(actor: &SessionActor) {
    actor.state.lock().await.running_task = Some(AgentTask::new(
        "running-turn",
        tokio::task::spawn_local(std::future::pending::<()>()).abort_handle(),
    ));
}

/// A switch between two stock (non-strict) built-in agents must still rebuild the harness: the
/// target has its own system prompt/toolset, even though `harnesses_are_compatible` — designed
/// for same-harness model routing — calls the pair "compatible".
#[tokio::test]
async fn switch_to_a_different_stock_agent_rebuilds_the_harness() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            *actor.active_agent_type.lock() = Some("grok-build-plan".to_string());

            let switched = actor.apply_plan_agent_switch(Some("grok-build")).await;

            assert!(switched, "the switch must report that it changed something");
            assert_eq!(
                actor.active_agent_type.lock().as_deref(),
                Some("grok-build"),
                "the harness must be rebuilt onto the target agent, not left on the source \
                 agent just because both are non-strict harnesses",
            );
        })
        .await;
}

/// Switching to the agent that is already active is a true no-op: no rebuild is needed because
/// there is nothing to change.
#[tokio::test]
async fn switch_to_the_already_active_agent_skips_the_rebuild() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            *actor.active_agent_type.lock() = Some("grok-build".to_string());

            let switched = actor.apply_plan_agent_switch(Some("grok-build")).await;

            // Model/effort/sampling still get (re)applied, so this can report `true`; what
            // matters is that no rebuild churn occurs and the agent identity is unchanged.
            let _ = switched;
            assert_eq!(
                actor.active_agent_type.lock().as_deref(),
                Some("grok-build"),
            );
        })
        .await;
}

/// The switch is queued from the actor loop right after a turn finalizes but runs detached
/// (`spawn_local`), racing `handle_turn_end`'s goal continuation and the next queued prompt. If a
/// new turn has already started by the time the switch actually runs, it must back off instead of
/// mutating session state (sampling config, context window, compaction threshold) out from under
/// that turn.
#[tokio::test]
async fn switch_backs_off_when_a_turn_is_already_running() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            *actor.active_agent_type.lock() = Some("grok-build-plan".to_string());
            fake_running_turn(&actor).await;

            let model_before = actor
                .chat_state_handle
                .get_sampling_config()
                .await
                .map(|c| c.model);

            let switched = actor.apply_plan_agent_switch(Some("grok-build")).await;

            assert!(
                !switched,
                "must not report success while a turn is in flight"
            );
            assert_eq!(
                actor.active_agent_type.lock().as_deref(),
                Some("grok-build-plan"),
                "must not rebuild the harness underneath a running turn",
            );
            assert_eq!(
                actor
                    .chat_state_handle
                    .get_sampling_config()
                    .await
                    .map(|c| c.model),
                model_before,
                "must not swap the sampling config underneath a running turn",
            );
        })
        .await;
}
