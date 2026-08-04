//! Shell side of `web_fetch` content distillation.
//!
//! `xai-grok-tools` owns the step itself and defines the seam
//! ([`WebContentDistiller`]); it deliberately owns no routing, and its trait doc
//! names the three rules the host has to keep. This module is the only
//! implementation of that trait, and it keeps them by never routing anything
//! itself:
//!
//! 1. The tool is answered with a catalog *key*
//!    ([`ModelsManager::catalog_key`]), never a routing slug, so the pin's
//!    provider qualification survives the round trip through the tool. A bare
//!    slug names the last-declared entry of that slug, which is a different
//!    provider's endpoint and credential whenever two providers serve one slug.
//! 2. The request goes out on a sampler resolved for that key by
//!    [`SessionActor::resolve_aux_sampler_client`] — the same routed-aux path
//!    prompt suggestion and the Auto-mode classifier use, which pairs the
//!    resolved catalog entry's endpoint and credential with that entry's own
//!    routing slug. One resolution produces both, so the wire `model` and the
//!    route it travels cannot come from different entries. Its `None` means
//!    "this model has no route of its own", and here it means *skip*: riding the
//!    session's client would hand one provider's endpoint and credential another
//!    provider's slug.
//! 3. Nothing first-party is fabricated for a session that is not first-party.
//!    The unpinned default is the session's *own* model, so an unpinned
//!    distillation stays on whatever provider the session already talks to, and
//!    the resolver's own gate (an entry with no credential of its own must sit
//!    on a first-party bearer URL before any credential is attached) is what
//!    decides whether a pinned one runs at all.
//!
//! # Why a channel
//!
//! [`SessionActor`] holds `Cell`/`RefCell` state, so it is not `Sync` and cannot
//! sit behind the `Send + Sync` trait object the tool holds. The `Send + Sync`
//! half is [`ChannelWebContentDistiller`], which answers the two catalog
//! questions in place — [`ModelsManager`] is `Send + Sync` and cheap to clone,
//! which is exactly why the trait splits translating a pin from running an
//! inference — and forwards only `infer` to a task on the session's own
//! `LocalSet`.
//!
//! # Fail open
//!
//! Every error here is a *skip*: the tool's `distill::apply` can only replace a
//! body it already has, so an `Err` from `infer` returns the fetched page
//! untouched. Nothing in this module can produce less than the fetch alone
//! produced.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use xai_grok_sampling_types::conversation::{ConversationItem, ConversationRequest};
use xai_grok_tools::implementations::grok_build::web_fetch::{
    WebContentDistiller, WebContentDistillerResource,
};

use super::SessionActor;
use crate::agent::models::ModelsManager;

/// One distillation request, handed from a `web_fetch` call to the session's
/// `LocalSet`.
pub(crate) struct DistillJob {
    /// The catalog key the tool placed the pin at. Resolved once, here, into a
    /// route and the wire slug that route serves.
    model_key: String,
    system_prompt: String,
    user_prompt: String,
    respond_to: oneshot::Sender<Result<String, String>>,
}

/// The `Send + Sync` half of the distiller: catalog reads answered in place,
/// inference forwarded to [`serve_distill_jobs`].
pub(crate) struct ChannelWebContentDistiller {
    models: ModelsManager,
    jobs: mpsc::UnboundedSender<DistillJob>,
}

impl ChannelWebContentDistiller {
    pub(crate) fn new(models: ModelsManager, jobs: mpsc::UnboundedSender<DistillJob>) -> Self {
        Self { models, jobs }
    }
}

#[async_trait::async_trait]
impl WebContentDistiller for ChannelWebContentDistiller {
    /// Place a configured pin in the catalog and answer with its key.
    ///
    /// The key, not the entry's routing slug: a slug is ambiguous by
    /// construction once two providers serve the same one, and
    /// `config::find_by_slug` resolves that ambiguity to the last-declared
    /// entry. Handing the tool a slug would therefore let a pin spelled
    /// `<provider>/<model>` come back as an id that re-resolves to the *other*
    /// provider — its endpoint, its credential — with nothing downstream able to
    /// tell that the qualification had been dropped.
    fn catalog_key(&self, model_pin: &str) -> Option<String> {
        self.models.catalog_key(model_pin)
    }

