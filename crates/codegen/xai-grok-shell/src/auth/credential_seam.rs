//! Plugin credential seam: lets a sidecar plugin supply, refresh, or
//! interactively authorize the outbound bearer, without `xai-grok-auth` ever
//! depending on the hooks crate.
//!
//! This mirrors the sampler's `provider_request` seam (see
//! `session::acp_session_impl::provider_control`): the dispatch lives here in
//! the shell and is injected into [`ShellAuthCredentialProvider`] as a callback
//! ([`PluginCredentialSeam`]); the credential provider itself stays free of any
//! hooks dependency and simply consults the callback (fail-open) before its
//! built-in resolution/refresh.
//!
//! Three events drive the seam:
//! - `resolve_credential` (Replace) — supply a bearer instead of the built-in
//!   resolution, e.g. from an external identity provider.
//! - `refresh_credential` (Replace) — mint a fresh bearer on a `401`/expiry.
//! - `start_oauth_flow` (Intercept) — drive the whole interactive authorization
//!   (authorize URL / device code / callback / token exchange) and return the
//!   final bearer.
//!
//! Masking the resolved bearer onto outbound requests is handled by the
//! existing `provider_request` seam, not here; this seam only produces the
//! credential the core then holds and sends.

use std::sync::Arc;

use serde::Deserialize;
use xai_grok_hooks::discovery::HookRegistry;
use xai_grok_hooks::dispatcher::{dispatch_intercept, dispatch_replace};
use xai_grok_hooks::event::{HookEventEnvelope, HookEventName, HookPayload};
use xai_grok_hooks::invoker::PluginHookInvoker;
use xai_grok_hooks::runner::RunContext;

/// A credential a plugin returned across the seam. The shell-side mirror of the
/// wire `PluginCredentialDto`; the bearer is held by the core and masked in
/// logs by the `xai-grok-secrets` sanitizer (bearer/JWT shapes are redacted).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PluginCredential {
    /// Bearer token to send on outbound requests.
    pub token: String,
    /// Whether the token-auth marker header accompanies the bearer. Defaults to
    /// `true`; `false` requests a bare Bearer (deployment-key style).
    #[serde(default = "default_true")]
    pub needs_token_auth_header: bool,
    /// Absolute expiry (Unix-epoch milliseconds); `None` = no known expiry.
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    /// Stable owner id, echoed back on a later `refresh_credential`.
    #[serde(default)]
    pub owner_id: Option<String>,
}

fn default_true() -> bool {
    true
}

impl PluginCredential {
    /// Whether the credential is unexpired at `now_ms` (always true when no
    /// expiry is set).
    pub fn is_unexpired(&self, now_ms: i64) -> bool {
        self.expires_at_ms.is_none_or(|exp| exp > now_ms)
    }

    /// Parse a dispatched Replace/Intercept payload into a credential, logging
    /// (never panicking) on a malformed reply so the seam fails open.
    fn from_payload(value: serde_json::Value) -> Option<Self> {
        match serde_json::from_value::<Self>(value) {
            Ok(cred) if !cred.token.is_empty() => Some(cred),
            Ok(_) => {
                tracing::warn!("credential seam: plugin returned an empty token; ignoring");
                None
            }
            Err(err) => {
                tracing::warn!(%err, "credential seam: malformed plugin credential; ignoring");
                None
            }
        }
    }
}

