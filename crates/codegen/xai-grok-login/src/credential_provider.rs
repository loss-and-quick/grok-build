use std::sync::Arc;

use arc_swap::ArcSwap;
use reqwest::RequestBuilder;
use xai_grok_auth::{AuthCredentialProvider, CredentialSnapshot, HttpAuth};

use crate::AuthManager;
use crate::backend::{ActiveAuthBackend, AuthBackend};
use crate::credential_seam::{PluginCredential, PluginCredentialSeam};
use crate::grok_auth_credentials::GrokAuthCredentials;

/// `api_key.id` for the active credential: hash the stable API key, never the OIDC bearer (which rotates).
/// `None` for non-API-key auth.
fn api_key_id_for(auth: Option<&crate::GrokAuth>) -> Option<String> {
    auth.filter(|a| matches!(a.auth_mode, crate::AuthMode::ApiKey))
        .map(|a| xai_grok_telemetry::config::deployment_id_from_key(&a.key))
}

/// Sampler [`BearerResolver`](xai_grok_sampler::BearerResolver) over a live [`AuthManager`].
/// Wire-valid only: it never stamps a hard-expired access token (the client auth contract).
/// Shared by the session sampler and subagent configs so the contract can't drift between them.
pub struct WireValidBearerResolver(pub Arc<AuthManager>);

impl WireValidBearerResolver {
    /// The one constructor both the session sampler and subagent configs use, so the wire-valid contract cannot drift between the call sites.
    pub fn shared(auth_manager: Arc<AuthManager>) -> xai_grok_sampler::SharedBearerResolver {
        Arc::new(Self(auth_manager))
    }
}

impl std::fmt::Debug for WireValidBearerResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireValidBearerResolver").finish()
    }
}

/// Ceiling for the pre-send refresh wait.
/// Long enough for one external-binary run (7 s) or a first-party token exchange; the exchange keeps going in the background if it overruns and hot-swaps the token when it lands.
const PRE_SEND_REFRESH_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Time the send itself needs once the wait ends, reserved out of a still wire-valid bearer's remaining life.
const PRE_SEND_STAMP_MARGIN: std::time::Duration = std::time::Duration::from_millis(500);

/// How long `prepare_for_send` may wait on a refresh: the wait must end before the cached bearer dies, or a slow mint turns a request that could have carried the old token into one that carries none.
fn pre_send_refresh_budget(remaining_wire_life: std::time::Duration) -> std::time::Duration {
    remaining_wire_life
        .saturating_sub(PRE_SEND_STAMP_MARGIN)
        .min(PRE_SEND_REFRESH_BUDGET)
}

impl xai_grok_sampler::BearerResolver for WireValidBearerResolver {
    fn current_bearer(&self) -> Option<String> {
        // The samplers attach this resolver whenever the endpoint is a first-party xAI URL.
        // A session minted by another authority would send its token there on every chat call.
        if !ActiveAuthBackend::default().is_xai_authority() {
            return None;
        }
        self.0.current_wire_valid().map(|a| a.key)
    }

    /// Closes the pre-flight→send gap: the turn's pre-flight ran `auth()`, but the request can leave much later (a sampling-permit wait, a rate-limit sleep, a resubmit that skips the pre-flight).
    /// If the cached bearer is wire-valid now but would not survive the send, refresh here instead of letting `current_bearer` strip it and send the request with no credential.
    /// Only that race is handled here. With no wire-valid bearer at all the pre-flight has already made its refresh attempt and the 401 arm (recovery, parking) owns the outcome; a refresh per send would let every parked, deliberately credential-less resubmit drive the escalation budget.
    fn prepare_for_send(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if !ActiveAuthBackend::default().is_xai_authority() {
                return;
            }
            let Some(remaining) = self.0.remaining_wire_life() else {
                return;
            };
            if self.0.has_sendable_token() {
                return;
            }
            let budget = pre_send_refresh_budget(remaining);
            xai_grok_telemetry::unified_log::warn(
                "auth: pre-send refresh, cached bearer would not outlive the send",
                None,
                Some(serde_json::json!({ "budget_ms": budget.as_millis() as u64 })),
            );
            if budget.is_zero() {
                // Less than the stamp margin left: no time to mint, and `current_bearer` still carries the bearer
                return;
            }
            let _ = self.0.silent_refresh_within(budget).await;
        })
    }
}

/// Resolves a snapshot's `deployment_id` from an enterprise deployment key.
/// Injected at construction so the provider stays off shell config; callers with a
/// deployment key pass the managed-config deployment-id resolver.
pub type DeploymentIdResolver =
    std::sync::Arc<dyn Fn(Option<&str>) -> Option<String> + Send + Sync>;

/// Production impl: wraps the live `AuthManager`.
/// 401 recovery delegates to `AuthManager::unauthorized_recovery`.
///
/// When a [`PluginCredentialSeam`] is injected (session-aware construction), a
/// plugin-supplied credential takes precedence over the built-in resolution:
/// [`resolve_credential`](Self::resolve_credential) caches it, and every wire
/// path (`apply`/`snapshot`/`needs_token_auth_header`) prefers the cache. The
/// built-in `AuthManager` path stays fully available and is used whenever the
/// cache is empty — the plugin channel is fail-open, never a hard dependency.
pub struct ShellAuthCredentialProvider {
    auth_manager: Arc<AuthManager>,
    static_credentials: GrokAuthCredentials,
    deployment_id_resolver: DeploymentIdResolver,
    /// Injected plugin seam; `None` outside session-aware construction.
    credential_seam: Option<Arc<dyn PluginCredentialSeam>>,
    /// The most recent plugin-supplied credential, preferred on the wire while
    /// present. Empty until a resolve/refresh/oauth seam call succeeds.
    plugin_credential: ArcSwap<Option<PluginCredential>>,
    /// The outbound endpoint the seam is resolving/refreshing *for*. Stashed by
    /// [`resolve_credential`](Self::resolve_credential) (whose caller knows the
    /// target) so the argument-less [`refresh_after_unauthorized`] can scope its
    /// `refresh_credential` dispatch to the same endpoint. Empty until the first
    /// resolve — a refresh with an empty target lets a scoping plugin pass
    /// through (fail-safe), never leaking a credential to an unknown provider.
    seam_base_url: ArcSwap<String>,
    /// The `auth_account` selector the seam is resolving/refreshing *for* —
    /// which of the plugin's accounts for this provider the core wants. Stashed
    /// alongside [`Self::seam_base_url`] for the same reason: the argument-less
    /// [`refresh_after_unauthorized`](Self::refresh_after_unauthorized) must ask
    /// for the same account the resolve did. `None` = the plugin's default
    /// account, which is the pre-account behaviour.
    seam_account: ArcSwap<Option<String>>,
}

impl ShellAuthCredentialProvider {
    /// Constructs without a deployment-id resolver; the snapshot omits `deployment_id`.
    /// Only correct for callers that never set a deployment key; deployment-key callers
    /// must use [`Self::with_deployment_id_resolver`].
    pub fn new(
        auth_manager: Arc<AuthManager>,
        deployment_key: Option<String>,
        alpha_test_key: Option<String>,
    ) -> Self {
        Self::with_deployment_id_resolver(
            auth_manager,
            deployment_key,
            alpha_test_key,
            std::sync::Arc::new(|_| None),
        )
    }

    pub fn with_deployment_id_resolver(
        auth_manager: Arc<AuthManager>,
        deployment_key: Option<String>,
        alpha_test_key: Option<String>,
        deployment_id_resolver: DeploymentIdResolver,
    ) -> Self {
        let mut static_credentials = GrokAuthCredentials::new(None);
        static_credentials.deployment_key = deployment_key;
        static_credentials.alpha_test_key = alpha_test_key;
        Self {
            auth_manager,
            static_credentials,
            deployment_id_resolver,
            credential_seam: None,
            plugin_credential: ArcSwap::from_pointee(None),
            seam_base_url: ArcSwap::from_pointee(String::new()),
            seam_account: ArcSwap::from_pointee(None),
        }
    }