    /// The session's own model, as its catalog key.
    ///
    /// Not a compiled-in first-party slug: that would put a session running
    /// entirely on a custom `[[provider]]` onto xAI the moment a bearer happened
    /// to be reachable, which is the first-party request the trait's third rule
    /// forbids fabricating. The session model is the one entry a session is
    /// known to be able to reach, on the account it is already spending.
    ///
    /// It is also not a cost the caller did not already agree to: without
    /// distillation the same page enters this same model's context, in full and
    /// for the rest of the conversation. Operators who want a smaller helper
    /// pin one with `[toolset.web_fetch.distill] model`.
    ///
    /// `None` when the catalog cannot place the current model id — nothing is
    /// reachable, so distillation is skipped.
    fn default_catalog_key(&self) -> Option<String> {
        let current = self.models.current_model_id();
        self.models.catalog_key(current.0.as_ref())
    }

    async fn infer(
        &self,
        model_key: &str,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let (respond_to, answer) = oneshot::channel();
        self.jobs
            .send(DistillJob {
                model_key: model_key.to_owned(),
                system_prompt: system_prompt.to_owned(),
                user_prompt: user_prompt.to_owned(),
                respond_to,
            })
            .map_err(|_| "the session's distillation task is gone")?;
        // The tool applies its own `distill_timeout` around this call. Dropping
        // this receiver on expiry leaves the job running to completion on the
        // session task, where its answer is discarded — one in-flight request,
        // not a leak, and the raw page has already been returned by then.
        Ok(answer.await.map_err(|_| "distillation job was dropped")??)
    }
}

/// Serve distillation jobs until the last sender is dropped.
///
/// `resolve` is [`SessionActor::resolve_aux_sampler_client`] in production: it
/// takes a catalog key and hands back the client for the endpoint that entry
/// declares, paired with that entry's own routing slug, or `None` when the model
/// has no route of its own. `None` **skips**. It must never degrade to the
/// session's client the way image-describe and session-title do: those name a
/// slug the session endpoint is known to serve, whereas here the model was
/// chosen from a different entry, and a client carries one endpoint and one
/// credential.
pub(crate) async fn serve_distill_jobs<R, F>(
    mut jobs: mpsc::UnboundedReceiver<DistillJob>,
    session_id: String,
    resolve: R,
) where
    R: Fn(String) -> F,
    F: std::future::Future<Output = Option<(xai_grok_sampler::SamplingClient, String)>>,
{
    while let Some(job) = jobs.recv().await {
        let route = resolve(job.model_key.clone()).await;
        let result = run_distill_job(
            route,
            &job.model_key,
            &job.system_prompt,
            &job.user_prompt,
            &session_id,
        )
        .await;
        if let Err(error) = &result {
            tracing::debug!(
                model = %job.model_key,
                %error,
                "web_fetch distillation skipped; the raw page is returned"
            );
        }
        let _ = job.respond_to.send(result);
    }
}

/// Run one distillation inference against an already-resolved route.
///
/// `model` is the routing slug of the entry `resolve` landed on, and the client
/// is that same entry's endpoint and credential. They are one resolution of one
/// key, so there is nothing left here to cross-check: the key was carried whole
/// from the pin, and the slug was derived from it rather than the other way
/// round.
async fn run_distill_job(
    route: Option<(xai_grok_sampler::SamplingClient, String)>,
    model_key: &str,
    system_prompt: &str,
    user_prompt: &str,
    session_id: &str,
) -> Result<String, String> {
    let Some((client, model)) = route else {
        return Err(format!(
            "aux model {model_key:?} has no route of its own; it must not ride the session client"
        ));
    };
    let request = ConversationRequest {
        items: vec![
            ConversationItem::system(system_prompt.to_owned()),
            ConversationItem::user(user_prompt.to_owned()),
        ],
        tools: vec![],
        model: Some(model),
        x_grok_conv_id: Some(format!("webfetchdistill-{}", uuid::Uuid::new_v4())),
        x_grok_req_id: Some(format!("xai-webfetchdistill-{}", uuid::Uuid::new_v4())),
        x_grok_session_id: Some(session_id.to_owned()),
        x_grok_agent_id: Some(xai_grok_telemetry::id::agent_id()),
        ..Default::default()
    };
    let response = client
        .conversation_collect(request)
        .await
        .map_err(|e| e.to_string())?;
    Ok(response.assistant_text())
}

