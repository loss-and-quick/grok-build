//! Post-override facts about each catalog entry, for the `/providers` panel.
//!
//! The ACP model list the pager already holds carries only a thin `_meta`
//! (`totalContextTokens`, `agentType`, `provider`, `firstParty` and the
//! reasoning-effort menu). The fields whose *declared* and *resolved* values
//! diverge in practice — the slug that actually goes on the wire as `model`,
//! the endpoint, the wire format, the output ceiling — never reach it.
//!
//! Rather than block on widening that `_meta`, this module re-runs the shell's
//! own catalog resolution in-process over the same effective config the shell
//! was launched with. It is a read-only call into
//! [`xai_grok_shell::agent::config::resolve_model_list`] with no prefetch, so
//! what comes back is exactly what that function produces for every
//! config-declared entry: `[[provider]]` expansion applied, then the matching
//! `[model."…"]` table applied over it.
//!
//! The deliberate limit: entries that exist only in the server-side prefetch
//! (first-party models) have no local declaration, so they are absent here and
//! the panel renders them from ACP `_meta` alone rather than inventing values.

use indexmap::IndexMap;
use xai_grok_shell::agent::config::{
    Config as AgentConfig, ConfigModelOverride, resolve_model_list,
};

/// What one catalog key resolved to, after `[[provider]]` expansion and any
/// `[model."<key>"]` table on top of it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedModelFacts {
    /// The slug actually sent as the request's `model` field.
    pub wire_slug: String,
    /// Host (and path, when it carries an API version) of the resolved endpoint.
    pub endpoint: String,
    /// Wire format the sampler speaks to this entry, e.g. `chat_completions`.
    pub api_backend: String,
    /// Resolved context window in tokens.
    pub context_window: u64,
    /// Resolved output ceiling, when one is declared.
    pub max_output_tokens: Option<u32>,
    /// Resolved agent type.
    pub agent_type: String,
    /// Whether the entry carries its own credential (static key, env key, or a
    /// named auth provider). Presence only — never the value.
    pub own_credentials: bool,
    /// The entry was synthesized by a `[[provider]]` expansion.
    pub from_provider_table: bool,
    /// A `[model."<key>"]` table applies to the entry.
    pub from_model_table: bool,
    /// Fields the `[model."<key>"]` table set on top of a `[[provider]]`
    /// expansion. Non-empty only when both provenances apply — this is the
    /// "a per-model table overrode a provider-level value" signal.
    pub overrides: Vec<&'static str>,
}

/// Locally resolved facts for every config-declared catalog entry, keyed by
/// catalog key.
#[derive(Debug, Clone, Default)]
pub struct ResolvedCatalog {
    entries: IndexMap<String, ResolvedModelFacts>,
    /// Provider id per key it expanded into, from `[[provider]] models`.
    provider_of: IndexMap<String, String>,
}

impl ResolvedCatalog {
    /// Facts for a catalog key, or `None` when the key is not config-declared
    /// (a prefetch-only entry).
    pub fn get(&self, key: &str) -> Option<&ResolvedModelFacts> {
        self.entries.get(key)
    }

    /// The `[[provider]]` id that expanded into this key, if any. Recovered
    /// from the declaration rather than by splitting the key, so it still
    /// resolves when a per-model table changed the entry's slug and the key no
    /// longer ends with it.
    pub fn provider_for(&self, key: &str) -> Option<&str> {
        self.provider_of.get(key).map(String::as_str)
    }

    /// Whether anything was resolved at all. An empty catalog means the panel
    /// falls back to ACP `_meta` for every row.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolve from an already-parsed agent config. Pure — no I/O, no network,
    /// no prefetch — so the panel's values come from the same merge order the
    /// shell applies.
    pub fn from_agent_config(cfg: &AgentConfig) -> Self {
        let mut provider_of = IndexMap::new();
        for provider in &cfg.providers {
            for slug in &provider.models {
                provider_of.insert(format!("{}/{slug}", provider.id), provider.id.clone());
            }
        }

        let resolved = resolve_model_list(cfg, None);
        let mut entries = IndexMap::with_capacity(resolved.len());
        for (key, entry) in &resolved {
            let info = entry.info();
            let from_provider_table = provider_of.contains_key(key);
            let model_table = cfg.config_models.get(key);
            entries.insert(
                key.clone(),
                ResolvedModelFacts {
                    wire_slug: info.model.clone(),
                    endpoint: endpoint_label(&info.base_url),
                    api_backend: api_backend_label(&info.api_backend),
                    context_window: info.context_window.get(),
                    max_output_tokens: info.max_completion_tokens,
                    agent_type: info.agent_type.clone(),
                    own_credentials: entry.has_own_credentials(),
                    from_provider_table,
                    from_model_table: model_table.is_some(),
                    overrides: match (from_provider_table, model_table) {
                        (true, Some(table)) => declared_fields(table),
                        _ => Vec::new(),
                    },
                },
            );
        }
        Self {
            entries,
            provider_of,
        }
    }

