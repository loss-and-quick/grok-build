//! Bounded model failover for auxiliary one-shot inference.
//!
//! `[[model_fallbacks]]` used to protect only the main turn: the retry loop
//! lives in `run_turn_via_sampler`, and every auxiliary caller — the Auto-mode
//! permission classifier, prompt suggestion, `web_fetch` distillation, image
//! description — built its own `ConversationRequest` and issued it straight at a
//! client. A single 429, 5xx or dropped connection therefore took the whole
//! feature out with nothing to point at. This module gives those callers the
//! same chains under a much smaller budget.
//!
//! Session titling was worse off still: it runs on the persistence actor's own
//! task, which cannot reach a `SessionActor` at all, so it held a client built
//! at session open and had nothing to ask for a second one. It arrives here
//! over [`AuxInferenceBridge`], which is the `Send + Sync` seam onto this loop.
//!
//! [`AuxInferenceBridge`]: super::AuxInferenceBridge
//!
//! # Why not just call the turn's failover
//!
//! [`SessionActor::try_provider_failover`] is deliberately not reused:
//!
//! * it consults the `provider_error` plugin hook, whose payload has no way to
//!   say "this was a background one-shot", so a plugin `Retry { model }`
//!   directive written for the turn would silently redirect a classifier or a
//!   distillation to a model chosen for something else;
//! * it emits a user-facing `RetryState::Retrying` notice, which is wrong for
//!   work the user never asked for and is not waiting on;
//! * it installs the substituted config on the session's sampler actor, which
//!   an aux call must never do — the next turn would inherit it.
//!
//! What is shared is the part that carries the operator's intent: the built-in
//! chain lookup and its cooldown ([`SessionActor::builtin_fallback_target`]).
//!
//! # A catalog miss is not a failure
//!
//! Every caller here resolves its model before it gets this far, and a model the
//! catalog cannot place is a *skip* — availability, not a runtime error (see
//! `effective_suggest_model`). Nothing in this module can turn that into a hop:
//! it is only ever entered with a client that already resolved, and a chain
//! target that does not resolve stops the hop rather than degrading to some
//! other model. What a miss costs differs by caller — distillation skips, a
//! session title falls to the session's own route — but in neither case does it
//! reach the missing model's chain.
//!
//! # Budgets
//!
//! Per caller, never one number everywhere. An aux call must not inherit the
//! turn's budget: the classifier gates a permission decision on every tool call,
//! so a long chain there would be paid on the critical path of the whole
//! session. The constants below carry the reasoning for each.

use xai_grok_sampler::SamplingClient;
use xai_grok_sampling_types::{ConversationRequest, ConversationResponse, SamplingError};

use super::*;

/// Auto-mode permission classifier: one alternative.
///
/// It gates a permission verdict, so its latency is paid before every tool call
/// the session makes. One hop covers the case this exists for — a provider that
/// is down or throttling — while a longer chain would multiply the worst case of
/// the most frequently issued aux call in the process.
pub(super) const AUX_HOPS_PERMISSION_CLASSIFIER: u32 = 1;

/// Prompt suggestion: one alternative.
///
/// Fires once per completed turn and renders as ghost text next to an idle
/// prompt or not at all. Worth one hop because a whole provider being down
/// otherwise removes the feature silently for the rest of the session; not worth
/// more, because by the third attempt the user has usually started typing.
pub(super) const AUX_HOPS_PROMPT_SUGGEST: u32 = 1;

/// `web_fetch` distillation: one alternative.
///
/// The tool call is blocked on it and the tool applies its own
/// `distill_timeout` around the whole thing, so extra hops mostly buy expiry.
/// Failing distillation returns the raw page, which is degraded but correct.
pub(super) const AUX_HOPS_WEB_FETCH_DISTILL: u32 = 1;

/// Session title: two alternatives.
///
/// The only aux call with exactly one chance ever. It fires once, from the
/// persistence actor's own task, and the generator marks itself done before the
/// request goes out — a session that titles off truncated user text keeps that
/// title for the rest of its life, and a resumed session that already has one
/// never asks again. Nothing is waiting on it either: no tool call is blocked,
/// no ghost-text window is closing, no permission verdict is pending, and it
/// rides the background admission lane. So the bound worth respecting is total
/// work rather than latency, and that bound is small — three requests of at most
/// a hundred output tokens, once per session.
///
/// `pub(crate)`, unlike its neighbours, because the caller that spends it lives
/// outside this actor: the persistence actor's title task holds the budget and
/// hands it back over the bridge.
pub(crate) const AUX_HOPS_SESSION_TITLE: u32 = 2;

