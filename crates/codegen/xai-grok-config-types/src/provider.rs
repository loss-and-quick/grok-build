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
        assert_eq!(p.headers.get("anthropic-version").map(String::as_str), Some("2023-06-01"));
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