/// The injected callback [`ShellAuthCredentialProvider`] consults before its
/// built-in resolution/refresh. `None` from any method means "no plugin
/// credential" — the caller falls back to the built-in path (fail-open).
#[async_trait::async_trait]
pub trait PluginCredentialSeam: Send + Sync + std::fmt::Debug + 'static {
    /// Resolve a credential before the built-in resolution runs. `reason`
    /// describes the context (`bootstrap`, `outbound`, …); `base_url` is the
    /// outbound endpoint the credential is resolved *for*, so the plugin can
    /// scope its reply to the target provider (empty when the fire site has no
    /// specific target). `account` is the configured `auth_account` selector —
    /// which of the plugin's accounts for this provider the core wants; `None`
    /// asks for the plugin's default account.
    async fn resolve(
        &self,
        reason: &str,
        base_url: &str,
        account: Option<&str>,
    ) -> Option<PluginCredential>;

    /// Mint a fresh credential on a `401`/expiry. `owner_id` is the owner of the
    /// credential being refreshed, when known; `base_url` is the outbound
    /// endpoint the refreshed credential is destined for (see [`Self::resolve`]).
    /// `account` is the requested `auth_account` selector, a separate question
    /// from `owner_id`: the former says *which account the core wants*, the
    /// latter *whose token went stale*. They differ when nothing is cached yet
    /// or when the cached token belongs to another account.
    async fn refresh(
        &self,
        reason: &str,
        owner_id: Option<&str>,
        base_url: &str,
        account: Option<&str>,
    ) -> Option<PluginCredential>;

    /// Drive the whole interactive authorization flow and return the final
    /// credential. `reason` describes what triggered it (`missing_credential`,
    /// `sign_in`, …). `target_plugin`, when `Some(name)`, restricts the flow to
    /// that single plugin's handler (used when the user picks one plugin's
    /// sign-in from `/login`); `None` keeps the all-subscribers behavior so the
    /// change is additive. `account` names which of the plugin's accounts to
    /// authorize (see [`Self::resolve`]).
    async fn start_oauth_flow(
        &self,
        reason: &str,
        target_plugin: Option<&str>,
        account: Option<&str>,
    ) -> Option<PluginCredential>;
}

/// Concrete seam that dispatches the three credential events to subscribed
/// sidecar plugins through the hooks registry.
///
/// Holds an immutable snapshot of the registry plus the session's plugin
/// invoker and envelope metadata. Resolve/refresh go through the Replace
/// dispatcher; the interactive flow goes through the Intercept dispatcher. The
/// per-event deadline is the plugin's configured hook timeout — an interactive
/// flow declares a long one (bounded by the hook-timeout cap), so no separate
/// timeout is threaded here.
pub struct HookCredentialSeam {
    registry: HookRegistry,
    invoker: Arc<dyn PluginHookInvoker>,
    session_id: String,
    cwd: String,
    workspace_root: String,
}

impl std::fmt::Debug for HookCredentialSeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookCredentialSeam")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl HookCredentialSeam {
    pub fn new(
        registry: HookRegistry,
        invoker: Arc<dyn PluginHookInvoker>,
        session_id: String,
        cwd: String,
        workspace_root: String,
    ) -> Self {
        Self {
            registry,
            invoker,
            session_id,
            cwd,
            workspace_root,
        }
    }

    fn envelope(&self, event: HookEventName, payload: HookPayload) -> HookEventEnvelope {
        HookEventEnvelope {
            hook_event_name: event,
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            workspace_root: self.workspace_root.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: None,
            payload,
        }
    }

    fn run_ctx(&self) -> RunContext<'_> {
        RunContext {
            session_id: &self.session_id,
            workspace_root: &self.workspace_root,
            plugin_invoker: Some(self.invoker.clone()),
        }
    }

    /// Whether any plugin subscribes to `event`; lets a caller skip the seam
    /// entirely when nothing would fire.
    pub fn has_subscriber(&self, event: HookEventName) -> bool {
        self.registry.has_enabled_hooks_for_canonical(event)
    }

    /// A registry containing only `plugin`'s hooks for `event`. Used to target a
    /// single plugin's interactive sign-in when the user selects it from
    /// `/login`, so no other subscribed plugin's handler runs.
    fn registry_for_plugin(&self, event: HookEventName, plugin: &str) -> HookRegistry {
        let mut filtered = HookRegistry::default();
        filtered.append_specs(
            self.registry
                .hooks_for_canonical(event)
                .into_iter()
                .filter(|spec| spec.plugin.as_deref() == Some(plugin))
                .cloned()
                .collect(),
        );
        filtered
    }
}

#[async_trait::async_trait]
impl PluginCredentialSeam for HookCredentialSeam {
    async fn resolve(
        &self,
        reason: &str,
        base_url: &str,
        account: Option<&str>,
    ) -> Option<PluginCredential> {
        if !self.has_subscriber(HookEventName::ResolveCredential) {
            return None;
        }
        let envelope = self.envelope(
            HookEventName::ResolveCredential,
            HookPayload::ResolveCredential {
                reason: reason.to_string(),
                base_url: base_url.to_string(),
                owner_hint: account.map(str::to_string),
            },
        );
        let value = dispatch_replace(
            &self.registry,
            HookEventName::ResolveCredential,
            &envelope,
            &self.run_ctx(),
        )
        .await?;
        PluginCredential::from_payload(value)
    }