/// Image description: two alternatives.
///
/// The only aux call whose failure changes what the model can *see* — without a
/// description the turn proceeds on an image the model was never shown, and it
/// has no way to know that. It runs once per attached image during prompt build,
/// not per turn, so the extra hop is bounded work paid rarely.
pub(super) const AUX_HOPS_IMAGE_DESCRIBE: u32 = 2;

/// Why an auxiliary call stopped hopping and surfaced its error.
///
/// Recorded on the debug log at every stop so a feature that quietly went away
/// leaves something to point at. Previously a failed aux call logged its
/// transport error and nothing about *why* no fallback happened, which is the
/// half that tells an operator whether their chain is even wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuxHopStop {
    /// The caller's hop budget is spent.
    BudgetExhausted,
    /// No `[[model_fallbacks]]` chain matches this model and error class, or
    /// every target in the matching chain is still cooling down.
    NoChain,
    /// A chain named a target that has no aux route of its own. Treated exactly
    /// like the caller's own catalog miss: stop, never degrade onto some other
    /// endpoint's client.
    TargetUnroutable,
}

impl AuxHopStop {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::BudgetExhausted => "budget_exhausted",
            Self::NoChain => "no_chain",
            Self::TargetUnroutable => "target_unroutable",
        }
    }
}

/// The identities a chain's `from` may be spelled as for one resolved aux model.
///
/// A caller resolves a catalog *key* (`<provider>/<model>`) and the resolver
/// hands back that entry's own routing *slug*. An operator writing
/// `[[model_fallbacks]] from = ...` may reasonably have written either, so both
/// are offered to the lookup, key first — the key is the unambiguous one, and
/// preferring it means a chain written against a provider-qualified pin is not
/// shadowed by one written against the bare slug two providers share.
pub(super) fn chain_identities(catalog_key: &str, wire_model: &str) -> Vec<String> {
    let mut ids = vec![catalog_key.to_owned()];
    if wire_model != catalog_key {
        ids.push(wire_model.to_owned());
    }
    ids
}

/// One auxiliary call's route: the client that issues it and the identities it
/// answers to.
#[derive(Clone)]
pub(super) struct AuxRoute {
    /// Issues the request. Already carries the concurrency class the caller
    /// chose (see [`SamplingClient::as_background`]).
    pub(super) client: SamplingClient,
    /// The catalog key this route was resolved from.
    pub(super) catalog_key: String,
    /// The wire model the route's entry serves, i.e. what goes in
    /// `ConversationRequest::model`.
    pub(super) wire_model: String,
    /// Whether a hop's replacement client should also take the background lane.
    pub(super) background: bool,
}

impl SessionActor {
    /// Resolve one auxiliary route by catalog key, or `None` when the key has no
    /// route of its own (the caller then skips, exactly as it does for a catalog
    /// miss).
    pub(super) async fn resolve_aux_route(
        &self,
        catalog_key: &str,
        background: bool,
    ) -> Option<AuxRoute> {
        let (client, wire_model, _context_window) =
            self.resolve_aux_sampler_client(catalog_key).await?;
        Some(AuxRoute {
            client: if background {
                client.as_background()
            } else {
                client
            },
            catalog_key: catalog_key.to_owned(),
            wire_model,
            background,
        })
    }