    /// Inject the plugin credential seam. Session-aware callers build a
    /// [`HookCredentialSeam`](crate::credential_seam::HookCredentialSeam)
    /// from the session's hook registry + plugin invoker and pass it here so a
    /// plugin can supply/refresh/authorize the outbound bearer.
    pub fn with_credential_seam(mut self, seam: Arc<dyn PluginCredentialSeam>) -> Self {
        self.credential_seam = Some(seam);
        self
    }

    /// The cached plugin credential, if one is present and unexpired.
    fn active_plugin_credential(&self) -> Option<Arc<PluginCredential>> {
        let guard = self.plugin_credential.load();
        let cred = guard.as_ref().as_ref()?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        cred.is_unexpired(now_ms).then(|| {
            // Clone into an Arc so the caller holds it past the guard's lifetime.
            Arc::new(cred.clone())
        })
    }

    /// Ask the plugin seam to resolve a credential before the built-in
    /// resolution runs, caching it so subsequent wire paths prefer it. Returns
    /// `true` when a plugin credential was obtained. No-op (returns `false`)
    /// when no seam is injected or the plugin passes through — the built-in
    /// resolution then applies. Fired at session bootstrap and whenever no
    /// usable credential is cached. `base_url` is the outbound endpoint this
    /// credential is resolved *for*: it rides the `resolve_credential` payload
    /// so a plugin can scope its reply to the target provider, and is stashed so
    /// a later [`refresh_after_unauthorized`] scopes its refresh identically.
    /// `account` is the configured `auth_account` selector, riding the payload
    /// as `ownerHint` so one plugin can hold several accounts for the same
    /// provider; `None` asks for the plugin's default account (the behaviour
    /// before the selector existed) and is likewise stashed for the refresh.
    pub async fn resolve_credential(
        &self,
        reason: &str,
        base_url: &str,
        account: Option<&str>,
    ) -> bool {
        self.seam_base_url.store(Arc::new(base_url.to_string()));
        self.seam_account
            .store(Arc::new(account.map(str::to_string)));
        let Some(seam) = self.credential_seam.as_ref() else {
            return false;
        };
        match seam.resolve(reason, base_url, account).await {
            Some(cred) => {
                self.plugin_credential.store(Arc::new(Some(cred)));
                true
            }
            None => false,
        }
    }

    /// Drive the plugin's interactive authorization flow, caching the final
    /// credential. Returns `true` when the flow produced one. Triggered on an
    /// explicit sign-in or when no usable credential exists. `target_plugin`,
    /// when `Some(name)`, restricts the flow to that single plugin's handler
    /// (the `/login` provider the user selected); `None` consults all
    /// subscribers. `account` names which of that plugin's accounts to
    /// authorize (see [`resolve_credential`](Self::resolve_credential)).
    pub async fn start_oauth_flow(
        &self,
        reason: &str,
        target_plugin: Option<&str>,
        account: Option<&str>,
    ) -> bool {
        let Some(seam) = self.credential_seam.as_ref() else {
            return false;
        };
        match seam.start_oauth_flow(reason, target_plugin, account).await {
            Some(cred) => {
                self.plugin_credential.store(Arc::new(Some(cred)));
                true
            }
            None => false,
        }
    }
}

// Manual Debug impl that redacts the token, like RefreshableSpanExporter in otel_layer.rs
impl std::fmt::Debug for ShellAuthCredentialProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellAuthCredentialProvider")
            .field("auth_manager", &"<configured>")
            .field("credential_seam", &self.credential_seam.is_some())
            .field(
                "has_plugin_credential",
                &self.plugin_credential.load().is_some(),
            )
            .finish()
    }
}

impl HttpAuth for ShellAuthCredentialProvider {
    fn apply(&self, builder: RequestBuilder, base_url: &str) -> RequestBuilder {
        // A plugin-supplied credential wins over the built-in resolution: send
        // it as a user token (Bearer + token-auth marker) or a bare Bearer,
        // per its `needs_token_auth_header` flag.
        if let Some(cred) = self.active_plugin_credential() {
            let mut creds = GrokAuthCredentials::new(None);
            if cred.needs_token_auth_header {
                creds.user_token = Some(cred.token.clone());
            } else {
                creds.deployment_key = Some(cred.token.clone());
            }
            creds.alpha_test_key = self.static_credentials.alpha_test_key.clone();
            return creds.apply(builder, base_url);
        }
        // This trait is sync, so no refresh happens here: the proactive task keeps the cache hot and `refresh_after_unauthorized()` handles 401s
        // Wire-valid only: never stamp a hard-expired access token
        let mut creds = self.static_credentials.clone();
        // A session minted elsewhere must not reach an xAI host.
        // A deployment key is configured locally rather than minted, so it still applies.
        if creds.deployment_key.is_none()
            && ActiveAuthBackend::default().is_xai_authority()
            && let Some(auth) = self.auth_manager.current_wire_valid()
        {
            creds.user_token = Some(auth.key);
        }
        creds.apply(builder, base_url)
    }
}

#[async_trait::async_trait]
impl AuthCredentialProvider for ShellAuthCredentialProvider {
    fn snapshot(&self) -> CredentialSnapshot {
        if let Some(cred) = self.active_plugin_credential() {
            return CredentialSnapshot {
                token: Some(cred.token.clone()),
                user_id: cred.owner_id.clone(),
                ..Default::default()
            };
        }
        // The token must match what `HttpAuth::apply` puts on the wire (wire-valid only)
        // Identity fields may still come from a soft-expired cache
        if let Some(ref dk) = self.static_credentials.deployment_key {
            return CredentialSnapshot {
                token: Some(dk.clone()),
                deployment_id: (self.deployment_id_resolver)(Some(dk)),
                ..Default::default()
            };
        }
        let identity = self.auth_manager.current_or_expired();
        let user_id = identity.as_ref().map(|a| a.user_id.clone());
        let team_id = identity.as_ref().and_then(|a| a.team_id.clone());
        let organization_id = identity.as_ref().and_then(|a| a.organization_id.clone());
        let api_key_id = api_key_id_for(identity.as_ref());
        let token = ActiveAuthBackend::default()
            .is_xai_authority()
            .then(|| self.auth_manager.current_wire_valid().map(|a| a.key))
            .flatten();
        CredentialSnapshot {
            token,
            user_id,
            team_id,
            deployment_id: None,
            api_key_id,
            organization_id,
        }
    }

    async fn refresh_after_unauthorized(&self) -> bool {
        // The plugin refresh seam runs first: a plugin credential means the
        // built-in `AuthManager` may hold nothing at all, so its recovery would
        // no-op. On passthrough/absence, fall back to the built-in refresh,
        // which stays available independently of the plugin channel.
        if let Some(seam) = self.credential_seam.as_ref() {
            let owner = self
                .plugin_credential
                .load()
                .as_ref()
                .as_ref()
                .and_then(|c| c.owner_id.clone());
            let base_url = self.seam_base_url.load();
            // The account the *resolve* asked for, not the owner of whatever is
            // cached: those are different questions, and only the former stays
            // true when nothing is cached yet.
            let account = self.seam_account.load();
            if let Some(cred) = seam
                .refresh(
                    "unauthorized",
                    owner.as_deref(),
                    base_url.as_str(),
                    account.as_deref(),
                )
                .await
            {
                self.plugin_credential.store(Arc::new(Some(cred)));
                return true;
            }
            // A cached plugin credential the plugin declined to refresh: the
            // built-in `AuthManager` path is not its owner, so don't claim a
            // recovery it can't perform.
            if self.plugin_credential.load().is_some() {
                return false;
            }
        }
        if self.static_credentials.deployment_key.is_some() {
            return false;
        }
        self.auth_manager
            .try_recover_unauthorized(crate::recovery::RecoverySource::Background)
            .await
    }

