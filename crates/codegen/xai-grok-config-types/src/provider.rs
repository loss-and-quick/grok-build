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
use xai_grok_sampling_types::{ReasoningEffort, ReasoningEffortOption, ThinkingDialect};

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

/// Which request header a custom provider's credential rides in.
///
/// Maps 1:1 onto the sampler's `AuthScheme`
/// (crates/codegen/xai-grok-sampler/src/config.rs), but lives here so config
/// parsing does not depend on the sampler crate — the same inversion
/// [`ProviderFormat`] makes for `ApiBackend`.
///
/// Deliberately has no `Default`: the absence of a declaration means "use the
/// wire format's own scheme", which is a decision only the format table can
/// make, so "unset" must not collapse into one of these variants here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthScheme {
    /// `Authorization: Bearer <credential>`.
    Bearer,
    /// `x-api-key: <credential>` (Anthropic API keys).
    XApiKey,
    /// `x-goog-api-key: <credential>` (Google Gemini; kept out of the URL so it
    /// never lands in request logs).
    GoogleApiKey,
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
    /// Credential sent per [`Self::auth_scheme`], defaulting to the format's own
    /// scheme (Bearer / `x-api-key` / `x-goog-api-key`). May be a `$VAR` or
    /// `{file:/path}` reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Which header this provider's credential rides in, overriding the wire
    /// format's default.
    ///
    /// This exists because the auth scheme is a property of the *credential*,
    /// not only of the wire format. An Anthropic-format endpoint authenticated
    /// by an API key wants `x-api-key` — which is why that is the `messages`
    /// default — but the very same endpoint authenticated by an OAuth bearer (a
    /// subscription token minted by an OAuth flow, typically resolved by a
    /// credential plugin) is accepted *only* on `Authorization: Bearer`; sent as
    /// `x-api-key` it is by definition not a valid key and every request 401s,
    /// however often the token is refreshed. One wire format therefore has to be
    /// able to speak either scheme, chosen per provider entry.
    ///
    /// `None` (the default) keeps the format's own scheme, so nothing changes
    /// for a provider that does not declare one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<ProviderAuthScheme>,
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
    /// Maximum output tokens this provider's models may be asked to generate.
    ///
    /// Like the context window above and the effort menu below, an output
    /// ceiling is a property of the *endpoint*, not of the session, so every
    /// model this provider serves inherits the declaration.
    ///
    /// A `format = "messages"` provider has to declare one. The Messages API
    /// requires `max_tokens` on every request and rejects any value above the
    /// target model's own output limit — a limit that differs per model
    /// (`claude-opus-4-8` allows 128K output tokens, `claude-haiku-4-5` 64K),
    /// and one that nothing at request-build time can look up. Rather than
    /// guess a number that is wrong for some models, the sampler refuses to
    /// invent a ceiling and fails the build, so it must come from here — or
    /// from a `[model."<id>/<model>"]` table, which overrides this default for
    /// one model whose ceiling differs from its siblings'.
    ///
    /// The other three formats send it only when set, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
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
    /// Which `thinking` dialect this provider's models accept. Only the
    /// `messages` format has such a field; the other three ignore it.
    ///
    /// Spelled `thinking = "adaptive"`, `thinking = "off"`, or
    /// `thinking = { budget_tokens = 8000 }`. Like the context window, the
    /// output ceiling and the effort menu, the dialect describes the
    /// *endpoint*, so every model this provider serves inherits it; a
    /// `[model."<id>/<model>"]` table refines it for the one model that differs
    /// from its siblings.
    ///
    /// It has to be declared rather than derived because the accepted dialect
    /// varies by model generation and the wrong one is a hard rejection, and
    /// nothing downstream can classify an arbitrary slug: a gateway may serve
    /// any model under any name.
    ///
    /// `None` (the default) leaves the previous inference in place — adaptive,
    /// and only when an effort is requested — so a provider that does not
    /// declare one behaves exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingDialect>,
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
            auth_scheme = "bearer"
            proxy = "http://proxy.test:8080"
            models = ["m-large", "m-small"]
            context_window = 128000
            max_completion_tokens = 64000
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
        // A messages-format endpoint whose credential is an OAuth bearer says so
        // here; the format default alone would send it as `x-api-key`.
        assert_eq!(p.auth_scheme, Some(ProviderAuthScheme::Bearer));
        assert_eq!(p.proxy.as_deref(), Some("http://proxy.test:8080"));
        assert_eq!(p.models, vec!["m-large", "m-small"]);
        assert_eq!(p.context_window.map(|c| c.get()), Some(128000));
        assert_eq!(p.max_completion_tokens, Some(64000));
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
        // Unset = the wire format decides which header the credential rides in.
        assert!(p.auth_scheme.is_none());
        assert!(p.headers.is_empty());
        assert!(p.proxy.is_none());
        assert!(p.models.is_empty());
        assert!(p.context_window.is_none());
        // No declared output ceiling: a messages-format endpoint would have to
        // supply one, and the sampler says so rather than guessing.
        assert!(p.max_completion_tokens.is_none());
        // Absent selector = "the plugin's default account".
        assert!(p.auth_account.is_none());
        // No effort declaration = the control stays hidden, as before.
        assert!(p.reasoning_efforts.is_empty());
        assert!(p.reasoning_effort.is_none());
        assert!(!p.supports_reasoning_effort);
        // No declared dialect = the previous inference, so nothing changes for a
        // provider that does not declare one.
        assert!(p.thinking.is_none());
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

    /// Each spelling of the thinking dialect round-trips back to the same
    /// value, so a config the HM module generates re-parses unchanged.
    #[test]
    fn each_thinking_dialect_round_trips() {
        for (toml_value, expected) in [
            ("\"adaptive\"", ThinkingDialect::Adaptive),
            ("\"off\"", ThinkingDialect::Off),
            (
                "{ budget_tokens = 8000 }",
                ThinkingDialect::Budget {
                    budget_tokens: std::num::NonZeroU32::new(8000).unwrap(),
                },
            ),
        ] {
            let toml = format!(
                r#"
                id = "acme"
                format = "messages"
                base_url = "https://example.test/v1"
                models = ["m-large"]
                thinking = {toml_value}
            "#
            );
            let p: ProviderConfig = toml::from_str(&toml).unwrap();
            assert_eq!(p.thinking, Some(expected), "parsing {toml_value}");
            let round_tripped: ProviderConfig =
                toml::from_str(&toml::to_string(&p).unwrap()).unwrap();
            assert_eq!(
                round_tripped.thinking,
                Some(expected),
                "re-parsing {toml_value}"
            );
        }
    }

    /// A misspelled dialect fails at config parse, where the message can name
    /// the accepted spellings, rather than as an opaque 400 mid-session.
    #[test]
    fn rejects_unknown_thinking_dialect() {
        let toml = r#"
            id = "acme"
            base_url = "https://example.test/v1"
            thinking = "enabled"
        "#;
        let err = toml::from_str::<ProviderConfig>(toml)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("adaptive"),
            "message should list the spellings: {err}"
        );
    }

    /// The Messages API rejects a budget below 1024, so the floor is enforced
    /// where the number is written instead of surfacing as a request failure.
    #[test]
    fn rejects_thinking_budget_below_the_api_floor() {
        let toml = r#"
            id = "acme"
            base_url = "https://example.test/v1"
            thinking = { budget_tokens = 512 }
        "#;
        let err = toml::from_str::<ProviderConfig>(toml)
            .unwrap_err()
            .to_string();
        assert!(err.contains("1024"), "message should name the floor: {err}");
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

    /// The declaration is spelled the same way the sampler spells its own
    /// `AuthScheme`, so one table maps onto the other without translation of the
    /// wire strings.
    #[test]
    fn each_auth_scheme_round_trips() {
        for (s, want) in [
            ("bearer", ProviderAuthScheme::Bearer),
            ("x_api_key", ProviderAuthScheme::XApiKey),
            ("google_api_key", ProviderAuthScheme::GoogleApiKey),
        ] {
            let toml = format!(
                "id=\"i\"\nbase_url=\"https://e.test\"\nformat=\"messages\"\nauth_scheme=\"{s}\""
            );
            let p: ProviderConfig = toml::from_str(&toml).unwrap();
            assert_eq!(p.auth_scheme, Some(want));
        }
    }

    /// An unknown scheme is a config error rather than a silent fallback to the
    /// format default — a typo there would 401 at request time with nothing to
    /// point at.
    #[test]
    fn rejects_unknown_auth_scheme() {
        let toml = "id=\"i\"\nbase_url=\"https://e.test\"\nauth_scheme=\"x-api-key\"";
        assert!(toml::from_str::<ProviderConfig>(toml).is_err());
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
