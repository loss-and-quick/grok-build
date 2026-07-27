use serde::{Deserialize, Serialize};

/// Access gate from `grok_build_access_gate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateInfo {
    pub message: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

/// Typed auth metadata passed from the shell to the pager via ACP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMeta {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub auth_mode: Option<String>,
    /// `true` when the active session credential is a first-party xAI account
    /// login: a grok.com web login, an OIDC login (including enterprise
    /// issuers), or an external auth provider that declared an xAI issuer.
    ///
    /// Surfaced so the pager can scope grok.com account features (tier gates,
    /// usage/billing surface, subscription upsell) to accounts that actually
    /// have one. A plain API key is BYOK, and a plugin-OAuth credential
    /// belongs to some other vendor — neither is a grok.com account, so those
    /// gates must not apply. Mirrors [`crate::auth::GrokAuth::is_session_auth`];
    /// absent (`false`) when there is no current credential.
    ///
    /// Deliberately *not* `GrokAuth::is_xai_auth`: that one is `false` for
    /// `AuthMode::WebLogin`, which is exactly the grok.com account the gates
    /// exist for.
    #[serde(default)]
    pub is_first_party_account: bool,
    /// Team principal UUID when the session is a team login (`None` for personal).
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
    #[serde(default)]
    pub is_zdr: bool,
    #[serde(default)]
    pub team_role: Option<String>,
    /// Defaults to opted-out (safer) until auth meta is populated.
    #[serde(default = "crate::auth::default_coding_data_retention_opt_out")]
    pub coding_data_retention_opt_out: bool,
    #[serde(default)]
    pub show_resolved_model: Option<bool>,
    /// `Some` = user is blocked; `None` = user has access.
    #[serde(default)]
    pub gate: Option<GateInfo>,
    /// User-friendly display name for the current subscription tier
    /// (e.g. "SuperGrok Heavy", "X Premium", "Free"). From CCP `/settings`.
    #[serde(default)]
    pub subscription_tier: Option<String>,
}

impl Default for AuthMeta {
    fn default() -> Self {
        Self {
            email: None,
            auth_mode: None,
            is_first_party_account: false,
            team_id: None,
            team_name: None,
            is_zdr: false,
            team_role: None,
            coding_data_retention_opt_out: crate::auth::default_coding_data_retention_opt_out(),
            show_resolved_model: None,
            gate: None,
            subscription_tier: None,
        }
    }
}