    fn needs_token_auth_header(&self) -> bool {
        if let Some(cred) = self.active_plugin_credential() {
            return cred.needs_token_auth_header;
        }
        self.static_credentials.deployment_key.is_none()
    }
}

/// Resolves the embedding credentials for `embed_base_url`, attaching the xAI session credential only to xAI-operated endpoints over `https`.
pub fn embedding_session_credentials(
    embed_base_url: &str,
    auth_manager: Option<&Arc<AuthManager>>,
    api_key_provider: Option<xai_grok_tools::types::SharedApiKeyProvider>,
) -> xai_grok_memory::EndpointScopedCredentials {
    let auth_credentials = auth_manager.map(|am| {
        Arc::new(ShellAuthCredentialProvider::new(am.clone(), None, None))
            as Arc<dyn AuthCredentialProvider>
    });
    xai_grok_memory::EndpointScopedCredentials::for_endpoint(
        embed_base_url,
        xai_grok_shell_base::util::is_xai_api_bearer_url,
        auth_credentials,
        api_key_provider,
    )
}

/// Lets `StorageClient` (in xai-file-utils) emit shell's 401-attribution event without xai-file-utils depending on shell.
/// Holds the live `AuthManager` so attribution events carry the correct user_id.
pub struct StorageClientAttributionBridge {
    auth_manager: Arc<AuthManager>,
    session_id: Option<String>,
}

impl std::fmt::Debug for StorageClientAttributionBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageClientAttributionBridge")
            .finish_non_exhaustive()
    }
}

impl StorageClientAttributionBridge {
    pub fn new(auth_manager: Arc<AuthManager>, session_id: Option<String>) -> Self {
        Self {
            auth_manager,
            session_id,
        }
    }
}

impl xai_file_utils::storage_client::Auth401AttributionCallback for StorageClientAttributionBridge {
    fn record_401(&self, operation: &str, sent_bearer_prefix: Option<&str>) {
        crate::attribution::record_consumer_401(
            self.auth_manager.as_ref(),
            self.session_id.as_deref(),
            crate::attribution::ConsumerKind::StorageClient,
            operation,
            sent_bearer_prefix,
        );
    }
}

/// Credential provider for the OTel layer's `RefreshableSpanExporter`. Starts with a bootstrap `AuthManager` (disk-read-only, no refresher).
/// [`Self::set_live`] upgrades it to the agent's live `Arc<AuthManager>` once the agent is initialized.
/// After upgrade: `snapshot()` reads from the live manager's in-memory cache (kept hot by the proactive refresh task) instead of re-reading disk. `refresh_after_unauthorized()` routes through `unauthorized_recovery` for active OIDC or external-binary refresh. Before upgrade, the bootstrap manager only reads from disk.
pub struct OtelAuthCredentialProvider {
    /// Bootstrap manager used before the live one is available.
    bootstrap: Arc<AuthManager>,
    /// Swapped to the agent's live `AuthManager` via `set_live()`.
    /// `None` means still in bootstrap mode.
    live: arc_swap::ArcSwap<Option<Arc<AuthManager>>>,
    /// Enterprise deployment key. Takes precedence over OIDC in `snapshot_inner`.
    deployment_key: arc_swap::ArcSwap<Option<String>>,
    /// Resolves `deployment_id` from the deployment key in `snapshot_inner`.
    deployment_id_resolver: DeploymentIdResolver,
}

impl OtelAuthCredentialProvider {
    /// Constructs without a deployment-id resolver; the snapshot omits `deployment_id`.
    /// Use [`Self::with_deployment_id_resolver`] whenever a deployment key may be set.
    #[cfg(test)]
    fn new(bootstrap: Arc<AuthManager>) -> Self {
        Self::with_deployment_id_resolver(bootstrap, std::sync::Arc::new(|_| None))
    }

    fn with_deployment_id_resolver(
        bootstrap: Arc<AuthManager>,
        deployment_id_resolver: DeploymentIdResolver,
    ) -> Self {
        Self {
            bootstrap,
            live: arc_swap::ArcSwap::from_pointee(None),
            deployment_key: arc_swap::ArcSwap::from_pointee(None),
            deployment_id_resolver,
        }
    }

    /// Upgrade to the agent's live `AuthManager`.
    /// After this call, `snapshot()` reads from the live manager and `refresh_after_unauthorized()` drives the full recovery state machine.
    pub fn set_live(&self, auth_manager: Arc<AuthManager>) {
        self.live.store(Arc::new(Some(auth_manager)));
    }

    pub fn set_deployment_key(&self, key: String) {
        self.deployment_key.store(Arc::new(Some(key)));
    }

    /// Email for the external stream: OIDC/gateway only, never API-key,
    /// deployment-key, git, or blank. Identity, not a content gate.
    fn oauth_gateway_email(&self) -> Option<String> {
        if self.deployment_key.load().is_some() {
            return None;
        }
        let (am, _) = self.load_state();
        let auth = am.current_or_expired()?;
        oauth_gateway_email_from_auth(&auth)
    }

    /// Loads `live` once, returning the live manager when set, else the bootstrap.
    fn load_state(&self) -> (Arc<AuthManager>, bool) {
        let guard = self.live.load();
        match guard.as_ref() {
            Some(am) => (am.clone(), true),
            None => (self.bootstrap.clone(), false),
        }
    }
}

impl std::fmt::Debug for OtelAuthCredentialProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (_, is_live) = self.load_state();
        f.debug_struct("OtelAuthCredentialProvider")
            .field("mode", &if is_live { "live" } else { "bootstrap" })
            .finish()
    }
}

impl HttpAuth for OtelAuthCredentialProvider {
    fn apply(&self, builder: RequestBuilder, base_url: &str) -> RequestBuilder {
        // The collector is an xAI host, so a session token from another authority must not be sent to it
        // A deployment key is configured locally rather than minted, so it still applies.
        if self.deployment_key.load().is_none() && !ActiveAuthBackend::default().is_xai_authority()
        {
            return builder;
        }
        let snapshot = self.snapshot_inner();
        let mut creds = GrokAuthCredentials::new(None);
        if self.deployment_key.load().is_some() {
            creds.deployment_key = snapshot.token;
        } else {
            creds.user_token = snapshot.token;
        }
        creds.apply(builder, base_url)
    }
}

impl OtelAuthCredentialProvider {
    fn snapshot_inner(&self) -> CredentialSnapshot {
        if let Some(ref dk) = **self.deployment_key.load() {
            return CredentialSnapshot {
                token: Some(dk.clone()),
                deployment_id: (self.deployment_id_resolver)(Some(dk)),
                ..Default::default()
            };
        }

        let (am, is_live) = self.load_state();
        if !is_live {
            am.force_reload_from_disk();
        }
        let auth = am.current_or_expired();
        let user_id = auth.as_ref().map(|a| a.user_id.clone());
        let team_id = auth.as_ref().and_then(|a| a.team_id.clone());
        let organization_id = auth.as_ref().and_then(|a| a.organization_id.clone());
        let api_key_id = api_key_id_for(auth.as_ref());
        let token = auth.map(|a| a.key);
        CredentialSnapshot {
            token,
            user_id,
            team_id,
            deployment_id: None,
            api_key_id,
            organization_id,
        }
    }
}

#[async_trait::async_trait]
impl AuthCredentialProvider for OtelAuthCredentialProvider {
    fn snapshot(&self) -> CredentialSnapshot {
        // The exporter reads the token from here rather than through `apply`, so both need the guard.
        // A deployment key is configured locally, so it still applies.
        if self.deployment_key.load().is_none() && !ActiveAuthBackend::default().is_xai_authority()
        {
            return CredentialSnapshot::default();
        }
        self.snapshot_inner()
    }

    fn has_usable_credential(&self) -> bool {
        if self.deployment_key.load().is_some() {
            return true;
        }
        if !ActiveAuthBackend::default().is_xai_authority() {
            return false;
        }
        self.load_state().0.has_usable_token()
    }

