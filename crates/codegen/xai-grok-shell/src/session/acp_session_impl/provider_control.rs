//! Provider request interception and provider/model failover.
//!
//! Two seams, both bridging the sampler's callback-injection points to the
//! plugin hook dispatcher without the sampler ever depending on the hooks
//! crate:
//!
//! * [`HookRequestInterceptor`] implements the sampler's
//!   [`xai_grok_sampler::RequestInterceptor`] by firing the `provider_request`
//!   Replace hook. It is attached to the per-turn sampler config only when a
//!   hook is actually subscribed (see
//!   [`SessionActor::build_hook_request_interceptor`]).
//! * The provider/model failover loop lives at the turn call-site
//!   (`run_turn_via_sampler`). On a provider error it consults the
//!   `provider_error` hook first; on passthrough/absence it falls back to the
//!   config-driven built-in chains (`[[model_fallbacks]]`). This ordering makes
//!   a plugin directive win over the built-in chain, while the chain keeps
//!   working even when no plugin is wired.
//!
//! Credentials never cross either seam: the sampler strips auth headers before
//! calling the interceptor and re-attaches them afterwards.

use std::sync::Arc;

use xai_grok_hooks::discovery::HookRegistry;
use xai_grok_hooks::event::{HookEventEnvelope, HookEventName, HookPayload};
use xai_grok_hooks::invoker::PluginHookInvoker;
use xai_grok_sampler::{
    ErrorDirective, RequestInterceptor, RequestReplacement, RequestView, SamplerConfig,
    SeamFuture, SharedRequestInterceptor,
};

use super::*;

/// Default cap on provider-fallback model switches within a single turn when
/// `[provider_fallback_max_attempts]` is unset. Distinct from the sampler's
/// internal transport retry budget.
const DEFAULT_PROVIDER_FALLBACK_ATTEMPTS: u32 = 3;

/// What a `provider_error` directive resolves to, before the built-in chain is
/// consulted. Factored out so the hook-vs-chain priority is unit-testable
/// without a live plugin hook.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FailoverDirectiveOutcome {
    /// Surface the error immediately; no failover.
    Fail,
    /// A plugin chose this model — it wins over the built-in chain.
    UseModel(String),
    /// No plugin directive (passthrough / no hook); defer to the built-in
    /// `[[model_fallbacks]]` chain.
    UseChain,
}

/// Map a `provider_error` directive to a failover outcome. A `Retry` from a
/// plugin wins over the built-in chain; `Passthrough` defers to it; `Fail`
/// stops failover.
pub(super) fn failover_outcome_for_directive(
    directive: ErrorDirective,
    current_model: &str,
) -> FailoverDirectiveOutcome {
    match directive {
        ErrorDirective::Fail => FailoverDirectiveOutcome::Fail,
        // A `Retry` with no model substitution retries the same model.
        ErrorDirective::Retry { model, .. } => {
            FailoverDirectiveOutcome::UseModel(model.unwrap_or_else(|| current_model.to_string()))
        }
        ErrorDirective::Passthrough => FailoverDirectiveOutcome::UseChain,
    }
}

/// A [`RequestInterceptor`] that fires the `provider_request` Replace hook.
///
/// It owns everything needed to build a hook envelope and run context from a
/// background task, since the sampler invokes it off the session's thread. The
/// registry, plugin invoker, and envelope identity fields are snapshotted when
/// the interceptor is built (per turn), so a mid-session change is picked up on
/// the next turn.
pub(crate) struct HookRequestInterceptor {
    session_id: String,
    cwd: String,
    workspace_root: String,
    transcript_path: Option<String>,
    permission_mode: Option<String>,
    /// Identity of the session issuing the request: `main` for the top-level
    /// session, otherwise the subagent type. Captured per session at build time
    /// (the interceptor runs off the session thread, so `self` is unavailable
    /// inside `intercept`).
    agent: String,
    /// Normalized names of the tools available to `agent`, snapshotted from the
    /// session tool catalog at build time (same reason as `agent`).
    tools: Vec<String>,
    registry: Arc<HookRegistry>,
    plugin_invoker: Option<Arc<dyn PluginHookInvoker>>,
}

impl std::fmt::Debug for HookRequestInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRequestInterceptor")
            .field("session_id", &self.session_id)
            .field("agent", &self.agent)
            .field("tools", &self.tools)
            .field("has_plugin_invoker", &self.plugin_invoker.is_some())
            .finish()
    }
}