    async fn refresh(
        &self,
        reason: &str,
        owner_id: Option<&str>,
        base_url: &str,
        account: Option<&str>,
    ) -> Option<PluginCredential> {
        if !self.has_subscriber(HookEventName::RefreshCredential) {
            return None;
        }
        let envelope = self.envelope(
            HookEventName::RefreshCredential,
            HookPayload::RefreshCredential {
                reason: reason.to_string(),
                base_url: base_url.to_string(),
                owner_id: owner_id.map(str::to_string),
                owner_hint: account.map(str::to_string),
            },
        );
        let value = dispatch_replace(
            &self.registry,
            HookEventName::RefreshCredential,
            &envelope,
            &self.run_ctx(),
        )
        .await?;
        PluginCredential::from_payload(value)
    }

    async fn start_oauth_flow(
        &self,
        reason: &str,
        target_plugin: Option<&str>,
        account: Option<&str>,
    ) -> Option<PluginCredential> {
        if !self.has_subscriber(HookEventName::StartOauthFlow) {
            return None;
        }
        let envelope = self.envelope(
            HookEventName::StartOauthFlow,
            HookPayload::StartOauthFlow {
                reason: reason.to_string(),
                owner_hint: account.map(str::to_string),
            },
        );
        // When a specific plugin is targeted, dispatch against a registry
        // filtered to just that plugin's `StartOauthFlow` hooks, so exactly one
        // sidecar's handler drives the flow (every sidecar auto-subscribes to
        // the event, so an unfiltered dispatch would run the first one, not the
        // one the user picked). `None` dispatches against the full registry.
        let filtered;
        let registry = match target_plugin {
            Some(plugin) => {
                filtered = self.registry_for_plugin(HookEventName::StartOauthFlow, plugin);
                &filtered
            }
            None => &self.registry,
        };
        let value = dispatch_intercept(
            registry,
            HookEventName::StartOauthFlow,
            &envelope,
            &self.run_ctx(),
        )
        .await?;
        PluginCredential::from_payload(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_credential_defaults_and_expiry() {
        let c: PluginCredential = serde_json::from_value(serde_json::json!({ "token": "t" })).unwrap();
        assert!(c.needs_token_auth_header);
        assert_eq!(c.expires_at_ms, None);
        assert!(c.is_unexpired(0));

        let c = PluginCredential {
            token: "t".into(),
            needs_token_auth_header: false,
            expires_at_ms: Some(1_000),
            owner_id: Some("o".into()),
        };
        assert!(c.is_unexpired(999));
        assert!(!c.is_unexpired(1_000));
        assert!(!c.is_unexpired(1_001));
    }

    #[test]
    fn from_payload_rejects_empty_or_malformed() {
        assert!(PluginCredential::from_payload(serde_json::json!({ "token": "" })).is_none());
        assert!(PluginCredential::from_payload(serde_json::json!({ "nope": 1 })).is_none());
        let ok = PluginCredential::from_payload(serde_json::json!({
            "token": "abc", "needs_token_auth_header": false, "owner_id": "o"
        }))
        .unwrap();
        assert_eq!(ok.token, "abc");
        assert!(!ok.needs_token_auth_header);
        assert_eq!(ok.owner_id.as_deref(), Some("o"));
    }

    // ── Targeted `start_oauth_flow` dispatch ────────────────────────────
    //
    // Every sidecar plugin auto-subscribes to `StartOauthFlow`, so an
    // unfiltered Intercept dispatch runs the first subscriber. When the user
    // picks one plugin's sign-in from `/login`, the seam must run ONLY that
    // plugin's handler. These tests exercise the real hooks dispatcher (not a
    // seam stub) so they actually prove the registry filtering.

    use std::sync::Mutex;
    use xai_grok_hooks::config::{HandlerType, HookSpec};
    use xai_grok_hooks::invoker::{PluginHookFuture, PluginHookRequest, PluginHookResponse};

    /// A plugin-handler `StartOauthFlow` spec for `plugin`, exactly as
    /// `sidecar_plugin_hook_specs` registers one per sidecar.
    fn oauth_spec(plugin: &str) -> HookSpec {
        spec_for(plugin, HookEventName::StartOauthFlow)
    }

    /// A plugin-handler spec for `plugin` on any credential `event`.
    fn spec_for(plugin: &str, event: HookEventName) -> HookSpec {
        HookSpec {
            name: format!("plugin/{plugin}/sidecar:{event}"),
            event,
            handler_type: HandlerType::Plugin,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            command: None,
            command_raw: None,
            url: None,
            url_raw: None,
            plugin: Some(plugin.to_string()),
            plugin_handler: None,
            timeout_ms: 300_000,
            source_dir: std::path::PathBuf::from("/tmp"),
            extra_env: std::collections::HashMap::new(),
        }
    }

    /// Records every invoked plugin name and mints a `tok-<plugin>` credential.
    /// Also stashes the last forwarded envelope so a test can assert what rode
    /// the wire (e.g. `ownerHint`).
    struct RecordingInvoker {
        seen: Arc<Mutex<Vec<String>>>,
        last_envelope: Arc<Mutex<Option<serde_json::Value>>>,
    }

    impl PluginHookInvoker for RecordingInvoker {
        fn invoke<'a>(&'a self, req: PluginHookRequest) -> PluginHookFuture<'a> {
            let plugin = req.plugin.clone();
            self.seen.lock().unwrap().push(plugin.clone());
            *self.last_envelope.lock().unwrap() = Some(req.payload.clone());
            Box::pin(async move {
                Ok(PluginHookResponse::Replace {
                    payload: Some(serde_json::json!({ "token": format!("tok-{plugin}") })),
                })
            })
        }
    }