    /// Load the effective config from disk and resolve it. Returns an empty
    /// catalog when the config cannot be read or parsed — the panel then
    /// degrades to ACP `_meta` rather than refusing to open.
    ///
    /// Blocking file I/O; call it on an explicit user action (opening the
    /// panel), not from the render path.
    pub fn load() -> Self {
        let Ok(raw) = xai_grok_shell::config::load_effective_config() else {
            return Self::default();
        };
        match AgentConfig::new_from_toml_cfg(&raw) {
            Ok(cfg) => Self::from_agent_config(&cfg),
            Err(_) => Self::default(),
        }
    }
}

/// Fields a `[model."<key>"]` table declares that a `[[provider]]` entry also
/// supplies (or that change what goes on the wire). Order is stable so the
/// rendered list does not reshuffle between frames.
fn declared_fields(table: &ConfigModelOverride) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if table.model.is_some() {
        fields.push("model");
    }
    if table.base_url.is_some() {
        fields.push("base_url");
    }
    if table.api_backend.is_some() {
        fields.push("api_backend");
    }
    if table.context_window.is_some() {
        fields.push("context_window");
    }
    if table.max_completion_tokens.is_some() {
        fields.push("max_completion_tokens");
    }
    if !table.reasoning_efforts.is_empty() {
        fields.push("reasoning_efforts");
    }
    if table.reasoning_effort.is_some() {
        fields.push("reasoning_effort");
    }
    if table.agent_type.is_some() {
        fields.push("agent_type");
    }
    if !table.extra_headers.is_empty() {
        fields.push("extra_headers");
    }
    if table.api_key.is_some()
        || table.env_key.is_some()
        || table.auth_provider.is_some()
        || table.model_provider.is_some()
    {
        fields.push("credentials");
    }
    fields
}