impl RequestInterceptor for HookRequestInterceptor {
    fn intercept<'a>(
        &'a self,
        view: &'a RequestView,
    ) -> SeamFuture<'a, Option<RequestReplacement>> {
        Box::pin(async move {
            let envelope = HookEventEnvelope {
                hook_event_name: HookEventName::ProviderRequest,
                session_id: self.session_id.clone(),
                cwd: self.cwd.clone(),
                workspace_root: self.workspace_root.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                transcript_path: self.transcript_path.clone(),
                client_identifier: None,
                prompt_id: None,
                permission_mode: self.permission_mode.clone(),
                payload: HookPayload::ProviderRequest {
                    endpoint: view.endpoint.clone(),
                    model: view.model.clone(),
                    base_url_alias: view.base_url_alias.clone(),
                    agent: self.agent.clone(),
                    tools: self.tools.clone(),
                    headers: view.headers.clone(),
                    body: view.body.clone(),
                },
            };
            let ctx = xai_grok_hooks::runner::RunContext {
                session_id: &self.session_id,
                workspace_root: &self.workspace_root,
                plugin_invoker: self.plugin_invoker.clone(),
            };
            let replaced = xai_grok_hooks::dispatcher::dispatch_replace(
                &self.registry,
                HookEventName::ProviderRequest,
                &envelope,
                &ctx,
            )
            .await;
            // No hook replaced -> passthrough. A replacement that does not
            // deserialize into the expected shape fails open (passthrough)
            // rather than corrupting the request.
            match replaced {
                Some(value) => match serde_json::from_value::<RequestReplacement>(value) {
                    Ok(replacement) => Some(replacement),
                    Err(err) => {
                        tracing::warn!(%err, "provider_request replacement did not parse; ignoring");
                        None
                    }
                },
                None => None,
            }
        })
    }
}

impl SessionActor {
    /// Build a `provider_request` interceptor for the current turn, or `None`
    /// when no hook is subscribed. Gating here keeps the hot path free of any
    /// body-serialization round-trip when nothing is listening.
    ///
    /// The agent identity and tool catalog are snapshotted here, on the session
    /// thread, because the interceptor runs off it (the sampler invokes it from
    /// a background task) where neither `self` nor the tool bridge is reachable.
    /// Capturing per session also gives a subagent's turns its own type rather
    /// than the parent's `main` — the parent-inherited seed interceptor is
    /// replaced on every turn by this rebuild.
    pub(super) async fn build_hook_request_interceptor(&self) -> Option<SharedRequestInterceptor> {
        let registry = self.hook_registry.borrow().clone()?;
        if !registry.has_enabled_hooks_for_canonical(HookEventName::ProviderRequest) {
            return None;
        }
        Some(Arc::new(self.assemble_request_interceptor(registry).await))
    }

    /// Assemble the interceptor with the identity and tool catalog snapshotted
    /// from this session. Factored out of the gate above so the population is
    /// unit-testable without wiring a live `provider_request` hook.
    async fn assemble_request_interceptor(
        &self,
        registry: Arc<HookRegistry>,
    ) -> HookRequestInterceptor {
        let plugin_invoker = self
            .plugin_host
            .clone()
            .map(|h| h as Arc<dyn PluginHookInvoker>);
        // `main` for the root session, else the subagent type — the same
        // identity the tool-surface and permission layers key on.
        let agent = self
            .subagent_type_label()
            .unwrap_or_else(|| "main".to_string());
        // Names of the tools available to this agent, from the session catalog
        // (the single, format-agnostic source), rather than re-parsing the
        // per-format request body. Includes MCP/plugin tools (`plugin__tool`),
        // which the wire-facing builtin list hides — an injector gating on a
        // companion tool needs to see it. Clone the bridge Arc before the await
        // so no `RefCell` borrow is held across it.
        let bridge = self.agent.borrow().tool_bridge().clone();
        let tools = bridge
            .tool_definitions()
            .await
            .into_iter()
            .map(|d| d.function.name)
            .collect();
        HookRequestInterceptor {
            session_id: self.session_id_string(),
            cwd: self.session_info.cwd.clone(),
            workspace_root: self.hook_workspace_root(),
            transcript_path: self.get_transcript_path(),
            permission_mode: Some(self.permission_mode_label().to_string()),
            agent,
            tools,
            registry,
            plugin_invoker,
        }
    }