impl SessionActor {
    /// Register the `web_fetch` distiller on the session's current tool bridge.
    ///
    /// Called at spawn and again after an agent rebuild, which installs a fresh
    /// `Resources`. Each call mints its own channel and service task; the task
    /// from a previous call exits once the resource holding its sender is
    /// dropped with the old bridge.
    ///
    /// The task holds a `Weak`, not an `Arc`: its sender lives in a resource the
    /// session ultimately owns, so an owning handle would be a cycle. A session
    /// already torn down resolves no route, which skips.
    pub(crate) async fn wire_web_fetch_distiller(self: &Arc<Self>) {
        let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();
        let weak = Arc::downgrade(self);
        let session_id = self.session_info.id.to_string();
        tokio::task::spawn_local(async move {
            serve_distill_jobs(jobs_rx, session_id, move |key| {
                let weak = weak.clone();
                async move { weak.upgrade()?.resolve_aux_sampler_client(&key).await }
            })
            .await;
            tracing::debug!("web_fetch distiller task exiting (channel closed)");
        });
        let distiller = ChannelWebContentDistiller::new(self.models_manager.clone(), jobs_tx);
        let bridge = self.agent.borrow().tool_bridge().clone();
        bridge
            .update_resource(WebContentDistillerResource(Arc::new(distiller)))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::{
        Config, EndpointsConfig, ModelEntry, resolve_aux_model_sampling_config, resolve_model_list,
        stamp_session_local_sampler_fields,
    };
    use crate::sampling::SamplerConfig;
    use indexmap::IndexMap;
    use xai_grok_config_types::{ProviderConfig, ProviderFormat};
    use xai_grok_test_support::MockInferenceServer;

    /// The endpoint the live session talks to. Never first-party in these
    /// tests, so the resolver's first-party fallback tier cannot mask a result.
    const SESSION_MODEL: &str = "session-model";
    const AUX_MODEL: &str = "aux-model";
    /// One slug, two providers. The case a bare slug cannot disambiguate.
    const SHARED_MODEL: &str = "shared-model";

    /// A `[[provider]]` serving one model. `api_key: None` with an
    /// `auth_account` is the plugin-minted-bearer shape: an entry with no
    /// credential of its own, which is what `resolve_credentials` falls through
    /// on and the aux resolver must refuse to fill in.
    fn provider(
        id: &str,
        model: &str,
        base_url: &str,
        api_key: Option<&str>,
        auth_account: Option<&str>,
    ) -> ProviderConfig {
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
            auth_account: auth_account.map(str::to_owned),
            reasoning_efforts: Vec::new(),
            reasoning_effort: None,
            supports_reasoning_effort: false,
            thinking: None,
            max_concurrent: None,
        }
    }

    fn catalog_from(providers: Vec<ProviderConfig>) -> IndexMap<String, ModelEntry> {
        let cfg = Config {
            providers,
            ..Config::default()
        };
        resolve_model_list(&cfg, None)
    }