    async fn refresh_after_unauthorized(&self) -> bool {
        let (am, is_live) = self.load_state();
        if !is_live {
            return false;
        }
        am.try_recover_unauthorized(crate::recovery::RecoverySource::Background)
            .await
    }

    fn needs_token_auth_header(&self) -> bool {
        self.deployment_key.load().is_none()
    }
}

/// Process-wide OTel credential provider handle. This is one of two acceptable process-wide statics in this crate (the other is `TRACER_PROVIDER` in `otel_layer.rs`).
/// It is set once at tracing init, before any `AuthManager` exists, and holds a bootstrap-mode provider. The `ArcSwap` inside the provider handles the runtime auth state swap; the `OnceLock` itself is never re-written.
/// A static beats passing a handle: `build_default_otel_layer_config` is called from 15+ `init_tracing*` sites across 3 binaries. Passing a handle from all of them to the agent init site, where the live `AuthManager` is constructed, would touch ~20 files.
static OTEL_PROVIDER: std::sync::OnceLock<Arc<OtelAuthCredentialProvider>> =
    std::sync::OnceLock::new();

/// Upgrade the OTel credential provider to use the agent's live `AuthManager`.
/// Call this once after the main `AuthManager` is constructed and has its refresher configured.
/// No-ops if the OTel layer was never initialized (e.g. `InstrumentationMode::Disabled`).
pub fn wire_otel_auth_manager(auth_manager: Arc<AuthManager>) {
    if let Some(provider) = OTEL_PROVIDER.get() {
        provider.set_live(auth_manager);
        tracing::debug!("otel: upgraded credential provider to live AuthManager");
    }
    // The external stream's identity attributes come from the same snapshot, so re-sync it here
    sync_external_otel_identity();
}

/// Email for the external OTEL stream. OIDC/gateway only; never API-key,
/// WebLogin, or a blank address. Callers must also skip deployment-key
/// snapshots — this helper only inspects `GrokAuth`.
pub fn oauth_gateway_email_from_auth(auth: &crate::GrokAuth) -> Option<String> {
    match auth.auth_mode {
        crate::AuthMode::Oidc | crate::AuthMode::External => {
            auth.email.clone().filter(|e| !e.is_empty())
        }
        crate::AuthMode::ApiKey | crate::AuthMode::WebLogin => None,
    }
}

/// Push the current identity attributes (never the token) to the external OTEL stream. Reads the same `CredentialSnapshot` the internal layer stamps per export, so both pipelines attribute identically.
/// `user.id` is copied whenever the snapshot has a non-empty principal (including API-key sessions). OAuth/gateway email is attached when present; never from git, API-key, or deployment-key.
/// No-op when the OTel provider was never initialized or the external stream is dormant.
pub fn sync_external_otel_identity() {
    if let Some(provider) = OTEL_PROVIDER.get() {
        let snapshot = provider.snapshot();
        let mut attrs = xai_grok_telemetry::external::IdentityAttrs::from_snapshot(&snapshot);
        attrs.email = provider.oauth_gateway_email();
        xai_grok_telemetry::external::set_identity(attrs);
    }
}

/// No-ops if the OTel layer was never initialized.
pub fn wire_otel_deployment_key(key: String) {
    if let Some(provider) = OTEL_PROVIDER.get() {
        provider.set_deployment_key(key);
        tracing::debug!("otel: set deployment key on credential provider");
        // Re-sync so the external stream picks up `deployment.id`, which `snapshot_inner` derives from the key
        // Deployment-key-only setups wire the key after `wire_otel_auth_manager` already synced
        // Without this re-sync the attribute would stay absent on customer exports until a later sync ran
        sync_external_otel_identity();
    }
}