    /// Describe the session's *own* client as an aux route.
    ///
    /// For an auxiliary call whose model has no route of its own and which
    /// therefore rides the session client (the Auto-mode classifier when no
    /// `classifier_model` is pinned). Naming the session model and its catalog
    /// key here is what keeps a `[[model_fallbacks]]` chain written against the
    /// session model applicable to those calls too, instead of the
    /// no-dedicated-model case quietly opting out of failover.
    pub(super) async fn session_client_aux_route(
        &self,
        background: bool,
    ) -> Result<AuxRoute, acp::Error> {
        let client = self.prepare_chat_completion(false).await?;
        let wire_model = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| c.model)
            .unwrap_or_default();
        Ok(AuxRoute {
            client: if background {
                client.as_background()
            } else {
                client
            },
            catalog_key: self
                .models_manager
                .catalog_key(&wire_model)
                .unwrap_or_else(|| wire_model.clone()),
            wire_model,
            background,
        })
    }

    /// Pick the next chain target for an aux model that just failed, trying each
    /// spelling the operator may have written the chain against.
    fn aux_fallback_target(&self, route: &AuxRoute, error_class: &str) -> Option<String> {
        for id in chain_identities(&route.catalog_key, &route.wire_model) {
            if let Some((target, cooldown)) = self.builtin_fallback_target(&id, error_class) {
                if cooldown > std::time::Duration::ZERO {
                    self.arm_fallback_cooldown(&id, &target);
                }
                return Some(target);
            }
        }
        None
    }

    /// Rebuild `request` for a hop onto `target`, or `None` when the target has
    /// no aux route.
    ///
    /// The wire `model` and the reasoning effort are both re-derived from the
    /// target's own catalog entry. Nothing about the origin entry's model
    /// configuration is carried across — that is the whole point of resolving
    /// rather than string-substituting a model name.
    async fn build_aux_hop(
        &self,
        route: &AuxRoute,
        target: &str,
        request: &ConversationRequest,
    ) -> Option<(AuxRoute, ConversationRequest)> {
        // A chain may name the target either way round, same as `from`; the
        // catalog key is what the resolver and the capability lookups both want.
        let target_key = self
            .models_manager
            .catalog_key(target)
            .unwrap_or_else(|| target.to_owned());
        let next = self
            .resolve_aux_route(&target_key, route.background)
            .await?;
        let mut next_request = request.clone();
        next_request.model = Some(next.wire_model.clone());
        next_request.reasoning_effort = effort_for_hop_target(
            request.reasoning_effort,
            self.models_manager
                .model_supports_reasoning_effort(&target_key),
            &self.models_manager.model_reasoning_efforts(&target_key),
            self.models_manager
                .model_default_reasoning_effort(&target_key),
        );
        Some((next, next_request))
    }

    /// Issue one auxiliary request, hopping onto `[[model_fallbacks]]` targets
    /// on a runtime failure until `max_hops` substitutions are spent.
    ///
    /// A saturation timeout from the provider admission gate (a 503 carrying
    /// `should_retry: false`) is deliberately allowed to trigger a hop. That
    /// flag means "do not re-enter this queue", and a hop is not that: admission
    /// is keyed by `(base_url, wire model)` and a chain target is a different
    /// pair, so it has its own gate and its own free slots. Refusing to hop
    /// would let one provider's declared parallelism cap take the feature out
    /// entirely. Nor can the hop hold two slots at once: the permit rides the
    /// response stream, and this awaits each attempt to completion before the
    /// next one is built, so the failed attempt's permit has already dropped.
    pub(super) async fn aux_collect_with_fallback(
        &self,
        caller: &'static str,
        route: AuxRoute,
        request: ConversationRequest,
        max_hops: u32,
    ) -> Result<ConversationResponse, SamplingError> {
        let mut route = route;
        let mut request = request;
        let mut hops: u32 = 0;
        loop {
            let error = match route.client.conversation_collect(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(error) => error,
            };
            let error_class = xai_grok_sampler::classify_error_class(&error);
            let stop = if hops >= max_hops {
                Some(AuxHopStop::BudgetExhausted)
            } else {
                match self.aux_fallback_target(&route, error_class) {
                    None => Some(AuxHopStop::NoChain),
                    Some(target) => {
                        match self.build_aux_hop(&route, &target, &request).await {
                            Some((next_route, next_request)) => {
                                hops += 1;
                                tracing::debug!(
                                    caller,
                                    from = %route.wire_model,
                                    to = %next_route.wire_model,
                                    error_class,
                                    hop = hops,
                                    max_hops,
                                    effort = ?next_request.reasoning_effort,
                                    %error,
                                    "aux fallback: model failed, re-issuing on a chain target"
                                );
                                route = next_route;
                                request = next_request;
                                None
                            }
                            // The chain named a target this shell cannot route.
                            // Availability, not a runtime error — stop here
                            // rather than degrade onto another endpoint.
                            None => Some(AuxHopStop::TargetUnroutable),
                        }
                    }
                }
            };
            if let Some(stop) = stop {
                // The one line that makes a silent degrade visible: which
                // feature stopped working, on which model, why the fallback did
                // not save it, and how much budget it had.
                tracing::debug!(
                    caller,
                    model = %route.wire_model,
                    catalog_key = %route.catalog_key,
                    error_class,
                    stop = stop.as_str(),
                    hops,
                    max_hops,
                    %error,
                    "aux fallback: giving up, the caller degrades"
                );
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── chain identity ────────────────────────────────────────────────────

    #[test]
    fn chain_is_matched_on_the_key_first_then_the_slug() {
        assert_eq!(
            chain_identities("acme/some-model", "some-model"),
            vec!["acme/some-model".to_owned(), "some-model".to_owned()]
        );
    }

    #[test]
    fn chain_identity_is_not_duplicated_when_key_and_slug_agree() {
        assert_eq!(
            chain_identities("some-model", "some-model"),
            vec!["some-model".to_owned()]
        );
    }

    #[test]
    fn hop_stop_reasons_are_named_on_the_log() {
        assert_eq!(AuxHopStop::BudgetExhausted.as_str(), "budget_exhausted");
        assert_eq!(AuxHopStop::NoChain.as_str(), "no_chain");
        assert_eq!(AuxHopStop::TargetUnroutable.as_str(), "target_unroutable");
    }

    /// A saturation timeout from the provider admission gate is a *deliberate*
    /// non-retryable — but it still reaches a chain, because it lands in the
    /// `5xx` bucket every `on_errors` list already names. That is the intended
    /// policy and not an accident of the status code: `should_retry: false`
    /// forbids re-entering the *same* queue, and a chain target is a different
    /// `(endpoint, wire model)` pair with its own gate and its own free slots.
    /// Suppressing the hop here would let one provider's declared parallelism
    /// cap take a feature out entirely.
    #[test]
    fn a_saturation_timeout_still_reaches_the_chain() {
        let saturated = SamplingError::Api {
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            message: "no concurrency slot for `endpoint` after 600s".to_owned(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: Some(false),
            error_code: None,
        };
        assert_eq!(xai_grok_sampler::classify_error_class(&saturated), "5xx");
    }

    // ── Hop mechanics against live endpoints. ──────────────────────────────

    use crate::agent::config::{
        Config, FallbackErrorClass, ModelEntry, ModelFallback, resolve_model_list,
    };
    use indexmap::IndexMap;
    use xai_grok_config_types::{ProviderConfig, ProviderFormat};
    use xai_grok_test_support::MockInferenceServer;

    /// A base URL nothing is listening on: bind an ephemeral port, learn it,
    /// release it. Connecting there is refused, which is the `network` class.
    fn dead_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        format!("http://127.0.0.1:{port}/v1")
    }

    fn provider(id: &str, model: &str, base_url: &str, api_key: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            id: id.to_owned(),
            format: ProviderFormat::ChatCompletions,
            base_url: base_url.to_owned(),
            api_key: api_key.map(str::to_owned),
            auth_scheme: None,
            headers: IndexMap::new(),
            proxy: None,
            models: vec![model.to_owned()],
            context_window: None,
            max_completion_tokens: Some(4_096),
            auth_account: None,
            reasoning_efforts: Vec::new(),
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
            on_errors: vec![FallbackErrorClass::Network],
        }
    }

    /// A test actor whose catalog is `providers` and whose chains are `chains`.
    async fn actor_with(
        providers: Vec<ProviderConfig>,
        chains: Vec<ModelFallback>,
    ) -> crate::session::acp_session::SessionActor {
        actor_with_cfg(Config {
            providers,
            model_fallbacks: chains,
            ..Config::default()
        })
        .await
    }

    /// The same, with `[models] prompt_suggestion` pinned to `key` — the
    /// catalog-guarded tier, so a key the catalog cannot place is a skip.
    async fn actor_suggesting_with(
        providers: Vec<ProviderConfig>,
        chains: Vec<ModelFallback>,
        key: &str,
    ) -> crate::session::acp_session::SessionActor {
        actor_with_cfg(Config {
            providers,
            model_fallbacks: chains,
            prompt_suggest_model_pin: crate::config::PromptSuggestModelPin::Pinned(key.to_owned()),
            ..Config::default()
        })
        .await
    }

    /// `max_retries` is zeroed so a refused connection fails at once instead of
    /// spending the transport budget the sampler owns separately.
    async fn actor_with_cfg(cfg: Config) -> crate::session::acp_session::SessionActor {
        let catalog: IndexMap<String, ModelEntry> = resolve_model_list(&cfg, None);
        let tmp = std::env::temp_dir().join("grok-test-aux-fallback");
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
        let mut actor = crate::session::acp_session::support::create_test_actor(
            0,
            256_000,
            85,
            gateway_tx,
            persistence_tx,
        )
        .await;
        actor.models_manager = models_manager;
        actor.max_retries = 0;
        actor
    }

    /// Put the live session on `<provider>/<model>`, so a call that rides the
    /// session's own client goes where the test's catalog says it does.
    fn point_session_at(
        actor: &crate::session::acp_session::SessionActor,
        model: &str,
        base_url: &str,
    ) {
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

    /// The minimum a suggestion needs: a user turn and an agent reply.
    fn seed_conversation(actor: &crate::session::acp_session::SessionActor) {
        use xai_grok_sampling_types::conversation::ConversationItem;
        actor.chat_state_handle.replace_conversation(vec![
            ConversationItem::user("fix the flaky test".to_owned()),
            ConversationItem::assistant("Fixed it.".to_owned()),
        ]);
    }

    fn probe_request(model: &str) -> ConversationRequest {
        ConversationRequest {
            items: vec![
                xai_grok_sampling_types::conversation::ConversationItem::user("hi".to_owned()),
            ],
            model: Some(model.to_owned()),
            ..Default::default()
        }
    }

    /// The gap this module closes: an aux one-shot whose provider is unreachable
    /// used to fail outright. It now re-issues on the chain's target, and the
    /// substituted model — not the origin's — is what goes on the wire.
    #[tokio::test]
    async fn a_dead_origin_endpoint_is_re_issued_on_the_chain_target() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let backup = MockInferenceServer::start().await.unwrap();
                backup.set_response("answered by the backup");
                let actor = actor_with(
                    vec![
                        provider("origin", "origin-model", &dead_url(), Some("origin-key")),
                        provider("backup", "backup-model", &backup.url(), Some("backup-key")),
                    ],
                    vec![chain("origin/origin-model", &["backup/backup-model"])],
                )
                .await;

                let route = actor
                    .resolve_aux_route("origin/origin-model", false)
                    .await
                    .expect("the origin pin resolves; only the endpoint is down");
                let request = probe_request(&route.wire_model);
                let response = actor
                    .aux_collect_with_fallback("test", route, request, 1)
                    .await
                    .expect("the chain target answers");

                assert!(
                    response.assistant_text().contains("backup"),
                    "got: {}",
                    response.assistant_text()
                );
                let bodies = backup.request_bodies();
                assert_eq!(bodies.len(), 1, "exactly one re-issue");
                assert_eq!(
                    bodies[0].get("model").and_then(|m| m.as_str()),
                    Some("backup-model"),
                    "the hop must name the target entry's own slug, not the origin's"
                );
            })
            .await;
    }

    /// A budget of zero is the "no failover" setting, and it is enforced before
    /// any chain lookup: nothing is substituted and the target never sees a
    /// request.
    #[tokio::test]
    async fn a_zero_budget_surfaces_the_error_without_substituting() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let backup = MockInferenceServer::start().await.unwrap();
                let actor = actor_with(
                    vec![
                        provider("origin", "origin-model", &dead_url(), Some("origin-key")),
                        provider("backup", "backup-model", &backup.url(), Some("backup-key")),
                    ],
                    vec![chain("origin/origin-model", &["backup/backup-model"])],
                )
                .await;

                let route = actor
                    .resolve_aux_route("origin/origin-model", false)
                    .await
                    .expect("resolves");
                let request = probe_request(&route.wire_model);
                assert!(
                    actor
                        .aux_collect_with_fallback("test", route, request, 0)
                        .await
                        .is_err()
                );
                assert!(
                    backup.requests().is_empty(),
                    "a zero budget must not reach the chain at all"
                );
            })
            .await;
    }

    /// A chain target that is not in this shell's catalog is an *availability*
    /// answer, exactly like the caller's own catalog miss that skips the request
    /// before it starts. The hop stops there; it never degrades onto some other
    /// client that happens to be at hand.
    #[tokio::test]
    async fn a_chain_target_the_catalog_cannot_place_stops_the_hop() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bystander = MockInferenceServer::start().await.unwrap();
                let actor = actor_with(
                    vec![
                        provider("origin", "origin-model", &dead_url(), Some("origin-key")),
                        // In the catalog, but not named by the chain.
                        provider(
                            "bystander",
                            "bystander-model",
                            &bystander.url(),
                            Some("bystander-key"),
                        ),
                    ],
                    vec![chain("origin/origin-model", &["not-in-this-catalog"])],
                )
                .await;

                let route = actor
                    .resolve_aux_route("origin/origin-model", false)
                    .await
                    .expect("resolves");
                let request = probe_request(&route.wire_model);
                assert!(
                    actor
                        .aux_collect_with_fallback("test", route, request, 2)
                        .await
                        .is_err(),
                    "an unroutable target surfaces the original error"
                );
                assert!(
                    bystander.requests().is_empty(),
                    "an unroutable target must never fall through onto another entry"
                );
            })
            .await;
    }

    // ── Per-caller wiring. Each proves the shipped caller reaches the loop
    //    above, with its own budget, rather than issuing straight at a client.
    //    They share the harness above rather than restating it four times.

    /// Prompt suggestion: the pin's provider is down, the chain names another,
    /// and ghost text is produced instead of the feature going quiet.
    #[tokio::test]
    async fn prompt_suggest_hops_to_the_chain_target() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let backup = MockInferenceServer::start().await.unwrap();
                backup.set_response("run the tests");
                let actor = actor_suggesting_with(
                    vec![
                        provider("origin", "origin-model", &dead_url(), Some("origin-key")),
                        provider("backup", "backup-model", &backup.url(), Some("backup-key")),
                    ],
                    vec![chain("origin/origin-model", &["backup/backup-model"])],
                    "origin/origin-model",
                )
                .await;
                seed_conversation(&actor);

                assert_eq!(
                    actor.handle_suggest_prompt(None).await.as_deref(),
                    Some("run the tests")
                );
                assert_eq!(backup.request_bodies().len(), 1);
            })
            .await;
    }

    /// ...and the guard that must survive it: a pin this catalog cannot place is
    /// still a *skip*. No request is made, so no chain is ever consulted — a
    /// controlled disable must not become a fallback.
    #[tokio::test]
    async fn prompt_suggest_catalog_miss_skips_instead_of_entering_a_chain() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let backup = MockInferenceServer::start().await.unwrap();
                backup.set_response("should never be asked");
                let actor = actor_suggesting_with(
                    vec![provider(
                        "backup",
                        "backup-model",
                        &backup.url(),
                        Some("backup-key"),
                    )],
                    // A chain that would fire for the absent pin if a miss could
                    // ever reach one.
                    vec![chain("absent/absent-model", &["backup/backup-model"])],
                    "absent/absent-model",
                )
                .await;
                seed_conversation(&actor);

                assert_eq!(actor.handle_suggest_prompt(None).await, None);
                assert!(
                    backup.requests().is_empty(),
                    "a catalog miss must not send anything anywhere"
                );
            })
            .await;
    }

    /// `web_fetch` distillation: the shipped service loop, with the production
    /// resolve and collect seams, re-issues on the chain target instead of
    /// returning the raw page.
    #[tokio::test]
    async fn web_fetch_distillation_hops_to_the_chain_target() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let backup = MockInferenceServer::start().await.unwrap();
                backup.set_response("the page says the limit is 60 rpm");
                let actor = std::sync::Arc::new(
                    actor_with(
                        vec![
                            provider("origin", "origin-model", &dead_url(), Some("origin-key")),
                            provider("backup", "backup-model", &backup.url(), Some("backup-key")),
                        ],
                        vec![chain("origin/origin-model", &["backup/backup-model"])],
                    )
                    .await,
                );
                actor.wire_web_fetch_distiller().await;

                let bridge = actor.agent.borrow().tool_bridge().clone();
                let distiller: xai_grok_tools::implementations::grok_build::web_fetch::WebContentDistillerResource =
                    bridge
                        .read_resource()
                        .await
                        .expect("the distiller is registered on the bridge");
                let answer = distiller
                    .distiller()
                    .infer("origin/origin-model", "system", "user")
                    .await
                    .expect("the chain target distils the page");

                assert!(answer.contains("60 rpm"), "got: {answer}");
                assert_eq!(backup.request_bodies().len(), 1);
            })
            .await;
    }

    /// A chat-completions stream carrying one `session_title` tool call, which
    /// is the only shape title generation accepts as a model-written title —
    /// anything else falls through to truncated user text and would make these
    /// two tests unable to tell a hop from a degrade.
    fn title_tool_call_sse(title: &str) -> xai_grok_test_support::ScriptedResponse {
        use serde_json::{Value, json};
        use xai_grok_test_support::{ScriptedResponse, SseEvent};
        let chunk = |delta: Value, finish_reason: Value| {
            SseEvent::data(
                json!({
                    "id": "chatcmpl-title",
                    "object": "chat.completion.chunk",
                    "created": 1_234_567_890,
                    "model": "test-model",
                    "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }]
                })
                .to_string(),
            )
        };
        ScriptedResponse::sse(vec![
            chunk(
                json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_session_title",
                        "type": "function",
                        "function": {
                            "name": "session_title",
                            "arguments": json!({ "session_title": title }).to_string()
                        }
                    }]
                }),
                Value::Null,
            ),
            chunk(json!({}), json!("tool_calls")),
            SseEvent::data("[DONE]"),
        ])
    }

    /// Drive the shipped title generator to the title it routes back through
    /// the persistence channel.
    async fn title_from(
        actor: &std::sync::Arc<crate::session::acp_session::SessionActor>,
        pin: &str,
        user_text: &str,
    ) -> String {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut generator = crate::session::summary::SummaryGenerator::new(
            crate::session::summary::SummaryConfig {
                model: pin.to_owned(),
                persistence_tx: tx.downgrade(),
                aux: Some(actor.spawn_aux_inference_bridge()),
            },
        );
        generator.update(user_text.to_owned());
        match rx.recv().await {
            Some(PersistenceMsg::GeneratedTitle(title)) => title,
            other => panic!("expected a generated title, got {:?}", other.is_some()),
        }
    }

    /// The gap this wiring closes. A session title used to be issued on a
    /// client built at session open, which could only ever reach the one
    /// endpoint it was built for — so a title model that was throttled or down
    /// silently produced a title cut from the user's own first ten words, and
    /// that title is permanent. It now hops onto the operator's chain.
    #[tokio::test]
    async fn session_title_hops_to_the_chain_target() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let backup = MockInferenceServer::start().await.unwrap();
                backup.enqueue_response(
                    "/v1/chat/completions",
                    title_tool_call_sse("Fix flaky auth test in login"),
                );
                let actor = std::sync::Arc::new(
                    actor_with(
                        vec![
                            provider("origin", "origin-model", &dead_url(), Some("origin-key")),
                            provider("backup", "backup-model", &backup.url(), Some("backup-key")),
                        ],
                        vec![chain("origin/origin-model", &["backup/backup-model"])],
                    )
                    .await,
                );

                let title = title_from(
                    &actor,
                    "origin/origin-model",
                    "fix the flaky auth test in login.rs please",
                )
                .await;

                assert_eq!(
                    title, "Fix flaky auth test in login",
                    "the chain target's title, not the truncated-user-text degrade"
                );
                let bodies = backup.request_bodies();
                assert_eq!(bodies.len(), 1, "exactly one re-issue");
                assert_eq!(
                    bodies[0].get("model").and_then(|m| m.as_str()),
                    Some("backup-model"),
                    "the hop names the target entry's own slug, not the pin"
                );
            })
            .await;
    }

    /// ...and the guard that must survive it. A title pin this catalog cannot
    /// place is an availability answer, so it never enters that pin's chain: it
    /// degrades to the session's own route, which is the one slug the session's
    /// endpoint is known to serve. Forcing the pin's slug onto that endpoint
    /// would only 404, and entering its chain would send the title to a
    /// provider the operator never pointed titling at.
    #[tokio::test]
    async fn session_title_catalog_miss_degrades_without_entering_the_pins_chain() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let session_server = MockInferenceServer::start().await.unwrap();
                session_server.enqueue_response(
                    "/v1/chat/completions",
                    title_tool_call_sse("Titled by the session model"),
                );
                let backup = MockInferenceServer::start().await.unwrap();
                backup.set_response("should never be asked");
                let actor = std::sync::Arc::new(
                    actor_with(
                        vec![
                            provider(
                                "session-provider",
                                "session-model",
                                &session_server.url(),
                                Some("session-key"),
                            ),
                            provider("backup", "backup-model", &backup.url(), Some("backup-key")),
                        ],
                        // A chain that would fire for the absent pin if a miss
                        // could ever reach one.
                        vec![chain("absent/absent-model", &["backup/backup-model"])],
                    )
                    .await,
                );
                point_session_at(&actor, "session-model", &session_server.url());

                let title = title_from(&actor, "absent/absent-model", "fix the auth bug").await;

                assert_eq!(title, "Titled by the session model");
                let bodies = session_server.request_bodies();
                assert_eq!(bodies.len(), 1);
                assert_eq!(
                    bodies[0].get("model").and_then(|m| m.as_str()),
                    Some("session-model"),
                    "the absent pin must never go on the wire"
                );
                assert!(
                    backup.requests().is_empty(),
                    "a catalog miss must not enter the pin's chain"
                );
            })
            .await;
    }

    /// Image description: the call the model's sight depends on. Its origin
    /// endpoint is down and the chain carries it, so the turn is not built
    /// around an image nobody described.
    #[tokio::test]
    async fn image_description_hops_to_the_chain_target() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let backup = MockInferenceServer::start().await.unwrap();
                backup.set_response("a screenshot of a failing test");
                let actor = actor_with(
                    vec![
                        provider("origin", "origin-model", &dead_url(), Some("origin-key")),
                        provider("backup", "backup-model", &backup.url(), Some("backup-key")),
                    ],
                    vec![chain("origin/origin-model", &["backup/backup-model"])],
                )
                .await;
                let route = actor
                    .resolve_aux_route("origin/origin-model", false)
                    .await
                    .expect("resolves");

                let description = crate::session::image_describe::describe_user_images(
                    |request| {
                        actor.aux_collect_with_fallback(
                            "image_describe",
                            route.clone(),
                            request,
                            AUX_HOPS_IMAGE_DESCRIBE,
                        )
                    },
                    &route.wire_model,
                    "describe this".to_owned(),
                    &["data:image/png;base64,AAAA".to_owned()],
                )
                .await
                .expect("the chain target describes the image");

                assert!(description.contains("failing test"), "got: {description}");
                assert_eq!(backup.request_bodies().len(), 1);
            })
            .await;
    }

    /// Auto-mode permission classifier, unpinned: it rides the session's own
    /// client, and that route is described well enough that a chain written
    /// against the session model still carries it. The whole point is that the
    /// no-dedicated-model case does not quietly opt out of failover — without
    /// this the verdict silently drops to the heuristic whenever the session's
    /// provider is unwell.
    ///
    /// Driven through the shipped route builder and the shipped request builder
    /// rather than `wire_permission_auto_llm_classifier`, because that reads the
    /// developer's real auto-mode config off disk and would decide which branch
    /// this takes.
    #[tokio::test]
    async fn permission_classifier_on_the_session_client_hops_to_the_chain_target() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let backup = MockInferenceServer::start().await.unwrap();
                backup.set_response(r#"{"thinking":"","shouldBlock":false,"reason":"safe"}"#);
                let actor = actor_with(
                    vec![
                        provider("origin", "origin-model", &dead_url(), Some("origin-key")),
                        provider("backup", "backup-model", &backup.url(), Some("backup-key")),
                    ],
                    vec![chain("origin/origin-model", &["backup/backup-model"])],
                )
                .await;
                // Put the session itself on the dead provider, the way a live
                // session with no classifier pin would be.
                point_session_at(&actor, "origin-model", &dead_url());

                let route = actor
                    .session_client_aux_route(false)
                    .await
                    .expect("the session client builds");
                assert_eq!(route.wire_model, "origin-model");
                assert_eq!(
                    route.catalog_key, "origin/origin-model",
                    "the session model must be named by its catalog key, or no chain matches it"
                );

                let request = super::super::build_permission_classifier_request(
                    &route.client.api_backend(),
                    route.wire_model.clone(),
                    vec![],
                    None,
                    "test-session".to_owned(),
                );
                let response = actor
                    .aux_collect_with_fallback(
                        "permission_classifier",
                        route,
                        request,
                        AUX_HOPS_PERMISSION_CLASSIFIER,
                    )
                    .await
                    .expect("the chain target answers the side-query");

                assert!(
                    super::super::classifier_verdict_text(&response).contains("shouldBlock"),
                    "an enforceable verdict, not a silent drop to the heuristic"
                );
                assert_eq!(backup.request_bodies().len(), 1);
            })
            .await;
    }

    /// The entry condition, stated as a test: a model the catalog cannot place
    /// has no route, so a caller holding `None` never has a client to hand this
    /// module and the chain is structurally unreachable. A catalog miss stays a
    /// skip and can never become a fallback hop.
    #[tokio::test]
    async fn a_catalog_miss_yields_no_route_so_no_chain_can_be_entered() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let backup = MockInferenceServer::start().await.unwrap();
                let actor = actor_with(
                    vec![provider(
                        "backup",
                        "backup-model",
                        &backup.url(),
                        Some("backup-key"),
                    )],
                    // A chain that would fire for the missing pin, if anything
                    // could ever get far enough to consult it.
                    vec![chain("absent/absent-model", &["backup/backup-model"])],
                )
                .await;

                assert!(
                    actor
                        .resolve_aux_route("absent/absent-model", false)
                        .await
                        .is_none(),
                    "a pin the catalog cannot place resolves to no route"
                );
                assert!(
                    backup.requests().is_empty(),
                    "and nothing is sent anywhere on its behalf"
                );
            })
            .await;
    }
}
