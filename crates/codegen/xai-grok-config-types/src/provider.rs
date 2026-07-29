//! Custom LLM-provider registry types.
//!
//! A `[[provider]]` entry declares an external inference endpoint once —
//! its wire format, base URL, credential, extra headers, optional proxy —
//! and lists the model slugs it serves. The shell expands each provider
//! into synthesized catalog entries (keyed `<provider_id>/<model>` plus the
//! bare slug) so the existing model-routing path handles per-provider
//! base URL / auth / headers with no parallel machinery.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
use xai_grok_sampling_types::{ReasoningEffort, ReasoningEffortOption};

/// Wire format a custom provider speaks. Maps 1:1 onto the sampler's
/// `ApiBackend`, but lives here so config parsing does not depend on the
/// sampler crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFormat {
    /// OpenAI Chat Completions (`/chat/completions`).
    #[default]
    ChatCompletions,
    /// OpenAI Responses (`/responses`).
    Responses,
    /// Anthropic Messages (`/messages`).
    Messages,
    /// Google Gemini (`/models/<model>:streamGenerateContent`).
    Gemini,
}

/// A single `[[provider]]` registry entry.
///
/// `id` disambiguates when the same bare model slug is served by more than
/// one provider: selecting `<id>/<model>` forces this provider, while the
/// bare `<model>` resolves to whichever provider last claimed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Stable identifier, used as the `<id>/` routing prefix.
    pub id: String,
    /// Wire format spoken by this provider.
    #[serde(default)]
    pub format: ProviderFormat,
    /// Endpoint base URL, e.g. `https://example.test/v1`.
    pub base_url: String,
    /// Credential sent per the format's auth scheme (Bearer / `x-api-key` /
    /// `x-goog-api-key`). May be a `$VAR` or `{file:/path}` reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Extra request headers applied verbatim (values may be secret refs).
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub headers: IndexMap<String, String>,
    /// Per-provider HTTP(S) proxy URL. Overrides any `HTTP(S)_PROXY` env.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// Bare model slugs this provider serves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Default context window for this provider's models when a model does
    /// not otherwise supply one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<NonZeroU64>,
    /// Which of a credential plugin's accounts this provider's models
    /// authenticate as. Rides the plugin credential seam as the `ownerHint` on
    /// `resolve_credential` / `refresh_credential` / `start_oauth_flow`, so one
    /// sidecar plugin can hold several accounts for the same provider and two
    /// `[[provider]]` entries sharing a `base_url` can name different ones.
    /// `None` means "the plugin's default account".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_account: Option<String>,
    /// Reasoning-effort menu this provider's models offer (source of truth).
    /// Each element is either a bare canonical value (`"xhigh"`) or a table
    /// with `value` plus optional `id` / `label` / `description` / `default`,
    /// so the common case is `reasoning_efforts = ["low", "high"]`.
    ///
    /// A non-empty list implies `supports_reasoning_effort` and — absent an
    /// explicit `reasoning_effort` — supplies the default (the `default = true`
    /// entry, else the first). That is the same derivation the per-model
    /// catalog applies, so a provider author sets one field, not three.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<ReasoningEffortOption>,
    /// Effort sent when the session has not picked one. Set it to pin a
    /// default that is not the menu's own; leave it unset to derive from
    /// `reasoning_efforts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Escape hatch for an endpoint that accepts an effort but whose menu you
    /// do not want to enumerate: expose the control with the client's fallback
    /// list. Implied by a non-empty `reasoning_efforts`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_reasoning_effort: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_fields() {
        let toml = r#"
            id = "acme"
            format = "messages"
            base_url = "https://example.test/v1"
            api_key = "secret-token"
            proxy = "http://proxy.test:8080"
            models = ["m-large", "m-small"]
            context_window = 128000
            auth_account = "work"
            [headers]
            anthropic-version = "2023-06-01"
            x-extra = "on"
        "#;
        let p: ProviderConfig = toml::from_str(toml).unwrap();
        assert_eq!(p.id, "acme");
        assert_eq!(p.format, ProviderFormat::Messages);
        assert_eq!(p.base_url, "https://example.test/v1");
        assert_eq!(p.api_key.as_deref(), Some("secret-token"));
        assert_eq!(p.proxy.as_deref(), Some("http://proxy.test:8080"));
        assert_eq!(p.models, vec!["m-large", "m-small"]);
        assert_eq!(p.context_window.map(|c| c.get()), Some(128000));
        assert_eq!(p.auth_account.as_deref(), Some("work"));
        assert_eq!(
            p.headers.get("anthropic-version").map(String::as_str),
            Some("2023-06-01")
        );
        assert_eq!(p.headers.get("x-extra").map(String::as_str), Some("on"));
    }

    #[test]
    fn applies_defaults() {
        let toml = r#"
            id = "minimal"
            base_url = "https://example.test/v1"
        "#;
        let p: ProviderConfig = toml::from_str(toml).unwrap();
        assert_eq!(p.format, ProviderFormat::ChatCompletions);
        assert!(p.api_key.is_none());
        assert!(p.headers.is_empty());
        assert!(p.proxy.is_none());
        assert!(p.models.is_empty());
        assert!(p.context_window.is_none());
        // Absent selector = "the plugin's default account".
        assert!(p.auth_account.is_none());
        // No effort declaration = the control stays hidden, as before.
        assert!(p.reasoning_efforts.is_empty());
        assert!(p.reasoning_effort.is_none());
        assert!(!p.supports_reasoning_effort);
    }

    /// The common declaration: a bare array of canonical values. Each entry
    /// gets an id/label for free, and nothing else has to be written.
    #[test]
    fn parses_bare_reasoning_effort_list() {
        let toml = r#"
            id = "acme"
            base_url = "https://example.test/v1"
            models = ["m-large"]
            reasoning_efforts = ["low", "high", "xhigh"]
        "#;
        let p: ProviderConfig = toml::from_str(toml).unwrap();
        let values: Vec<_> = p.reasoning_efforts.iter().map(|o| o.value).collect();
        assert_eq!(
            values,
            vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Xhigh
            ]
        );
        assert_eq!(p.reasoning_efforts[2].id, "xhigh");
        assert_eq!(p.reasoning_efforts[2].label, "Xhigh");
        // Derivation happens at catalog build; the raw config keeps the two
        // legacy fields absent so "unset" stays distinguishable from "false".
        assert!(p.reasoning_effort.is_none());
        assert!(!p.supports_reasoning_effort);
    }

    /// The table form carries presentation and a menu default; both spellings
    /// may be mixed in one list.
    #[test]
    fn parses_table_reasoning_effort_entries() {
        let toml = r#"
            id = "acme"
            base_url = "https://example.test/v1"
            models = ["m-large"]
            reasoning_efforts = [
                "low",
                { value = "high", id = "deep", label = "Deep think", description = "slow", default = true },
            ]
        "#;
        let p: ProviderConfig = toml::from_str(toml).unwrap();
        assert_eq!(p.reasoning_efforts.len(), 2);
        assert!(!p.reasoning_efforts[0].default);
        let deep = &p.reasoning_efforts[1];
        assert_eq!(deep.value, ReasoningEffort::High);
        assert_eq!(deep.id, "deep");
        assert_eq!(deep.label, "Deep think");
        assert_eq!(deep.description.as_deref(), Some("slow"));
        assert!(deep.default);
    }

    /// A provider that accepts an effort but publishes no menu declares the
    /// gate (and optionally the default) directly.
    #[test]
    fn parses_menuless_reasoning_effort_declaration() {
        let toml = r#"
            id = "acme"
            base_url = "https://example.test/v1"
            models = ["m-large"]
            supports_reasoning_effort = true
            reasoning_effort = "medium"
        "#;
        let p: ProviderConfig = toml::from_str(toml).unwrap();
        assert!(p.supports_reasoning_effort);
        assert_eq!(p.reasoning_effort, Some(ReasoningEffort::Medium));
        assert!(p.reasoning_efforts.is_empty());
    }

    /// Two entries pointing at the same plugin-backed endpoint stay distinct
    /// accounts: the `<id>/` prefix disambiguates the routing key and
    /// `auth_account` disambiguates the credential.
    #[test]
    fn same_base_url_entries_carry_distinct_accounts() {
        let toml = r#"
            [[provider]]
            id = "acme-work"
            base_url = "https://example.test/v1"
            models = ["m-large"]
            auth_account = "work"

            [[provider]]
            id = "acme-personal"
            base_url = "https://example.test/v1"
            models = ["m-large"]
            auth_account = "personal"
        "#;
        #[derive(Deserialize)]
        struct Root {
            provider: Vec<ProviderConfig>,
        }
        let root: Root = toml::from_str(toml).unwrap();
        assert_eq!(root.provider[0].base_url, root.provider[1].base_url);
        assert_eq!(root.provider[0].auth_account.as_deref(), Some("work"));
        assert_eq!(root.provider[1].auth_account.as_deref(), Some("personal"));
    }

    #[test]
    fn parses_provider_array() {
        let toml = r#"
            [[provider]]
            id = "a"
            base_url = "https://a.test/v1"
            models = ["x"]

            [[provider]]
            id = "b"
            format = "gemini"
            base_url = "https://b.test/v1"
            models = ["y"]
        "#;
        #[derive(Deserialize)]
        struct Root {
            provider: Vec<ProviderConfig>,
        }
        let root: Root = toml::from_str(toml).unwrap();
        assert_eq!(root.provider.len(), 2);
        assert_eq!(root.provider[0].id, "a");
        assert_eq!(root.provider[1].format, ProviderFormat::Gemini);
    }

    #[test]
    fn each_format_round_trips() {
        for (s, want) in [
            ("chat_completions", ProviderFormat::ChatCompletions),
            ("responses", ProviderFormat::Responses),
            ("messages", ProviderFormat::Messages),
            ("gemini", ProviderFormat::Gemini),
        ] {
            let toml = format!("id=\"i\"\nbase_url=\"https://e.test\"\nformat=\"{s}\"");
            let p: ProviderConfig = toml::from_str(&toml).unwrap();
            assert_eq!(p.format, want);
        }
    }
}
