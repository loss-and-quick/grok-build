//! `x.ai/models/resolved`: post-override facts about each catalog entry.
//!
//! `x.ai/models/list` carries the catalog every client draws from, but its
//! `_meta` is thin: `totalContextTokens`, `agentType`, `provider`, `firstParty`
//! and the reasoning-effort menu. The fields whose *declared* and *resolved*
//! values diverge in practice — the slug that actually goes on the wire as
//! `model`, the endpoint, the wire format, the output ceiling — are not in it.
//!
//! This method answers with those, by replaying [`resolve_model_list`] over the
//! effective config: `[[provider]]` expansion first, then any matching
//! `[model."…"]` table on top. That resolution used to run inside the pager,
//! which could do it because it links this crate as a library and sits on the
//! same disk as the config. A browser can do neither, so the answer now comes
//! from the process that owns the file.
//!
//! The read is from disk rather than from the agent's live `cfg`, which is what
//! the pager did and what the panel is for: the question being asked is "what
//! does my configuration say", and a config edited since launch should show up.
//!
//! ## What is deliberately absent
//!
//! Nothing here is a credential and nothing here is one step from being one:
//!
//! - Whether an entry carries its own key is a `bool`. The key, the env var
//!   name that would hold it, and the named auth provider that would mint it
//!   are not sent, in any form.
//! - `overrides` is a list of *field names* a `[model."…"]` table set. A table
//!   that sets `api_key`, `env_key`, `auth_provider` or `model_provider`
//!   contributes the single word `credentials` and no value.
//! - `endpoint` is host, port and path. A `base_url` carrying userinfo
//!   (`https://key@host`) or a query string (`?api_key=…`) loses both — on the
//!   parse path because a URL's host is only its host, and on the fallback path
//!   because [`endpoint_label`] strips them by hand rather than echoing a
//!   string it could not parse.

use agent_client_protocol as acp;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::{ExtResult, to_raw_response};
use crate::agent::config::{Config as AgentConfig, ConfigModelOverride, resolve_model_list};

/// What one catalog key resolved to, after `[[provider]]` expansion and any
/// `[model."<key>"]` table on top of it.
///
/// `#[serde(default)]`: a client one version ahead of its shell reads a
/// response missing whatever was added last, and gets the zero value for it
/// rather than a parse failure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
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
    /// "a per-model table overrode a provider-level value" signal. Names only.
    pub overrides: Vec<String>,
}

/// One catalog key and what it resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedModelEntry {
    /// Catalog key, the same one `x.ai/models/list` uses.
    pub key: String,
    /// The `[[provider]]` id that expanded into this key, if any. Recovered
    /// from the declaration rather than by splitting the key, so it still
    /// resolves when a per-model table changed the entry's slug and the key no
    /// longer ends with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub facts: ResolvedModelFacts,
}

/// Every config-declared catalog entry, in declaration order.
///
/// The deliberate limit, unchanged from where this used to live: entries that
/// exist only in the server-side prefetch (first-party models) have no local
/// declaration, so they are absent here and a panel renders them from
/// `x.ai/models/list` `_meta` alone rather than inventing values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResolvedCatalogResponse {
    pub entries: Vec<ResolvedModelEntry>,
}

pub async fn handle(args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "x.ai/models/resolved" => to_raw_response(&load()),
        _ => Err(acp::Error::method_not_found()),
    }
}

/// Load the effective config and resolve it. An unreadable or unparseable
/// config answers with an empty catalog rather than an error: a client asking
/// this question is usually asking it *because* the catalog looks wrong, and a
/// panel that refuses to open explains nothing.
fn load() -> ResolvedCatalogResponse {
    let Ok(raw) = crate::config::load_effective_config() else {
        return ResolvedCatalogResponse::default();
    };
    match AgentConfig::new_from_toml_cfg(&raw) {
        Ok(cfg) => resolve(&cfg),
        Err(_) => ResolvedCatalogResponse::default(),
    }
}