    fn seam_with(registry: HookRegistry, seen: Arc<Mutex<Vec<String>>>) -> HookCredentialSeam {
        seam_recording(registry, seen, Arc::new(Mutex::new(None)))
    }

    fn seam_recording(
        registry: HookRegistry,
        seen: Arc<Mutex<Vec<String>>>,
        last_envelope: Arc<Mutex<Option<serde_json::Value>>>,
    ) -> HookCredentialSeam {
        HookCredentialSeam::new(
            registry,
            Arc::new(RecordingInvoker {
                seen,
                last_envelope,
            }),
            "sess".to_string(),
            "/tmp".to_string(),
            "/tmp".to_string(),
        )
    }

    /// A seam with a single `acme` subscriber for `event`, plus the handle that
    /// captures the envelope the plugin received.
    fn seam_capturing(event: HookEventName) -> (HookCredentialSeam, CapturedEnvelope) {
        let mut registry = HookRegistry::default();
        registry.append_specs(vec![spec_for("acme", event)]);
        let last = Arc::new(Mutex::new(None));
        let seam = seam_recording(registry, Arc::new(Mutex::new(Vec::new())), last.clone());
        (seam, last)
    }

    /// Handle onto the last envelope a plugin handler received.
    type CapturedEnvelope = Arc<Mutex<Option<serde_json::Value>>>;

    /// The envelope the plugin received, or a panic when nothing fired.
    fn captured(last: &CapturedEnvelope) -> serde_json::Value {
        last.lock()
            .unwrap()
            .clone()
            .expect("plugin handler was invoked")
    }

    // ── `auth_account` → `ownerHint` on all three events ─────────────────
    //
    // The account selector is what lets one sidecar plugin hold credentials for
    // several accounts of the same provider. These drive the real dispatcher so
    // they prove the value reaches the wire envelope, not just the seam.

    #[tokio::test]
    async fn resolve_forwards_account_as_owner_hint() {
        let (seam, last) = seam_capturing(HookEventName::ResolveCredential);
        seam.resolve("outbound", "https://example.test/v1", Some("work"))
            .await;
        assert_eq!(
            captured(&last).get("ownerHint").and_then(|v| v.as_str()),
            Some("work")
        );
    }

    #[tokio::test]
    async fn refresh_forwards_account_alongside_owner_id() {
        let (seam, last) = seam_capturing(HookEventName::RefreshCredential);
        seam.refresh(
            "unauthorized",
            Some("stale-owner"),
            "https://example.test/v1",
            Some("personal"),
        )
        .await;
        let envelope = captured(&last);
        // Two independent questions: whose token is stale vs. which account the
        // core wants back.
        assert_eq!(
            envelope.get("ownerId").and_then(|v| v.as_str()),
            Some("stale-owner")
        );
        assert_eq!(
            envelope.get("ownerHint").and_then(|v| v.as_str()),
            Some("personal")
        );
    }

