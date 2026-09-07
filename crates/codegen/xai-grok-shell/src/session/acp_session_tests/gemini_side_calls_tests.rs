//! Gemini is served, not refused, on the two auxiliary side calls.
//!
//! `handle_rewrite_memory_note` and `handle_ai_suggest` each dispatched backends themselves and
//! answered a Gemini one with a refusal. Both now hand the request to
//! `conversation_collect_with_idle_timeout`, which dispatches Gemini itself, so the refusals went
//! out with the dispatch that owned them.
//!
//! Neither call is visible in failure: a rewrite error is discarded by the client, which shows the
//! note back unchanged, and a suggest error is a `debug!` and no ghost text. A backend that
//! stopped being served here would look like the feature quietly doing nothing, on a provider
//! whose users would have no reason to suspect it. These tests hold both open against a Gemini
//! endpoint so it fails loudly instead.

use super::support::*;
use super::*;
use xai_grok_test_support::MockInferenceServer;

/// Serves the session's own model over `server` in Gemini's wire format.
async fn point_session_at_gemini(actor: &SessionActor, server: &MockInferenceServer) {
    let mut cfg = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("test actor has a sampling config");
    cfg.base_url = server.url();
    cfg.api_backend = xai_grok_sampling_types::ApiBackend::Gemini;
    actor.chat_state_handle.update_sampling_config(cfg);
}

/// Asserts the side call reached Gemini's endpoint with its prompt and sampling knobs intact.
///
/// The two callers build a system item, a user item, a temperature and a token cap and nothing
/// else, so every field they set has to appear: the translation drops what it cannot express
/// silently, and a prompt that arrived as an empty `contents` would still collect a plausible
/// answer out of the model.
fn assert_gemini_side_call(server: &MockInferenceServer, temperature: f32, max_output_tokens: u64) {
    let requests = server.requests();
    let logged = requests
        .iter()
        .rev()
        .find(|request| request.path.contains(":streamGenerateContent"))
        .expect("the side call must reach the Gemini endpoint, not be refused before it");
    let body = logged.body.as_ref().expect("Gemini request body is JSON");

    assert!(
        body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "the system prompt must survive translation: {body:#}"
    );
    assert_eq!(body["contents"][0]["role"], "user", "{body:#}");
    assert!(
        body["contents"][0]["parts"][0]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "the user prompt must survive translation: {body:#}"
    );
    let sent_temperature = body["generationConfig"]["temperature"]
        .as_f64()
        .expect("temperature reaches generationConfig") as f32;
    assert!(
        (sent_temperature - temperature).abs() < f32::EPSILON,
        "temperature: sent {sent_temperature}, expected {temperature}: {body:#}"
    );
    assert_eq!(
        body["generationConfig"]["maxOutputTokens"].as_u64(),
        Some(max_output_tokens),
        "{body:#}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn memory_note_rewrite_round_trips_on_a_gemini_provider() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("## Retry budget\n\n- capped at three attempts");
            point_session_at_gemini(&actor, &server).await;

            let rewritten = actor
                .handle_rewrite_memory_note("retries are capped at three", "worked on the sampler")
                .await
                .expect("a Gemini provider must serve the rewrite, not refuse it");

            assert_eq!(rewritten, "## Retry budget\n\n- capped at three attempts");
            assert_gemini_side_call(&server, 0.3, 1024);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ai_suggest_round_trips_on_a_gemini_provider() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("cargo clippy --all-targets");
            point_session_at_gemini(&actor, &server).await;

            let suggestion = actor
                .handle_ai_suggest("cargo cl", "/home/u/repo", None)
                .await
                .expect("a Gemini provider must serve the suggestion, not refuse it");

            assert_eq!(suggestion, "cargo clippy --all-targets");
            assert_gemini_side_call(&server, 0.1, 50);
        })
        .await;
}