/// Resolve from an already-parsed agent config. Pure — no I/O, no network, no
/// prefetch — so what a panel shows comes from the same merge order the sampler
/// applies.
pub fn resolve(cfg: &AgentConfig) -> ResolvedCatalogResponse {
    let mut provider_of: IndexMap<String, String> = IndexMap::new();
    for provider in &cfg.providers {
        for slug in &provider.models {
            provider_of.insert(format!("{}/{slug}", provider.id), provider.id.clone());
        }
    }

    let resolved = resolve_model_list(cfg, None);
    let mut entries = Vec::with_capacity(resolved.len());
    for (key, entry) in &resolved {
        let info = entry.info();
        let from_provider_table = provider_of.contains_key(key);
        let model_table = cfg.config_models.get(key);
        entries.push(ResolvedModelEntry {
            key: key.clone(),
            provider: provider_of.get(key).cloned(),
            facts: ResolvedModelFacts {
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
        });
    }
    ResolvedCatalogResponse { entries }
}

/// Fields a `[model."<key>"]` table declares that a `[[provider]]` entry also
/// supplies (or that change what goes on the wire). Order is stable so a
/// rendered list does not reshuffle between frames.
///
/// Every credential-bearing key collapses to the one word `credentials`: which
/// of the four was used is a detail about a secret, and the thing worth showing
/// is that the table took the credential over from its provider.
fn declared_fields(table: &ConfigModelOverride) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    if table.model.is_some() {
        fields.push("model".to_owned());
    }
    if table.base_url.is_some() {
        fields.push("base_url".to_owned());
    }
    if table.api_backend.is_some() {
        fields.push("api_backend".to_owned());
    }
    if table.context_window.is_some() {
        fields.push("context_window".to_owned());
    }
    if table.max_completion_tokens.is_some() {
        fields.push("max_completion_tokens".to_owned());
    }
    if !table.reasoning_efforts.is_empty() {
        fields.push("reasoning_efforts".to_owned());
    }
    if table.reasoning_effort.is_some() {
        fields.push("reasoning_effort".to_owned());
    }
    if table.agent_type.is_some() {
        fields.push("agent_type".to_owned());
    }
    if !table.extra_headers.is_empty() {
        fields.push("extra_headers".to_owned());
    }
    if table.api_key.is_some()
        || table.env_key.is_some()
        || table.auth_provider.is_some()
        || table.model_provider.is_some()
    {
        fields.push("credentials".to_owned());
    }
    fields
}

/// Serde name of the resolved backend (`chat_completions`, `messages`, …).
/// Read through serde rather than matched, so a new variant renders as itself.
fn api_backend_label(backend: &impl Serialize) -> String {
    match serde_json::to_value(backend) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

/// Host of a base URL, keeping a path segment when it carries the API version
/// (`example.test/v1`) — the same `/v1` vs `/v1beta` distinction that decides
/// which wire format an endpoint answers on.
///
/// A URL that does not parse falls back to the raw string with userinfo, query
/// and fragment cut off. That trim is the point: a `base_url` is user-written
/// config and can carry a key in any of the three, and this value leaves the
/// process.
fn endpoint_label(base_url: &str) -> String {
    if base_url.is_empty() {
        return String::new();
    }
    let Ok(url) = url::Url::parse(base_url) else {
        return trim_secrets(base_url);
    };
    let Some(host) = url.host_str() else {
        return trim_secrets(base_url);
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

/// Drop everything a URL-shaped string can hide a secret in: the userinfo
/// before `@`, and the query and fragment after `?` or `#`.
fn trim_secrets(raw: &str) -> String {
    let without_tail = raw.split(['?', '#']).next().unwrap_or_default();
    match without_tail.rsplit_once('@') {
        Some((_, after)) => after.to_owned(),
        None => without_tail.to_owned(),
    }
}

#[cfg(test)]
#[path = "providers_tests.rs"]
mod tests;