    #[tokio::test]
    async fn start_oauth_flow_forwards_account_as_owner_hint() {
        let (seam, last) = seam_capturing(HookEventName::StartOauthFlow);
        seam.start_oauth_flow("sign_in", Some("acme"), Some("work"))
            .await;
        assert_eq!(
            captured(&last).get("ownerHint").and_then(|v| v.as_str()),
            Some("work")
        );
    }

    /// The `/login` path end to end: the account the user picked is advertised
    /// inside the ACP method id, parsed back out at `authenticate`, and reaches
    /// the plugin as `ownerHint`. Covers everything between the picker and the
    /// wire except the session-actor hop, which only moves the parsed value.
    #[tokio::test]
    async fn account_picked_in_login_reaches_the_plugin() {
        use crate::agent::auth_method::{parse_plugin_oauth_id, plugin_oauth_auth_method};

        let method = plugin_oauth_auth_method("acme", "Acme", Some("work"), Some("Work"));
        let (plugin, account) =
            parse_plugin_oauth_id(method.id()).expect("advertised id is a plugin-oauth id");
        assert_eq!((plugin, account), ("acme", Some("work")));

        let (seam, last) = seam_capturing(HookEventName::StartOauthFlow);
        seam.start_oauth_flow("sign_in", Some(plugin), account)
            .await;
        assert_eq!(
            captured(&last).get("ownerHint").and_then(|v| v.as_str()),
            Some("work"),
            "the picked account must select which credential the plugin mints"
        );
    }

    /// `None` = "the plugin's default account": no `ownerHint` key at all, so
    /// a plugin written against the pre-account wire sees an unchanged payload.
    #[tokio::test]
    async fn no_account_omits_owner_hint_on_every_event() {
        let (seam, last) = seam_capturing(HookEventName::ResolveCredential);
        seam.resolve("outbound", "https://example.test/v1", None)
            .await;
        assert!(captured(&last).get("ownerHint").is_none());

        let (seam, last) = seam_capturing(HookEventName::RefreshCredential);
        seam.refresh("unauthorized", None, "https://example.test/v1", None)
            .await;
        let envelope = captured(&last);
        assert!(envelope.get("ownerHint").is_none());
        assert!(envelope.get("ownerId").is_none());

        let (seam, last) = seam_capturing(HookEventName::StartOauthFlow);
        seam.start_oauth_flow("sign_in", Some("acme"), None).await;
        assert!(captured(&last).get("ownerHint").is_none());
    }

    #[tokio::test]
    async fn start_oauth_flow_targets_only_the_named_plugin() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut registry = HookRegistry::default();
        registry.append_specs(vec![oauth_spec("alpha"), oauth_spec("beta")]);
        let seam = seam_with(registry, seen.clone());

        let cred = seam.start_oauth_flow("sign_in", Some("beta"), None).await;

        assert_eq!(cred.map(|c| c.token), Some("tok-beta".to_string()));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["beta".to_string()],
            "only the targeted plugin's handler must run"
        );
    }

    #[tokio::test]
    async fn start_oauth_flow_missing_target_yields_none() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut registry = HookRegistry::default();
        registry.append_specs(vec![oauth_spec("alpha")]);
        let seam = seam_with(registry, seen.clone());

        // Targeting a plugin that isn't a subscriber runs nothing and returns
        // None, so the caller can report a helpful failure.
        let cred = seam.start_oauth_flow("sign_in", Some("ghost"), None).await;
        assert!(cred.is_none());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn start_oauth_flow_untargeted_runs_first_subscriber() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut registry = HookRegistry::default();
        registry.append_specs(vec![oauth_spec("alpha"), oauth_spec("beta")]);
        let seam = seam_with(registry, seen.clone());

        // `None` keeps the additive all-subscribers behavior: Intercept stops
        // at the first handler.
        let cred = seam.start_oauth_flow("sign_in", None, None).await;
        assert_eq!(cred.map(|c| c.token), Some("tok-alpha".to_string()));
        assert_eq!(*seen.lock().unwrap(), vec!["alpha".to_string()]);
    }
}
