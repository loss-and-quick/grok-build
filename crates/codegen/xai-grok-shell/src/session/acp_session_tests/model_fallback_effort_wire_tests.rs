//! Wire-level regression coverage for the main-turn provider/model failover
//! hop in `run_turn_via_sampler`.
//!
//! `[[model_fallbacks]]` reissues a failed turn against a different
//! `[[provider]]` entry, which may declare a narrower `reasoning_efforts`
//! menu than the entry the request was built for, or no effort dial at all.
//! `provider_control::effort_for_hop_target` already proves the derivation
//! rule in isolation; these prove the hop call site actually applies it, by
//! inspecting the request that reaches the target over the wire rather than
//! any internal bookkeeping.

use super::support::*;
use super::*;
use xai_grok_config_types::{ProviderConfig, ProviderFormat};
use xai_grok_sampling_types::conversation::{ConversationItem, ConversationRequest};
use xai_grok_sampling_types::{ReasoningEffort, ReasoningEffortOption};
use xai_grok_test_support::MockInferenceServer;

use crate::agent::config::{Config, ModelEntry, ModelFallback, resolve_model_list};

fn opt(value: ReasoningEffort, default: bool) -> ReasoningEffortOption {
    ReasoningEffortOption {
        id: value.as_str().to_owned(),
        value,
        label: value.as_str().to_owned(),
        description: None,
        default,
    }
}

/// A base URL nothing is listening on: the origin's request fails with a
/// `network` error class, which every chain built here triggers on.
fn dead_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}/v1")
}

fn provider(
    id: &str,
    model: &str,
    base_url: &str,
    reasoning_efforts: Vec<ReasoningEffortOption>,
) -> ProviderConfig {
    ProviderConfig {
        id: id.to_owned(),
        format: ProviderFormat::ChatCompletions,
        base_url: base_url.to_owned(),
        api_key: Some(format!("{id}-key")),
        auth_scheme: None,
        headers: indexmap::IndexMap::new(),
        proxy: None,
        models: vec![model.to_owned()],
        context_window: None,
        max_completion_tokens: Some(4_096),
        auth_account: None,
        reasoning_efforts,
        reasoning_effort: None,
        supports_reasoning_effort: false,
        thinking: None,
        max_concurrent: None,
    }
}

fn chain(from: &str, to: &[&str]) -> ModelFallback {
    ModelFallback {
        from: from.to_owned(),
        to: to.iter().map(|s| (*s).to_owned()).collect(),
        cooldown_seconds: 0,
        // `run_turn_via_sampler` submits through the sampler actor's
        // streaming path, which wraps a refused connection as
        // `SamplingError::EventStreamError` (`classify_error_class` ->
        // `"stream"`), not the plain `Http` variant the direct
        // `conversation_collect` path used elsewhere classifies as
        // `"network"`. Both are listed so this fixture does not depend on
        // which submission path happens to run underneath it.
        on_errors: vec![
            crate::agent::config::FallbackErrorClass::Network,
            crate::agent::config::FallbackErrorClass::Stream,
        ],
    }
}

/// A test actor whose catalog is `providers`, whose chains are `chains`, and
/// whose sampler is a real actor (not the no-op test stub) so a hop actually
/// re-issues a second HTTP request instead of only updating in-memory state.
async fn actor_with(
    providers: Vec<ProviderConfig>,
    chains: Vec<ModelFallback>,
) -> Arc<SessionActor> {
    let cfg = Config {
        providers,
        model_fallbacks: chains,
        ..Config::default()
    };
    let catalog: indexmap::IndexMap<String, ModelEntry> = resolve_model_list(&cfg, None);
    let tmp = std::env::temp_dir().join("grok-test-model-fallback-effort-wire");
    let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new(
        &tmp,
        crate::auth::GrokComConfig::default(),
    ));
    let models_manager = crate::agent::models::ModelsManager::new(
        None,
        catalog,
        agent_client_protocol::ModelId::new("origin/origin-model"),
        auth_manager,
        cfg,
    );
    let (gateway_tx, _g) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _p) = tokio::sync::mpsc::unbounded_channel();
    let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    actor.models_manager = models_manager;
    // Fail fast on the dead origin instead of spending the sampler's own
    // transport retry budget before the model-level chain ever gets a turn.
    actor.max_retries = 0;
    let (sampler_event_tx, _sampler_event_rx) =
        tokio::sync::mpsc::unbounded_channel::<xai_grok_sampler::SamplingEvent>();
    actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        xai_grok_sampler::SamplerConfig::default(),
        xai_grok_sampler::RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        sampler_event_tx,
    );
    Arc::new(actor)
}