/// Serde name of the resolved backend (`chat_completions`, `messages`, …).
/// Read through serde rather than matched, so a new variant renders as itself.
fn api_backend_label(backend: &impl serde::Serialize) -> String {
    match serde_json::to_value(backend) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

/// Host of a base URL, keeping a path segment when it carries the API version
/// (`example.test/v1`) — the same `/v1` vs `/v1beta` distinction that decides
/// which wire format an endpoint answers on. Falls back to the raw string when
/// the URL does not parse.
fn endpoint_label(base_url: &str) -> String {
    if base_url.is_empty() {
        return String::new();
    }
    let Ok(url) = url::Url::parse(base_url) else {
        return base_url.to_string();
    };
    let Some(host) = url.host_str() else {
        return base_url.to_string();
    };
    let mut label = host.to_string();
    if let Some(port) = url.port() {
        label.push(':');
        label.push_str(&port.to_string());
    }
    let path = url.path().trim_end_matches('/');
    if !path.is_empty() {
        label.push_str(path);
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_from(toml_src: &str) -> AgentConfig {
        let raw: toml::Value = toml::from_str(toml_src).expect("fixture parses");
        AgentConfig::new_from_toml_cfg(&raw).expect("fixture builds a config")
    }

    /// A `[[provider]]` entry expands one key per listed slug, and the resolved
    /// facts come from the provider's own declaration.
    #[test]
    fn provider_expansion_resolves_endpoint_format_and_ceiling() {
        let cfg = config_from(
            r#"
            [[provider]]
            id = "acme"
            format = "messages"
            base_url = "https://api.example.test/v1"
            models = ["some-model", "other-model"]
            context_window = 1000000
            max_completion_tokens = 64000
            "#,
        );
        let catalog = ResolvedCatalog::from_agent_config(&cfg);

        let facts = catalog.get("acme/some-model").expect("expanded entry");
        assert_eq!(facts.wire_slug, "some-model");
        assert_eq!(facts.endpoint, "api.example.test/v1");
        assert_eq!(facts.api_backend, "messages");
        assert_eq!(facts.context_window, 1_000_000);
        assert_eq!(facts.max_output_tokens, Some(64_000));
        assert!(facts.from_provider_table);
        assert!(!facts.from_model_table);
        assert!(facts.overrides.is_empty());
        assert_eq!(catalog.provider_for("acme/some-model"), Some("acme"));
        assert!(catalog.get("acme/other-model").is_some());
    }

    /// The bug this panel exists for: a per-model table renames the wire slug,
    /// so the catalog key is no longer what reaches the vendor.
    #[test]
    fn per_model_table_overriding_provider_values_is_recorded() {
        let cfg = config_from(
            r#"
            [[provider]]
            id = "acme"
            format = "messages"
            base_url = "https://api.example.test/v1"
            models = ["some-model"]
            context_window = 256000
            max_completion_tokens = 8000

            [model."acme/some-model"]
            model = "some-model-wire-2"
            context_window = 1000000
            max_completion_tokens = 64000
            "#,
        );
        let facts = ResolvedCatalog::from_agent_config(&cfg)
            .get("acme/some-model")
            .cloned()
            .expect("expanded entry survives the per-model table");

        // Resolved, not declared: the provider's 256k/8k lost to the table.
        assert_eq!(facts.wire_slug, "some-model-wire-2");
        assert_eq!(facts.context_window, 1_000_000);
        assert_eq!(facts.max_output_tokens, Some(64_000));
        // Endpoint and format still come from the provider entry.
        assert_eq!(facts.endpoint, "api.example.test/v1");
        assert_eq!(facts.api_backend, "messages");
        assert!(facts.from_provider_table && facts.from_model_table);
        assert_eq!(
            facts.overrides,
            vec!["model", "context_window", "max_completion_tokens"]
        );
    }

    /// A per-model `api_backend` on a provider-expanded key does apply, and the
    /// panel says so — the belief that it did not once cost an investigation.
    #[test]
    fn per_model_api_backend_override_applies_and_is_listed() {
        let cfg = config_from(
            r#"
            [[provider]]
            id = "acme"
            format = "chat_completions"
            base_url = "https://api.example.test/v1"
            models = ["some-model"]

            [model."acme/some-model"]
            api_backend = "responses"
            "#,
        );
        let facts = ResolvedCatalog::from_agent_config(&cfg)
            .get("acme/some-model")
            .cloned()
            .expect("expanded entry");
        assert_eq!(facts.api_backend, "responses");
        assert_eq!(facts.overrides, vec!["api_backend"]);
    }

    /// A standalone `[model.…]` table with no `[[provider]]` behind it is not
    /// an override of anything, so nothing is listed as overridden.
    #[test]
    fn standalone_model_table_lists_no_overrides() {
        let cfg = config_from(
            r#"
            [model."some-model"]
            model = "some-model-wire"
            base_url = "https://api.example.test/v1"
            context_window = 200000
            "#,
        );
        let catalog = ResolvedCatalog::from_agent_config(&cfg);
        let facts = catalog.get("some-model").expect("config-declared entry");
        assert_eq!(facts.wire_slug, "some-model-wire");
        assert!(!facts.from_provider_table);
        assert!(facts.from_model_table);
        assert!(facts.overrides.is_empty());
        assert_eq!(catalog.provider_for("some-model"), None);
    }

    /// Credential presence is reported, never the credential.
    #[test]
    fn own_credentials_is_presence_only() {
        let cfg = config_from(
            r#"
            [[provider]]
            id = "acme"
            base_url = "https://api.example.test/v1"
            api_key = "not-a-real-key"
            models = ["some-model"]

            [[provider]]
            id = "example-provider"
            base_url = "https://api.example.test/v1"
            models = ["some-model"]
            "#,
        );
        let catalog = ResolvedCatalog::from_agent_config(&cfg);
        assert!(catalog.get("acme/some-model").unwrap().own_credentials);
        assert!(
            !catalog
                .get("example-provider/some-model")
                .unwrap()
                .own_credentials
        );
        let rendered = format!("{catalog:?}");
        assert!(
            !rendered.contains("not-a-real-key"),
            "resolved facts must not carry the credential"
        );
    }

    #[test]
    fn endpoint_label_keeps_version_path_and_port() {
        assert_eq!(
            endpoint_label("https://api.example.test/v1"),
            "api.example.test/v1"
        );
        assert_eq!(
            endpoint_label("https://api.example.test/v1beta/"),
            "api.example.test/v1beta"
        );
        assert_eq!(
            endpoint_label("http://127.0.0.1:8080/v1"),
            "127.0.0.1:8080/v1"
        );
        assert_eq!(endpoint_label(""), "");
        assert_eq!(endpoint_label("not a url"), "not a url");
    }
}
