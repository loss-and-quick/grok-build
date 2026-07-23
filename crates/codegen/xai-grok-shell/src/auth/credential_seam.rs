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
    /// specific target).
    async fn resolve(&self, reason: &str, base_url: &str) -> Option<PluginCredential>;

    /// Mint a fresh credential on a `401`/expiry. `owner_id` is the owner of the
    /// credential being refreshed, when known; `base_url` is the outbound
    /// endpoint the refreshed credential is destined for (see [`Self::resolve`]).
    async fn refresh(
        &self,
        reason: &str,
        owner_id: Option<&str>,
        base_url: &str,
    ) -> Option<PluginCredential>;

    /// Drive the whole interactive authorization flow and return the final
    /// credential. `reason` describes what triggered it (`missing_credential`,
    /// `sign_in`, …).
    async fn start_oauth_flow(&self, reason: &str) -> Option<PluginCredential>;
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
}

#[async_trait::async_trait]
impl PluginCredentialSeam for HookCredentialSeam {
    async fn resolve(&self, reason: &str, base_url: &str) -> Option<PluginCredential> {
        if !self.has_subscriber(HookEventName::ResolveCredential) {
            return None;
        }
        let envelope = self.envelope(
            HookEventName::ResolveCredential,
            HookPayload::ResolveCredential {
                reason: reason.to_string(),
                base_url: base_url.to_string(),
                owner_hint: None,
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

    async fn start_oauth_flow(&self, reason: &str) -> Option<PluginCredential> {
        if !self.has_subscriber(HookEventName::StartOauthFlow) {
            return None;
        }
        let envelope = self.envelope(
            HookEventName::StartOauthFlow,
            HookPayload::StartOauthFlow {
                reason: reason.to_string(),
                owner_hint: None,
            },
        );
        let value = dispatch_intercept(
            &self.registry,
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
}