    /// Consult the `provider_error` Replace hook for a directive on a failed
    /// request. Returns [`ErrorDirective::Passthrough`] when no hook is
    /// subscribed or the hook passed through / returned an unparseable value
    /// (fail-open). A `Retry` directive from a plugin wins over the built-in
    /// fallback chain.
    pub(super) async fn consult_provider_error_hook(
        &self,
        error_class: &str,
        model: &str,
        base_url_alias: &str,
        attempt: u32,
    ) -> ErrorDirective {
        let Some(registry) = self.hook_registry.borrow().clone() else {
            return ErrorDirective::Passthrough;
        };
        if !registry.has_enabled_hooks_for_canonical(HookEventName::ProviderError) {
            return ErrorDirective::Passthrough;
        }
        let envelope = self.make_hook_envelope(
            HookEventName::ProviderError,
            None,
            HookPayload::ProviderError {
                error_class: error_class.to_string(),
                model: model.to_string(),
                attempt,
                base_url_alias: base_url_alias.to_string(),
            },
        );
        let ctx = self.hook_run_ctx();
        match xai_grok_hooks::dispatcher::dispatch_replace(
            &registry,
            HookEventName::ProviderError,
            &envelope,
            &ctx,
        )
        .await
        {
            // The tolerant `Deserialize` on `ErrorDirective` fails open to
            // `Passthrough` on any shape it does not recognize.
            Some(value) => {
                serde_json::from_value::<ErrorDirective>(value).unwrap_or(ErrorDirective::Passthrough)
            }
            None => ErrorDirective::Passthrough,
        }
    }

    /// The cap on provider-fallback model switches within a single turn.
    /// Distinct from the sampler's internal transport retry budget: this counts
    /// only model/provider substitutions. Defaults to 3.
    pub(super) fn provider_fallback_max_attempts(&self) -> u32 {
        self.models_manager
            .provider_fallback_max_attempts()
            .unwrap_or(DEFAULT_PROVIDER_FALLBACK_ATTEMPTS)
    }

    /// Pick the first eligible fallback target for `current_model` on an error
    /// of `error_class` from the built-in `[[model_fallbacks]]` chains, honoring
    /// per-`(from, to)` cooldowns. Returns the chosen target model and its
    /// configured cooldown (so the caller can arm it), or `None` when no chain
    /// applies or every target is still cooling down.
    pub(super) fn builtin_fallback_target(
        &self,
        current_model: &str,
        error_class: &str,
    ) -> Option<(String, std::time::Duration)> {
        let now = std::time::Instant::now();
        let chains = self.models_manager.model_fallbacks();
        let cooldowns = self.provider_fallback_cooldowns.lock();
        for chain in &chains {
            if chain.from != current_model || !chain.triggers_on(error_class) {
                continue;
            }
            let cooldown = std::time::Duration::from_secs(chain.cooldown_seconds);
            for target in &chain.to {
                let key = (chain.from.clone(), target.clone());
                let cooling = cooldowns
                    .get(&key)
                    .is_some_and(|armed| now.duration_since(*armed) < cooldown);
                if !cooling {
                    return Some((target.clone(), cooldown));
                }
            }
        }
        None
    }

    /// Arm the cooldown for a `(from, to)` fallback pair.
    pub(super) fn arm_fallback_cooldown(&self, from: &str, to: &str) {
        self.provider_fallback_cooldowns
            .lock()
            .insert((from.to_string(), to.to_string()), std::time::Instant::now());
    }

    /// Build a sampler config that re-issues the current turn against
    /// `target_model`. A catalog model resolves to its own base URL / backend /
    /// credentials (cross-provider failover); an unknown target is treated as a
    /// same-provider model swap on the active config. Either way the session's
    /// local seams (interceptor, bearer resolver, attribution) are carried over.
    pub(super) async fn build_failover_config(
        &self,
        active: &SamplerConfig,
        target_model: &str,
    ) -> SamplerConfig {
        let models = self.models_manager.models();
        let creds = self.chat_state_handle.get_credentials().await;
        let resolved = crate::agent::config::resolve_model_to_sampling_config(
            target_model,
            &models,
            creds.api_key.as_deref(),
            creds.alpha_test_key.clone(),
            creds.client_version.clone(),
            None,
        );
        let mut cfg = match resolved {
            Some(mut cfg) => {
                crate::agent::config::stamp_session_local_sampler_fields(
                    &mut cfg,
                    active,
                    self.client_identifier.clone(),
                    Some(self.max_retries),
                );
                cfg
            }
            None => {
                let mut cfg = active.clone();
                cfg.model = target_model.to_string();
                cfg
            }
        };
        cfg.idle_timeout_secs = Some(self.inference_idle_timeout.as_secs());
        cfg
    }