    fn models_manager(
        catalog: IndexMap<String, ModelEntry>,
        current_model_id: &str,
    ) -> ModelsManager {
        let tmp = std::env::temp_dir().join("grok-test-web-fetch-distill");
        let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new(
            &tmp,
            crate::auth::GrokComConfig::default(),
        ));
        ModelsManager::new(
            None,
            catalog,
            agent_client_protocol::ModelId::new(current_model_id),
            auth_manager,
            Config::default(),
        )
    }

    /// The live session's own sampler config, built from its catalog entry the
    /// way the session itself builds it.
    fn session_config(catalog: &IndexMap<String, ModelEntry>, key: &str) -> SamplerConfig {
        let entry = catalog.get(key).expect("session entry synthesized");
        crate::agent::config::sampling_config_for_model(
            entry,
            crate::agent::config::resolve_credentials(entry, None),
            None,
            None,
            None,
            None,
        )
    }

    /// Stand-in for [`SessionActor::resolve_aux_sampler_client`]: the same
    /// resolve, stamp and client build, over an explicit catalog and session
    /// config instead of the live actor's.
    fn resolve_route(
        model_key: &str,
        catalog: &IndexMap<String, ModelEntry>,
        session: &SamplerConfig,
        session_key: Option<&str>,
    ) -> Option<(xai_grok_sampler::SamplingClient, String)> {
        let mut cfg = resolve_aux_model_sampling_config(
            model_key,
            catalog,
            &EndpointsConfig::default(),
            &session.base_url,
            session_key,
            false,
            None,
            None,
        )?;
        stamp_session_local_sampler_fields(&mut cfg, session, None, Some(1));
        let model = cfg.model.clone();
        Some((xai_grok_sampler::SamplingClient::new(cfg).ok()?, model))
    }

    /// Drive one `infer` through the real channel, service loop and resolver.
    async fn distill_once(
        models: ModelsManager,
        catalog: IndexMap<String, ModelEntry>,
        session: SamplerConfig,
        model_key: &str,
    ) -> Result<String, String> {
        let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();
        let served = serve_distill_jobs(jobs_rx, "test-session".to_owned(), move |model_key| {
            let catalog = catalog.clone();
            let session = session.clone();
            async move {
                // A signed-in session: the bearer that must not travel.
                resolve_route(&model_key, &catalog, &session, Some("session-jwt"))
            }
        });
        let distiller = ChannelWebContentDistiller::new(models, jobs_tx);
        let call = async move {
            let result = distiller.infer(model_key, "system", "user").await;
            // The loop runs until its last sender goes; this is that sender.
            drop(distiller);
            result
        };
        let (result, ()) = tokio::join!(call, served);
        result.map_err(|e| e.to_string())
    }

    /// The guard this wiring exists to keep. A pin whose catalog entry sits on a
    /// third-party endpoint and carries no credential of its own resolves to no
    /// route at all; distillation must skip, not reach for the session's client
    /// or the session bearer that client carries.
    #[tokio::test]
    async fn aux_pin_without_its_own_credential_skips_instead_of_borrowing_the_session_bearer() {
        let session_server = MockInferenceServer::start().await.unwrap();
        let aux_server = MockInferenceServer::start().await.unwrap();
        let catalog = catalog_from(vec![
            provider(
                "session-provider",
                SESSION_MODEL,
                &session_server.url(),
                Some("session-provider-key"),
                None,
            ),
            // No api_key / env_key / auth_provider: only an account name a
            // credential plugin would mint a bearer for.
            provider(
                "acme",
                AUX_MODEL,
                &aux_server.url(),
                None,
                Some("acme-account"),
            ),
        ]);
        let session = session_config(&catalog, "session-provider/session-model");
        let models = models_manager(catalog.clone(), "session-provider/session-model");
        let distiller =
            ChannelWebContentDistiller::new(models.clone(), mpsc::unbounded_channel().0);

        let pin_key = distiller
            .catalog_key("acme/aux-model")
            .expect("the pin is in the catalog — which is why it gets this far");
        assert_eq!(
            pin_key, "acme/aux-model",
            "the pin's qualification is carried, not collapsed to the slug"
        );

        let err = distill_once(models, catalog, session, &pin_key)
            .await
            .expect_err("a route-less aux model must not be distilled with");
        assert!(err.contains("no route of its own"), "got: {err}");
        assert!(
            aux_server.requests().is_empty(),
            "the third-party endpoint must not be sent a request it has no credential for"
        );
        assert!(
            session_server.requests().is_empty(),
            "and the session's own client must never be borrowed for another provider's slug"
        );
    }

    /// The positive control for the same wiring: a pin that does carry its own
    /// credential runs, on its own endpoint, under its own key, naming the bare
    /// slug — and the session's provider still sees nothing.
    #[tokio::test]
    async fn aux_pin_with_its_own_credential_runs_on_its_own_endpoint() {
        let session_server = MockInferenceServer::start().await.unwrap();
        let aux_server = MockInferenceServer::start().await.unwrap();
        aux_server.set_response("the rate limit is 60 rpm");
        let catalog = catalog_from(vec![
            provider(
                "session-provider",
                SESSION_MODEL,
                &session_server.url(),
                Some("session-provider-key"),
                None,
            ),
            provider("acme", AUX_MODEL, &aux_server.url(), Some("acme-key"), None),
        ]);
        let session = session_config(&catalog, "session-provider/session-model");
        let models = models_manager(catalog.clone(), "session-provider/session-model");
        let distiller =
            ChannelWebContentDistiller::new(models.clone(), mpsc::unbounded_channel().0);

        let pin_key = distiller.catalog_key("acme/aux-model").expect("in catalog");
        let answer = distill_once(models, catalog, session, &pin_key)
            .await
            .expect("a pin with its own credential resolves and runs");
        assert!(answer.contains("60 rpm"), "got: {answer}");

        let logged = aux_server
            .requests()
            .into_iter()
            .find(|e| e.body.is_some())
            .expect("the pinned provider received the request");
        assert_eq!(
            logged.body.as_ref().and_then(|b| b["model"].as_str()),
            Some(AUX_MODEL),
            "the bare slug goes on the wire, never the catalog key"
        );
        assert_eq!(
            logged.authorization.as_deref(),
            Some("Bearer acme-key"),
            "on its own credential, not the session's"
        );
        assert!(
            session_server.requests().is_empty(),
            "the session's provider must never be named another provider's model"
        );
    }

    /// Two providers serving one slug: the qualified pin names the first, and
    /// the first is where the request must land.
    ///
    /// The bare slug cannot express this. `config::find_by_slug` answers a bare
    /// `shared-model` with the LAST declared entry, so a seam that reduces the
    /// pin to its slug and lets the sampler resolver re-derive the entry hands
    /// the request to `b` — `b`'s endpoint, `b`'s credential — after the user
    /// spelled `a`. The qualified key is the only disambiguator there is, so it
    /// is the key, not the slug, that has to survive to the resolver.
    #[tokio::test]
    async fn a_pin_qualified_to_one_of_two_same_slug_providers_routes_to_that_one() {
        let session_server = MockInferenceServer::start().await.unwrap();
        let a_server = MockInferenceServer::start().await.unwrap();
        let b_server = MockInferenceServer::start().await.unwrap();
        a_server.set_response("answered by a");
        b_server.set_response("answered by b");
        let catalog = catalog_from(vec![
            provider(
                "session-provider",
                SESSION_MODEL,
                &session_server.url(),
                Some("session-provider-key"),
                None,
            ),
            provider("a", SHARED_MODEL, &a_server.url(), Some("a-key"), None),
            // Declared after `a`, so a bare-slug scan lands here.
            provider("b", SHARED_MODEL, &b_server.url(), Some("b-key"), None),
        ]);
        let session = session_config(&catalog, "session-provider/session-model");
        let models = models_manager(catalog.clone(), "session-provider/session-model");
        let distiller =
            ChannelWebContentDistiller::new(models.clone(), mpsc::unbounded_channel().0);

        let pin = distiller
            .catalog_key("a/shared-model")
            .expect("the qualified pin is a catalog key");
        let answer = distill_once(models, catalog, session, &pin)
            .await
            .expect("the pinned provider carries its own credential");
        assert_eq!(answer, "answered by a", "the pin named `a`, not `b`");

        let logged = a_server
            .requests()
            .into_iter()
            .find(|e| e.body.is_some())
            .expect("the pinned provider received the request");
        assert_eq!(
            logged.body.as_ref().and_then(|b| b["model"].as_str()),
            Some(SHARED_MODEL),
            "the bare slug still goes on the wire — a key is not a wire model"
        );
        assert_eq!(
            logged.authorization.as_deref(),
            Some("Bearer a-key"),
            "on the pinned provider's own credential"
        );
        assert!(
            b_server.requests().is_empty(),
            "the other provider serving the same slug must never see this request"
        );
    }

    /// Unpinned distillation stays on the provider the session already talks to.
    /// The alternative — a compiled-in first-party slug — is the fabricated
    /// first-party request the seam's third rule forbids.
    #[test]
    fn the_unpinned_default_is_the_session_model_not_a_first_party_slug() {
        let catalog = catalog_from(vec![provider(
            "acme",
            SESSION_MODEL,
            "https://acme.example/v1",
            Some("acme-key"),
            None,
        )]);
        let distiller = ChannelWebContentDistiller::new(
            models_manager(catalog, "acme/session-model"),
            mpsc::unbounded_channel().0,
        );
        assert_eq!(
            distiller.default_catalog_key().as_deref(),
            Some("acme/session-model"),
            "the session's own model, as the key that names its entry"
        );
    }

    /// Nothing placeable means nothing to send: skip rather than guess.
    #[test]
    fn an_id_the_catalog_cannot_place_yields_no_key_at_all() {
        let distiller = ChannelWebContentDistiller::new(
            models_manager(IndexMap::new(), "not-in-any-catalog"),
            mpsc::unbounded_channel().0,
        );
        assert_eq!(distiller.catalog_key("acme/aux-model"), None);
        assert_eq!(distiller.default_catalog_key(), None);
    }

    /// A dropped service task must fail open rather than hang the fetch.
    #[tokio::test]
    async fn a_gone_session_task_fails_open() {
        let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();
        drop(jobs_rx);
        let distiller =
            ChannelWebContentDistiller::new(models_manager(IndexMap::new(), "m"), jobs_tx);
        assert!(distiller.infer("m", "system", "user").await.is_err());
    }
}