/// Put the live session on `<model>` at `base_url`, so `run_turn_via_sampler`
/// reconstructs a config that names the origin this test's chain is written
/// against.
fn point_session_at(actor: &SessionActor, model: &str, base_url: &str) {
    actor
        .chat_state_handle
        .update_sampling_config(xai_grok_sampling_types::SamplingConfig {
            base_url: base_url.to_owned(),
            model: model.to_owned(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: Default::default(),
            extra_headers: Default::default(),
            query_params: Default::default(),
            env_http_headers: Default::default(),
            context_window: std::num::NonZeroU64::new(256_000).unwrap(),
            reasoning_effort: None,
            stream_tool_calls: None,
        });
}

fn probe_request(effort: Option<ReasoningEffort>) -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::user("hi".to_owned())],
        reasoning_effort: effort,
        ..Default::default()
    }
}

/// The origin declares `high`; the chain target's menu only lists `low`.
/// Replaying `high` unchanged would 400 the hop target by name — the target's
/// own default must go on the wire instead.
#[tokio::test(flavor = "current_thread")]
async fn hop_to_a_narrower_menu_sends_the_targets_default_not_the_origins_level() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let backup = MockInferenceServer::start().await.unwrap();
            backup.set_response("answered by the backup");
            let actor = actor_with(
                vec![
                    provider(
                        "origin",
                        "origin-model",
                        &dead_url(),
                        vec![opt(ReasoningEffort::High, true)],
                    ),
                    provider(
                        "backup",
                        "backup-model",
                        &backup.url(),
                        vec![opt(ReasoningEffort::Low, true)],
                    ),
                ],
                vec![chain("origin-model", &["backup-model"])],
            )
            .await;
            point_session_at(&actor, "origin-model", &dead_url());

            let outcome = actor
                .run_turn_via_sampler(probe_request(Some(ReasoningEffort::High)))
                .await
                .expect("the chain target answers after the origin's dead endpoint fails");
            assert!(matches!(outcome, SamplerTurnOutcome::Response(..)));

            let bodies = backup.request_bodies();
            assert_eq!(bodies.len(), 1, "exactly one re-issue, on the hop target");
            assert_eq!(
                bodies[0].get("model").and_then(|m| m.as_str()),
                Some("backup-model")
            );
            assert_eq!(
                bodies[0].get("reasoning_effort").and_then(|e| e.as_str()),
                Some("low"),
                "high is off the target's menu; its own default must reach the wire, \
                 not the origin's level and not no reasoning at all"
            );
        })
        .await;
}

/// The chain target declares no effort dial at all. Replaying the origin's
/// level unchanged would 400 an endpoint that rejects the parameter's mere
/// presence — the field must be absent from the wire body entirely.
#[tokio::test(flavor = "current_thread")]
async fn hop_to_a_target_with_no_effort_dial_omits_the_parameter() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let backup = MockInferenceServer::start().await.unwrap();
            backup.set_response("answered by the backup");
            let actor = actor_with(
                vec![
                    provider(
                        "origin",
                        "origin-model",
                        &dead_url(),
                        vec![opt(ReasoningEffort::High, true)],
                    ),
                    // No `reasoning_efforts` menu and `supports_reasoning_effort`
                    // left at its default `false`: a target with no dial.
                    provider("backup", "backup-model", &backup.url(), vec![]),
                ],
                vec![chain("origin-model", &["backup-model"])],
            )
            .await;
            point_session_at(&actor, "origin-model", &dead_url());

            let outcome = actor
                .run_turn_via_sampler(probe_request(Some(ReasoningEffort::High)))
                .await
                .expect("the chain target answers after the origin's dead endpoint fails");
            assert!(matches!(outcome, SamplerTurnOutcome::Response(..)));

            let bodies = backup.request_bodies();
            assert_eq!(bodies.len(), 1);
            assert_eq!(
                bodies[0].get("model").and_then(|m| m.as_str()),
                Some("backup-model")
            );
            assert!(
                bodies[0].get("reasoning_effort").is_none(),
                "the target takes no effort dial, so the parameter must not \
                 reach the wire at all: got {:?}",
                bodies[0].get("reasoning_effort")
            );
        })
        .await;
}