    /// Attempt a provider/model failover after a provider error. Consults the
    /// `provider_error` hook first (a plugin `Retry` directive wins); otherwise
    /// falls back to the built-in `[[model_fallbacks]]` chains. On success it
    /// installs the substituted config on the sampler, surfaces a retry notice,
    /// and returns the installed config so the caller can re-issue and keep it
    /// as the new failover base. Returns `None` when nothing applies (the caller
    /// then runs its normal error path).
    pub(super) async fn try_provider_failover(
        &self,
        error_class: &str,
        active_config: &SamplerConfig,
        current_model: &str,
        attempt: u32,
        max_attempts: u32,
    ) -> Option<SamplerConfig> {
        // 1) Programmable layer: the plugin hook decides first.
        let directive = self
            .consult_provider_error_hook(
                error_class,
                current_model,
                &active_config.base_url,
                attempt,
            )
            .await;
        let target = match failover_outcome_for_directive(directive, current_model) {
            FailoverDirectiveOutcome::Fail => return None,
            FailoverDirectiveOutcome::UseModel(model) => model,
            FailoverDirectiveOutcome::UseChain => {
                // 2) Built-in chain.
                let (target, cooldown) = self.builtin_fallback_target(current_model, error_class)?;
                if cooldown > std::time::Duration::ZERO {
                    self.arm_fallback_cooldown(current_model, &target);
                }
                target
            }
        };

        let config = self.build_failover_config(active_config, &target).await;
        let switched_model = config.model.clone();
        self.sampler_handle.update_config(config.clone());
        self.send_xai_notification(XaiSessionUpdate::RetryState(
            crate::extensions::notification::RetryState::Retrying {
                attempt,
                max_retries: max_attempts,
                reason: format!(
                    "Provider error ({error_class}); falling back to model {switched_model}"
                ),
            },
        ))
        .await;
        Some(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::{Config, FallbackErrorClass, ModelFallback};

    fn retry(model: Option<&str>) -> ErrorDirective {
        ErrorDirective::Retry {
            model: model.map(str::to_string),
            base_url_alias: None,
            max_attempts: None,
        }
    }

    // ── Directive priority (pure): a plugin `Retry` wins over the built-in
    //    chain; `Passthrough` defers to it; `Fail` stops failover. ──────────

    #[test]
    fn directive_retry_with_model_wins_over_chain() {
        assert_eq!(
            failover_outcome_for_directive(retry(Some("plugin-choice")), "primary"),
            FailoverDirectiveOutcome::UseModel("plugin-choice".to_string())
        );
    }

    #[test]
    fn directive_retry_without_model_retries_same_model() {
        assert_eq!(
            failover_outcome_for_directive(retry(None), "primary"),
            FailoverDirectiveOutcome::UseModel("primary".to_string())
        );
    }

    #[test]
    fn directive_fail_stops_failover() {
        assert_eq!(
            failover_outcome_for_directive(ErrorDirective::Fail, "primary"),
            FailoverDirectiveOutcome::Fail
        );
    }

    #[test]
    fn directive_passthrough_defers_to_chain() {
        assert_eq!(
            failover_outcome_for_directive(ErrorDirective::Passthrough, "primary"),
            FailoverDirectiveOutcome::UseChain
        );
    }

    // ── Built-in chain + cooldown (actor-backed). ──────────────────────────

    fn chain(from: &str, to: &[&str], cooldown: u64, on: Vec<FallbackErrorClass>) -> ModelFallback {
        ModelFallback {
            from: from.to_string(),
            to: to.iter().map(|s| s.to_string()).collect(),
            cooldown_seconds: cooldown,
            on_errors: on,
        }
    }

    fn models_manager_with(chains: Vec<ModelFallback>) -> crate::agent::models::ModelsManager {
        let cfg = Config {
            model_fallbacks: chains,
            ..Config::default()
        };
        let tmp = std::env::temp_dir().join("grok-test-provider-fallback");
        let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new(
            &tmp,
            crate::auth::GrokComConfig::default(),
        ));
        crate::agent::models::ModelsManager::new(
            None,
            indexmap::IndexMap::new(),
            agent_client_protocol::ModelId::new("primary"),
            auth_manager,
            cfg,
        )
    }

    async fn make_actor(mm: crate::agent::models::ModelsManager) -> SessionActor {
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
        actor.models_manager = mm;
        actor
    }

    #[tokio::test(flavor = "current_thread")]
    async fn builtin_chain_matches_and_respects_cooldown() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mm = models_manager_with(vec![chain(
                    "primary",
                    &["backup", "tertiary"],
                    60,
                    vec![FallbackErrorClass::ServerError],
                )]);
                let actor = make_actor(mm).await;

                // Wrong model / unlisted error class → no match.
                assert!(actor.builtin_fallback_target("other", "5xx").is_none());
                assert!(actor.builtin_fallback_target("primary", "rate_limit").is_none());

                // Match → first target, with the configured cooldown.
                let (target, cooldown) = actor
                    .builtin_fallback_target("primary", "5xx")
                    .expect("chain should match");
                assert_eq!(target, "backup");
                assert_eq!(cooldown, std::time::Duration::from_secs(60));

                // Arming the first pair's cooldown makes the next lookup skip it
                // and select the second target.
                actor.arm_fallback_cooldown("primary", "backup");
                let (target, _) = actor
                    .builtin_fallback_target("primary", "5xx")
                    .expect("second target after first cools down");
                assert_eq!(target, "tertiary");
            })
            .await;
    }

    // ── Interceptor population: agent identity + tool catalog snapshot. ─────

    fn empty_registry() -> Arc<HookRegistry> {
        Arc::new(xai_grok_hooks::discovery::load_hooks_from_sources(&[], &[]).0)
    }

    async fn session_catalog(actor: &SessionActor) -> Vec<String> {
        let bridge = actor.agent.borrow().tool_bridge().clone();
        bridge
            .tool_definitions()
            .await
            .into_iter()
            .map(|d| d.function.name)
            .collect()
    }

    /// A root session reports `main` and snapshots the live session tool
    /// catalog (not the empty placeholder the constructor previously emitted).
    #[tokio::test(flavor = "current_thread")]
    async fn interceptor_root_session_is_main_and_mirrors_catalog() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let actor = make_actor(models_manager_with(vec![])).await;
                // Swap in an agent with real tools so the snapshot is non-empty
                // and provably sourced from the catalog, not the old placeholder.
                *actor.agent.borrow_mut() =
                    crate::session::acp_session::support::test_agent_with_plan_tools().await;
                let interceptor = actor.assemble_request_interceptor(empty_registry()).await;

                assert_eq!(interceptor.agent, "main");
                let catalog = session_catalog(&actor).await;
                assert!(!catalog.is_empty(), "agent with plan tools registers tools");
                assert_eq!(interceptor.tools, catalog);
            })
            .await;
    }

    /// A subagent session reports its task `subagent_type` — the same identity
    /// the tool-surface and permission layers key on — not the parent's `main`.
    #[tokio::test(flavor = "current_thread")]
    async fn interceptor_subagent_reports_its_type() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut actor = make_actor(models_manager_with(vec![])).await;
                actor.startup_hints.is_subagent = true;
                actor.startup_hints.subagent_type = Some("reviewer".to_string());
                let interceptor = actor.assemble_request_interceptor(empty_registry()).await;

                assert_eq!(interceptor.agent, "reviewer");
                // Tools still come from this session's own catalog.
                assert_eq!(interceptor.tools, session_catalog(&actor).await);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failover_substitutes_model_from_builtin_chain() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mm = models_manager_with(vec![chain(
                    "primary",
                    &["backup"],
                    0,
                    vec![FallbackErrorClass::ServerError],
                )]);
                let actor = make_actor(mm).await;
                let active = xai_grok_sampler::SamplerConfig {
                    model: "primary".to_string(),
                    base_url: "https://primary.example/v1".to_string(),
                    ..Default::default()
                };

                // No hook wired → passthrough → the built-in chain switches the
                // model, and the substituted model appears on the re-issued config.
                let switched = actor
                    .try_provider_failover("5xx", &active, "primary", 1, 3)
                    .await
                    .expect("built-in chain should fire on a 5xx");
                assert_eq!(switched.model, "backup");

                // An error class not listed in `on_errors` → no failover.
                assert!(
                    actor
                        .try_provider_failover("auth", &active, "primary", 1, 3)
                        .await
                        .is_none()
                );
            })
            .await;
    }
}