/// Bootstrap the OTel credential provider both pager and TUI need at tracing init. Starts disk-read-only.
/// Call [`wire_otel_auth_manager`] after agent init to upgrade to the live `AuthManager` with active refresh.
/// Resolver and proxy URL are injected so this stays off shell config; the caller owns endpoint assembly.
pub fn install_bootstrap_otel_provider(
    proxy_base_url: String,
    deployment_id_resolver: DeploymentIdResolver,
) -> (Arc<dyn AuthCredentialProvider>, String) {
    let grok_com_config = crate::GrokComConfig::default();
    let token_header_value = grok_com_config.token_header.clone();

    let grok_home = xai_grok_shell_base::util::grok_home::grok_home();
    let bootstrap = Arc::new(AuthManager::new_with_proxy_base_url(
        &grok_home,
        grok_com_config,
        proxy_base_url,
    ));
    let provider = Arc::new(OtelAuthCredentialProvider::with_deployment_id_resolver(
        bootstrap,
        deployment_id_resolver,
    ));
    let _ = OTEL_PROVIDER.set(provider.clone());

    (
        provider as Arc<dyn AuthCredentialProvider>,
        token_header_value,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GrokAuth;
    use crate::GrokComConfig;
    use crate::manager::AuthManager;
    use chrono::{Duration as ChronoDuration, Utc};
    use std::sync::Mutex;
    use xai_grok_auth::AuthCredentialProvider;

    /// Serializes tests that pin `GROK_AUTH_EARLY_INVALIDATION_SECS`, since env vars are process-global and parallel tests would race.
    static EARLY_INVALIDATION_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard: pins `GROK_AUTH_EARLY_INVALIDATION_SECS` to the production default (300s) while held, restoring the previous value on drop.
    /// Acquires `EARLY_INVALIDATION_LOCK` so concurrent test runners can't observe a half-mutated env.
    struct EarlyInvalidationGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl EarlyInvalidationGuard {
        fn pin_to_default() -> Self {
            let lock = EARLY_INVALIDATION_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var("GROK_AUTH_EARLY_INVALIDATION_SECS").ok();
            // SAFETY: env var mutation is `unsafe` in edition 2024; the lock
            // above ensures no other test in this module reads/writes the
            // same key concurrently.
            unsafe { std::env::set_var("GROK_AUTH_EARLY_INVALIDATION_SECS", "300") };
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for EarlyInvalidationGuard {
        fn drop(&mut self) {
            // SAFETY: see `pin_to_default`; lock is still held until self is dropped.
            unsafe {
                match self.previous.take() {
                    Some(prev) => std::env::set_var("GROK_AUTH_EARLY_INVALIDATION_SECS", prev),
                    None => std::env::remove_var("GROK_AUTH_EARLY_INVALIDATION_SECS"),
                }
            }
        }
    }

    fn make_auth(key: &str, expires_in: ChronoDuration) -> GrokAuth {
        GrokAuth {
            key: key.to_string(),
            user_id: "test-user".to_string(),
            create_time: Utc::now(),
            expires_at: Some(Utc::now() + expires_in),
            ..GrokAuth::test_default()
        }
    }

    /// Build an `AuthManager` rooted at `dir`.
    /// The caller keeps `dir` alive for the duration of the test so the `TempDir` `Drop` actually cleans up.
    fn make_manager(dir: &tempfile::TempDir, initial: Option<GrokAuth>) -> Arc<AuthManager> {
        let mgr = AuthManager::new(dir.path(), GrokComConfig::default());
        if let Some(auth) = initial {
            mgr.hot_swap(auth);
        }
        Arc::new(mgr)
    }

    /// The shell half of the subagent-401 contract; the sampler half is pinned in xai-grok-sampler's resolver tests. Over a real `AuthManager` the resolver returns `None` when hard-expired (fail-closed).
    /// It returns the token inside the early-invalidation buffer (still proxy-accepted), and the fresh token after a rotation. The same resolver serves all three states without a client rebuild.
    #[test]
    fn wire_valid_resolver_tracks_manager_across_expiry_and_refresh() {
        use xai_grok_sampler::BearerResolver;
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("hard-expired-token", ChronoDuration::hours(-1))),
        );
        let resolver = WireValidBearerResolver(mgr.clone());

        assert_eq!(
            resolver.current_bearer(),
            None,
            "a hard-expired token must never ride the wire"
        );

        mgr.hot_swap(make_auth("buffer-window-token", ChronoDuration::minutes(4)));
        assert_eq!(
            resolver.current_bearer().as_deref(),
            Some("buffer-window-token"),
            "inside the early-invalidation buffer the token is still wire-valid"
        );

        mgr.hot_swap(make_auth("fresh-token", ChronoDuration::hours(1)));
        assert_eq!(
            resolver.current_bearer().as_deref(),
            Some("fresh-token"),
            "the same resolver must serve the rotated token without a rebuild"
        );
    }

    /// Refresher that mints a long-lived token and counts its runs.
    struct MintingRefresher(std::sync::atomic::AtomicU32);

    #[async_trait::async_trait]
    impl crate::refresh::TokenRefresher for MintingRefresher {
        async fn refresh(
            &self,
            _reason: crate::manager::RefreshReason,
        ) -> crate::refresh::RefreshOutcome {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::refresh::RefreshOutcome::success(GrokAuth {
                auth_mode: crate::AuthMode::Oidc,
                refresh_token: Some("rt".into()),
                ..make_auth("pre-send-minted", ChronoDuration::hours(1))
            })
        }
    }

    /// The pre-send hook closes the pre-flight→send gap.
    /// A bearer that is still wire-valid but would not outlive the send is refreshed before `current_bearer` reads it, so the request never leaves with no credential.
    /// A bearer with life to spare is left alone: no refresher run per request.
    #[tokio::test]
    async fn prepare_for_send_refreshes_a_bearer_that_would_not_outlive_the_send() {
        use xai_grok_sampler::BearerResolver;
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(GrokAuth {
                auth_mode: crate::AuthMode::Oidc,
                refresh_token: Some("rt".into()),
                ..make_auth("dying-token", ChronoDuration::seconds(2))
            }),
        );
        let refresher = Arc::new(MintingRefresher(std::sync::atomic::AtomicU32::new(0)));
        mgr.set_refresher(refresher.clone());
        let resolver = WireValidBearerResolver(mgr.clone());

        resolver.prepare_for_send().await;
        assert_eq!(
            refresher.0.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a bearer inside the send horizon must be refreshed before the send"
        );
        assert_eq!(
            resolver.current_bearer().as_deref(),
            Some("pre-send-minted"),
            "the request carries the renewed bearer"
        );

        // Plenty of life left: the hook is a cheap no-op.
        resolver.prepare_for_send().await;
        assert_eq!(
            refresher.0.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a sendable bearer must not trigger a refresh per request"
        );
    }

    /// With no wire-valid bearer the hook does nothing: the pre-flight already refreshed and the 401 arm owns the tokenless case.
    /// A refresh per send here would let parked, deliberately credential-less resubmits drive the escalation budget.
    #[tokio::test]
    async fn prepare_for_send_is_a_no_op_without_a_wire_valid_bearer() {
        use xai_grok_sampler::BearerResolver;
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(GrokAuth {
                auth_mode: crate::AuthMode::Oidc,
                refresh_token: Some("rt".into()),
                ..make_auth("hard-expired", ChronoDuration::hours(-1))
            }),
        );
        let refresher = Arc::new(MintingRefresher(std::sync::atomic::AtomicU32::new(0)));
        mgr.set_refresher(refresher.clone());
        let resolver = WireValidBearerResolver(mgr.clone());

        resolver.prepare_for_send().await;
        assert_eq!(
            refresher.0.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a hard-expired credential is the pre-flight's and the 401 arm's problem, not the send hook's"
        );
        assert_eq!(resolver.current_bearer(), None);
    }

    /// The wait is bounded by the cached bearer's remaining life, so a slow mint cannot outlive the token it was protecting.
    #[test]
    fn pre_send_budget_never_outlives_the_cached_bearer() {
        use std::time::Duration;
        assert_eq!(
            pre_send_refresh_budget(Duration::from_secs(60)),
            PRE_SEND_REFRESH_BUDGET,
            "plenty of life: the ceiling applies"
        );
        assert_eq!(
            pre_send_refresh_budget(Duration::from_secs(3)),
            Duration::from_millis(2500),
            "the stamp margin is reserved out of the remaining life"
        );
        assert_eq!(
            pre_send_refresh_budget(Duration::from_millis(300)),
            Duration::ZERO,
            "less than the margin: do not wait at all"
        );
    }

    /// Refresher that never answers inside any budget.
    struct StallingRefresher;

    #[async_trait::async_trait]
    impl crate::refresh::TokenRefresher for StallingRefresher {
        async fn refresh(
            &self,
            _reason: crate::manager::RefreshReason,
        ) -> crate::refresh::RefreshOutcome {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            crate::refresh::RefreshOutcome::transient("never")
        }
    }

    /// A stalled mint must not consume the bearer it was called to replace.
    /// The wait ends while the old bearer is still wire-valid, and the request goes out with it rather than with nothing.
    #[tokio::test(start_paused = true)]
    async fn prepare_for_send_gives_up_before_the_cached_bearer_dies() {
        use xai_grok_sampler::BearerResolver;
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(GrokAuth {
                auth_mode: crate::AuthMode::Oidc,
                refresh_token: Some("rt".into()),
                ..make_auth("dying-token", ChronoDuration::seconds(4))
            }),
        );
        mgr.set_refresher(Arc::new(StallingRefresher));
        let resolver = WireValidBearerResolver(mgr.clone());

        let started = tokio::time::Instant::now();
        resolver.prepare_for_send().await;
        let waited = started.elapsed();
        assert!(
            waited <= std::time::Duration::from_millis(3500),
            "the wait must end before the 4 s bearer dies (waited {waited:?})"
        );
        assert_eq!(
            resolver.current_bearer().as_deref(),
            Some("dying-token"),
            "the still wire-valid bearer rides the wire instead of nothing"
        );
    }

    /// `apply()` and `snapshot()` agree when the in-memory token is fresh: what snapshot reports is what goes on the wire.
    #[test]
    fn apply_and_snapshot_agree_on_live_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("live-token", ChronoDuration::hours(1))),
        );
        let provider = ShellAuthCredentialProvider::new(mgr, None, None);

        let snap = provider.snapshot();
        assert_eq!(snap.token.as_deref(), Some("live-token"));
        assert_eq!(snap.user_id.as_deref(), Some("test-user"));
    }

    /// During the 5-minute pre-refresh buffer window, `auth_manager.current()` returns `None`, but the token is still valid at the proxy. The manager treats such a token as expiring soon for refresh scheduling.
    /// The provider must fall back to `expired_auth()` so the in-memory token gets sent instead of nothing. Sending nothing here caused the bulk of the `POST /v1/storage` 401s observed in production.
    #[test]
    fn falls_back_to_expired_auth_during_buffer_window() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        // The token expires in 4 minutes, inside the pinned 5-minute buffer, so `current()` returns None and `expired_auth()` returns Some
        let mgr = make_manager(
            &dir,
            Some(make_auth("buffer-token", ChronoDuration::minutes(4))),
        );
        assert!(mgr.current().is_none(), "buffer-window precondition");
        assert!(mgr.expired_auth().is_some(), "buffer-window precondition");

        let provider = ShellAuthCredentialProvider::new(mgr, None, None);

        let snap = provider.snapshot();
        assert_eq!(
            snap.token.as_deref(),
            Some("buffer-token"),
            "snapshot should fall back to expired_auth instead of None"
        );
        assert_eq!(snap.user_id.as_deref(), Some("test-user"));
    }

    /// When `auth_manager` has nothing at all (no in-memory auth, expired or otherwise), `snapshot()` returns `None` for the user-token branch.
    /// `apply()` would then send no Authorization header.
    #[test]
    fn no_token_when_auth_manager_is_empty() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider = ShellAuthCredentialProvider::new(mgr, None, None);

        let snap = provider.snapshot();
        assert!(
            snap.token.is_none(),
            "snapshot should be None when manager has no auth"
        );
        assert!(snap.user_id.is_none());
    }

    /// 401 recovery routes through `unauthorized_recovery` and actually runs the configured refresher.
    #[tokio::test]
    async fn refresh_after_unauthorized_drives_recovery_state_machine() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(AuthManager::new(
            dir.path(),
            crate::GrokComConfig::default(),
        ));
        mgr.hot_swap(GrokAuth {
            key: "stale".into(),
            auth_mode: crate::AuthMode::Oidc,
            create_time: chrono::Utc::now() - ChronoDuration::hours(2),
            user_id: "u".into(),
            refresh_token: Some("rt-stale".into()),
            expires_at: Some(chrono::Utc::now() - ChronoDuration::hours(1)),
            ..GrokAuth::test_default()
        });

        struct OkRefresher {
            calls: Arc<std::sync::atomic::AtomicU32>,
        }
        #[async_trait::async_trait]
        impl crate::refresh::TokenRefresher for OkRefresher {
            async fn refresh(
                &self,
                _r: crate::manager::RefreshReason,
            ) -> crate::refresh::RefreshOutcome {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::refresh::RefreshOutcome::Success(Box::new(GrokAuth {
                    key: "fresh".into(),
                    auth_mode: crate::AuthMode::Oidc,
                    create_time: chrono::Utc::now(),
                    user_id: "u".into(),
                    refresh_token: Some("rt-new".into()),
                    expires_at: Some(chrono::Utc::now() + ChronoDuration::hours(1)),
                    ..GrokAuth::test_default()
                }))
            }
        }
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        mgr.set_refresher(Arc::new(OkRefresher {
            calls: calls.clone(),
        }));

        let provider = ShellAuthCredentialProvider::new(mgr.clone(), None, None);
        assert!(provider.refresh_after_unauthorized().await);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(mgr.current().unwrap().key, "fresh");

        // Verify snapshot picks up the refreshed token, keeping the wire and the cache in agreement
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("fresh"),
            "snapshot must reflect refreshed token for subsequent apply() calls"
        );
    }

    #[test]
    fn embedding_session_credentials_scopes_to_first_party() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("xai-session-token", ChronoDuration::hours(1))),
        );
        let api_key_provider: xai_grok_tools::types::SharedApiKeyProvider =
            Arc::new(crate::side_call_bearer::SharedAuthKeyProvider(mgr.clone()));

        for denied in [
            "https://byok.attacker.example/v1",
            // First-party host, but cleartext: bearer requires https.
            "http://api.x.ai/v1",
        ] {
            let resolved =
                embedding_session_credentials(denied, Some(&mgr), Some(api_key_provider.clone()));
            assert!(
                resolved.is_empty(),
                "session credentials must not reach {denied}"
            );
        }

        let resolved = embedding_session_credentials(
            "https://api.x.ai/v1",
            Some(&mgr),
            Some(api_key_provider),
        );
        assert!(!resolved.is_empty());
    }

    /// Deployment-key path has no recovery (operator owns the bearer).
    #[tokio::test]
    async fn refresh_after_unauthorized_is_noop_for_deployment_key() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider =
            ShellAuthCredentialProvider::new(mgr, Some("deployment-key".to_string()), None);
        assert!(!provider.refresh_after_unauthorized().await);
    }

    #[test]
    fn snapshot_populates_tenant_id_per_auth_mode() {
        use xai_grok_telemetry::config::deployment_id_from_key;
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();

        let dep = ShellAuthCredentialProvider::with_deployment_id_resolver(
            make_manager(&dir, None),
            Some("xai-token-EX".into()),
            None,
            std::sync::Arc::new(|k: Option<&str>| {
                k.filter(|s| !s.is_empty()).map(deployment_id_from_key)
            }),
        )
        .snapshot();
        assert_eq!(
            dep.deployment_id.as_deref(),
            Some(deployment_id_from_key("xai-token-EX").as_str())
        );
        assert!(dep.api_key_id.is_none());

        let api_auth = GrokAuth {
            key: "sk-apikey-xyz".into(),
            auth_mode: crate::AuthMode::ApiKey,
            expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            ..GrokAuth::test_default()
        };
        let api = ShellAuthCredentialProvider::new(make_manager(&dir, Some(api_auth)), None, None)
            .snapshot();
        assert_eq!(
            api.api_key_id.as_deref(),
            Some(deployment_id_from_key("sk-apikey-xyz").as_str())
        );
        assert!(api.deployment_id.is_none());
        assert_eq!(
            api.user_id.as_deref(),
            Some("test-user"),
            "API-key sessions still carry the snapshot principal; emit attaches user.id"
        );

        let oidc = ShellAuthCredentialProvider::new(
            make_manager(
                &dir,
                Some(make_auth("oidc-token", ChronoDuration::hours(1))),
            ),
            None,
            None,
        )
        .snapshot();
        assert!(oidc.deployment_id.is_none() && oidc.api_key_id.is_none());
    }

    /// Bootstrap mode: `snapshot()` re-reads disk, so a token rotated by a sibling process is picked up without a live AuthManager.
    #[test]
    fn otel_bootstrap_snapshot_picks_up_disk_writes() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::GrokComConfig::default().auth_scope();
        let auth_path = dir.path().join("auth.json");

        let mgr = make_manager(
            &dir,
            Some(make_auth("initial-token", ChronoDuration::hours(1))),
        );
        let mut store = crate::read_auth_json(&auth_path).unwrap_or_default();
        store.insert(
            scope.clone(),
            make_auth("initial-token", ChronoDuration::hours(1)),
        );
        crate::storage::write_auth_json(&auth_path, &store).unwrap();

        let provider = OtelAuthCredentialProvider::new(mgr);
        assert_eq!(provider.snapshot().token.as_deref(), Some("initial-token"));

        // Simulate sibling rotation on disk.
        let mut store = crate::read_auth_json(&auth_path).unwrap();
        store.insert(scope, make_auth("rotated-token", ChronoDuration::hours(1)));
        crate::storage::write_auth_json(&auth_path, &store).unwrap();

        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("rotated-token"),
            "must pick up sibling-rotated tokens from disk"
        );
    }

    /// After `set_live()`, `snapshot()` reads from the live manager's in-memory cache with no disk re-read.
    /// `refresh_after_unauthorized()` then drives the recovery state machine.
    #[tokio::test]
    async fn otel_live_mode_uses_shared_auth_manager() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let bootstrap_dir = tempfile::tempdir().unwrap();
        let bootstrap_mgr = make_manager(&bootstrap_dir, None);
        let provider = OtelAuthCredentialProvider::new(bootstrap_mgr);

        // Bootstrap mode: no token.
        assert!(provider.snapshot().token.is_none());
        assert!(!provider.refresh_after_unauthorized().await);

        // Wire up the live AuthManager with a fresh token.
        let live_dir = tempfile::tempdir().unwrap();
        let live_mgr = make_manager(
            &live_dir,
            Some(make_auth("live-token", ChronoDuration::hours(1))),
        );
        provider.set_live(live_mgr.clone());

        // Live mode: reads from in-memory cache.
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("live-token"),
            "must read from live AuthManager after set_live()"
        );

        // Rotate the live manager's token (simulating proactive refresh).
        live_mgr.hot_swap(make_auth("rotated-live", ChronoDuration::hours(1)));
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("rotated-live"),
            "must see rotated token from live manager"
        );
    }

    /// `refresh_after_unauthorized` drives recovery when live.
    #[tokio::test]
    async fn otel_live_refresh_after_unauthorized_drives_recovery() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let bootstrap_dir = tempfile::tempdir().unwrap();
        let bootstrap_mgr = make_manager(&bootstrap_dir, None);
        let provider = OtelAuthCredentialProvider::new(bootstrap_mgr);

        let live_dir = tempfile::tempdir().unwrap();
        let live_mgr = Arc::new(AuthManager::new(
            live_dir.path(),
            crate::GrokComConfig::default(),
        ));
        live_mgr.hot_swap(GrokAuth {
            key: "stale".into(),
            auth_mode: crate::AuthMode::Oidc,
            create_time: chrono::Utc::now() - ChronoDuration::hours(2),
            user_id: "u".into(),
            refresh_token: Some("rt-stale".into()),
            expires_at: Some(chrono::Utc::now() - ChronoDuration::hours(1)),
            ..GrokAuth::test_default()
        });

        struct OkRefresher;
        #[async_trait::async_trait]
        impl crate::refresh::TokenRefresher for OkRefresher {
            async fn refresh(
                &self,
                _r: crate::manager::RefreshReason,
            ) -> crate::refresh::RefreshOutcome {
                crate::refresh::RefreshOutcome::Success(Box::new(GrokAuth {
                    key: "refreshed".into(),
                    auth_mode: crate::AuthMode::Oidc,
                    create_time: chrono::Utc::now(),
                    user_id: "u".into(),
                    refresh_token: Some("rt-new".into()),
                    expires_at: Some(chrono::Utc::now() + ChronoDuration::hours(1)),
                    ..GrokAuth::test_default()
                }))
            }
        }
        live_mgr.set_refresher(Arc::new(OkRefresher));

        provider.set_live(live_mgr.clone());
        assert!(
            provider.refresh_after_unauthorized().await,
            "live mode must drive recovery"
        );
        assert_eq!(live_mgr.current().unwrap().key, "refreshed");
    }

    #[test]
    fn otel_deployment_key_sent_when_no_oidc_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None); // no OIDC token
        let provider = OtelAuthCredentialProvider::new(mgr);
        provider.set_deployment_key("enterprise-key".to_string());

        let snap = provider.snapshot();
        assert_eq!(
            snap.token.as_deref(),
            Some("enterprise-key"),
            "deployment key must be sent when no OIDC token exists"
        );
    }

    #[test]
    fn otel_deployment_key_wins_over_oidc_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("oidc-token", ChronoDuration::hours(1))),
        );
        let provider = OtelAuthCredentialProvider::new(mgr);
        provider.set_deployment_key("deployment-key-123".to_string());

        let snap = provider.snapshot();
        assert_eq!(
            snap.token.as_deref(),
            Some("deployment-key-123"),
            "deployment key must win over OIDC token"
        );
        assert!(snap.user_id.is_none());
    }

    #[test]
    fn has_usable_credential_reflects_auth_state() {
        let _guard = EarlyInvalidationGuard::pin_to_default();

        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, Some(make_auth("live", ChronoDuration::hours(1))));
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            provider.has_usable_credential(),
            "valid unexpired token is usable"
        );

        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, Some(make_auth("stale", ChronoDuration::hours(-1))));
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            !provider.has_usable_credential(),
            "expired token is not usable"
        );

        let dir = tempfile::tempdir().unwrap();
        let provider = OtelAuthCredentialProvider::new(make_manager(&dir, None));
        assert!(
            !provider.has_usable_credential(),
            "absent token is not usable"
        );

        // A refresh verdict must not make a still wire-valid access token unusable
        // The gate keys on wire-validity, not the verdict, so a refresh failure doesn't pause uploads while the cached token is good
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, Some(make_auth("live", ChronoDuration::hours(1))));
        mgr.record_permanent_failure(
            "live".into(),
            crate::error::RefreshTokenFailedReason::RefreshTokenRejected.into(),
        );
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            provider.has_usable_credential(),
            "a wire-valid token stays usable despite a permanent refresh verdict"
        );

        let dir = tempfile::tempdir().unwrap();
        let provider = OtelAuthCredentialProvider::new(make_manager(&dir, None));
        provider.set_deployment_key("enterprise-key".to_string());
        assert!(
            provider.has_usable_credential(),
            "static deployment key is always usable"
        );
    }

    /// A token inside the early-invalidation buffer is still accepted by the proxy. The buffer is a client-side pre-refresh margin, not a wire expiry, and the sender puts the token on the wire via `current_or_expired()`.
    /// The export gate must therefore keep it usable even though `current()` reports `None`. Regression test: exports used to stop during the buffer window.
    #[test]
    fn has_usable_credential_true_inside_early_invalidation_buffer() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("buffered", ChronoDuration::minutes(4))),
        );
        assert!(
            mgr.current().is_none(),
            "4-min token sits inside the pinned 5-min buffer, so current() is None"
        );
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            provider.has_usable_credential(),
            "a buffer-window token is still wire-valid, so the gate keeps it usable"
        );
    }

    /// A configured `deployment_key` always wins over the AuthManager-resolved user token, matching the precedence in `GrokAuthCredentials::apply`.
    /// The snapshot must report the deployment key so the 401-attribution prefix matches the wire bytes.
    #[test]
    fn deployment_key_wins_over_resolved_user_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("user-token", ChronoDuration::hours(1))),
        );
        let provider =
            ShellAuthCredentialProvider::new(mgr, Some("deployment-key-12345".to_string()), None);

        let snap = provider.snapshot();
        assert_eq!(snap.token.as_deref(), Some("deployment-key-12345"));
        // The deployment-key path returns a `None` user_id per the CredentialSnapshot contract; only user-token resolution carries a user_id
        assert!(snap.user_id.is_none());
    }

    /// A configurable [`PluginCredentialSeam`] for the seam-precedence tests.
    /// `seen_base_url` records the `base_url` the last resolve/refresh dispatch
    /// carried, so a test can assert the outbound target is forwarded to the
    /// plugin (the scoping the plugin then enforces); `seen_account` does the
    /// same for the `auth_account` selector.
    #[derive(Debug, Default)]
    struct MockSeam {
        resolve: Option<PluginCredential>,
        refresh: Option<PluginCredential>,
        oauth: Option<PluginCredential>,
        seen_base_url: std::sync::Mutex<Option<String>>,
        seen_account: std::sync::Mutex<Option<String>>,
    }
    #[async_trait::async_trait]
    impl PluginCredentialSeam for MockSeam {
        async fn resolve(
            &self,
            _reason: &str,
            base_url: &str,
            account: Option<&str>,
        ) -> Option<PluginCredential> {
            *self.seen_base_url.lock().unwrap() = Some(base_url.to_string());
            *self.seen_account.lock().unwrap() = account.map(str::to_string);
            self.resolve.clone()
        }
        async fn refresh(
            &self,
            _reason: &str,
            _owner_id: Option<&str>,
            base_url: &str,
            account: Option<&str>,
        ) -> Option<PluginCredential> {
            *self.seen_base_url.lock().unwrap() = Some(base_url.to_string());
            *self.seen_account.lock().unwrap() = account.map(str::to_string);
            self.refresh.clone()
        }
        async fn start_oauth_flow(
            &self,
            _reason: &str,
            _target_plugin: Option<&str>,
            account: Option<&str>,
        ) -> Option<PluginCredential> {
            *self.seen_account.lock().unwrap() = account.map(str::to_string);
            self.oauth.clone()
        }
    }

    fn plugin_cred(token: &str, header: bool) -> PluginCredential {
        PluginCredential {
            token: token.into(),
            needs_token_auth_header: header,
            expires_at_ms: None,
            owner_id: Some("plugin-owner".into()),
        }
    }

    /// resolve_credential caches the plugin bearer and every wire path prefers
    /// it over the built-in `AuthManager` token.
    #[tokio::test]
    async fn seam_resolve_feeds_snapshot_and_wire() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("builtin-token", ChronoDuration::hours(1))),
        );
        let seam = Arc::new(MockSeam {
            resolve: Some(plugin_cred("plugin-token", false)),
            ..Default::default()
        });
        let provider =
            ShellAuthCredentialProvider::new(mgr, None, None).with_credential_seam(seam.clone());

        // Before resolve: the built-in token is used.
        assert_eq!(provider.snapshot().token.as_deref(), Some("builtin-token"));

        assert!(
            provider
                .resolve_credential("bootstrap", "https://api.anthropic.com", None)
                .await
        );
        // The outbound target is forwarded to the plugin so it can scope its
        // reply (the fix for credentials leaking to a foreign provider).
        assert_eq!(
            seam.seen_base_url.lock().unwrap().as_deref(),
            Some("https://api.anthropic.com")
        );
        let snap = provider.snapshot();
        assert_eq!(snap.token.as_deref(), Some("plugin-token"));
        assert_eq!(snap.user_id.as_deref(), Some("plugin-owner"));
        // A bare-Bearer plugin credential clears the token-auth marker.
        assert!(!provider.needs_token_auth_header());
        // No `auth_account` configured → no account selector reaches the plugin,
        // which is the pre-selector behaviour.
        assert!(seam.seen_account.lock().unwrap().is_none());
    }

    /// The configured `auth_account` rides the resolve dispatch, and the
    /// argument-less 401 refresh asks for the *same* account — so a plugin
    /// holding several accounts for one provider refreshes the right one.
    #[tokio::test]
    async fn seam_account_rides_resolve_and_is_reused_by_refresh() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let seam = Arc::new(MockSeam {
            resolve: Some(plugin_cred("work-token", true)),
            refresh: Some(plugin_cred("work-token-2", true)),
            ..Default::default()
        });
        let provider =
            ShellAuthCredentialProvider::new(mgr, None, None).with_credential_seam(seam.clone());

        assert!(
            provider
                .resolve_credential("bootstrap", "https://example.test/v1", Some("work"))
                .await
        );
        assert_eq!(seam.seen_account.lock().unwrap().as_deref(), Some("work"));

        // The refresh takes no arguments, so it must reuse the stashed selector
        // rather than the cached credential's owner id.
        assert!(provider.refresh_after_unauthorized().await);
        assert_eq!(seam.seen_account.lock().unwrap().as_deref(), Some("work"));
        assert_eq!(provider.snapshot().token.as_deref(), Some("work-token-2"));
    }

    /// Two providers of the same plugin, distinguished only by `auth_account`,
    /// resolve independently: each dispatch carries its own selector and caches
    /// its own bearer.
    #[tokio::test]
    async fn two_accounts_on_one_base_url_resolve_independently() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        const BASE: &str = "https://example.test/v1";

        let work_seam = Arc::new(MockSeam {
            resolve: Some(plugin_cred("work-token", true)),
            ..Default::default()
        });
        let work = ShellAuthCredentialProvider::new(make_manager(&dir, None), None, None)
            .with_credential_seam(work_seam.clone());

        let personal_seam = Arc::new(MockSeam {
            resolve: Some(plugin_cred("personal-token", true)),
            ..Default::default()
        });
        let personal = ShellAuthCredentialProvider::new(make_manager(&dir, None), None, None)
            .with_credential_seam(personal_seam.clone());

        assert!(
            work.resolve_credential("outbound", BASE, Some("work"))
                .await
        );
        assert!(
            personal
                .resolve_credential("outbound", BASE, Some("personal"))
                .await
        );

        // Same endpoint, different account selector, different bearer.
        assert_eq!(
            work_seam.seen_base_url.lock().unwrap().as_deref(),
            personal_seam.seen_base_url.lock().unwrap().as_deref()
        );
        assert_eq!(
            work_seam.seen_account.lock().unwrap().as_deref(),
            Some("work")
        );
        assert_eq!(
            personal_seam.seen_account.lock().unwrap().as_deref(),
            Some("personal")
        );
        assert_eq!(work.snapshot().token.as_deref(), Some("work-token"));
        assert_eq!(personal.snapshot().token.as_deref(), Some("personal-token"));
    }

    /// Passthrough (the plugin declines) leaves the built-in resolution intact.
    #[tokio::test]
    async fn seam_resolve_passthrough_keeps_builtin() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("builtin-token", ChronoDuration::hours(1))),
        );
        let provider = ShellAuthCredentialProvider::new(mgr, None, None)
            .with_credential_seam(Arc::new(MockSeam::default()));

        assert!(
            !provider
                .resolve_credential("bootstrap", "https://api.x.ai/v1", None)
                .await
        );
        assert_eq!(provider.snapshot().token.as_deref(), Some("builtin-token"));
        assert!(provider.needs_token_auth_header());
    }

    /// No seam injected → the built-in path is untouched (fail-open default).
    #[tokio::test]
    async fn no_seam_uses_builtin_only() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("builtin-token", ChronoDuration::hours(1))),
        );
        let provider = ShellAuthCredentialProvider::new(mgr, None, None);
        assert!(
            !provider
                .resolve_credential("bootstrap", "https://api.x.ai/v1", None)
                .await
        );
        assert!(!provider.start_oauth_flow("sign_in", None, None).await);
        assert_eq!(provider.snapshot().token.as_deref(), Some("builtin-token"));
    }

    /// On a 401, the refresh seam mints a fresh plugin bearer that the next
    /// snapshot reflects (so the retry sends it).
    #[tokio::test]
    async fn seam_refresh_updates_snapshot() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider = ShellAuthCredentialProvider::new(mgr, None, None).with_credential_seam(
            Arc::new(MockSeam {
                refresh: Some(plugin_cred("refreshed-plugin-token", true)),
                ..Default::default()
            }),
        );
        assert!(provider.refresh_after_unauthorized().await);
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("refreshed-plugin-token")
        );
    }

    /// The interactive flow's final bearer is cached and preferred on the wire.
    #[tokio::test]
    async fn seam_oauth_flow_caches_final_credential() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider = ShellAuthCredentialProvider::new(mgr, None, None).with_credential_seam(
            Arc::new(MockSeam {
                oauth: Some(plugin_cred("oauth-token", true)),
                ..Default::default()
            }),
        );
        assert!(provider.snapshot().token.is_none());
        assert!(
            provider
                .start_oauth_flow("missing_credential", None, None)
                .await
        );
        assert_eq!(provider.snapshot().token.as_deref(), Some("oauth-token"));
    }

    /// An expired plugin credential is ignored: the wire falls back to the
    /// built-in path until a refresh replaces it.
    #[tokio::test]
    async fn expired_plugin_credential_falls_back() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("builtin-token", ChronoDuration::hours(1))),
        );
        let provider = ShellAuthCredentialProvider::new(mgr, None, None).with_credential_seam(
            Arc::new(MockSeam {
                resolve: Some(PluginCredential {
                    token: "stale-plugin".into(),
                    needs_token_auth_header: true,
                    expires_at_ms: Some(1),
                    owner_id: None,
                }),
                ..Default::default()
            }),
        );
        assert!(
            provider
                .resolve_credential("bootstrap", "https://api.x.ai/v1", None)
                .await
        );
        // Cached but already expired -> snapshot falls back to the built-in token.
        assert_eq!(provider.snapshot().token.as_deref(), Some("builtin-token"));
    }

    #[test]
    fn oauth_gateway_email_oidc_and_external_only() {
        let oidc = GrokAuth {
            auth_mode: crate::AuthMode::Oidc,
            email: Some("alice@corp.example".into()),
            ..GrokAuth::test_default()
        };
        assert_eq!(
            oauth_gateway_email_from_auth(&oidc).as_deref(),
            Some("alice@corp.example")
        );

        let external = GrokAuth {
            auth_mode: crate::AuthMode::External,
            email: Some("bob@gateway.example".into()),
            ..GrokAuth::test_default()
        };
        assert_eq!(
            oauth_gateway_email_from_auth(&external).as_deref(),
            Some("bob@gateway.example")
        );

        let blank = GrokAuth {
            auth_mode: crate::AuthMode::Oidc,
            email: Some(String::new()),
            ..GrokAuth::test_default()
        };
        assert_eq!(oauth_gateway_email_from_auth(&blank), None);

        let api = GrokAuth {
            auth_mode: crate::AuthMode::ApiKey,
            email: Some("should-not-export@example.com".into()),
            ..GrokAuth::test_default()
        };
        assert_eq!(oauth_gateway_email_from_auth(&api), None);
    }
}
