//! Post-override facts about each catalog entry, for the `/providers` panel.
//!
//! The ACP model list the pager already holds carries only a thin `_meta`
//! (`totalContextTokens`, `agentType`, `provider`, `firstParty` and the
//! reasoning-effort menu). The fields whose *declared* and *resolved* values
//! diverge in practice — the slug that actually goes on the wire as `model`,
//! the endpoint, the wire format, the output ceiling — are not in it.
//!
//! Those arrive over `x.ai/models/resolved`, resolved by the agent that owns
//! the config. This module is only the index the panel reads them through:
//! catalog key to facts, and catalog key to the `[[provider]]` id that expanded
//! into it.
//!
//! It used to do the resolving as well, in this process, by loading
//! `config.toml` off the pager's own disk. That worked only because the pager
//! links the shell as a library and sits on the same machine as the config, and
//! a browser can do neither. The resolution moved to the shell; the facts type
//! is now that method's type, so the panel and the wire cannot drift apart into
//! two shapes.
//!
//! The deliberate limit is unchanged: entries that exist only in the
//! server-side prefetch (first-party models) have no local declaration, so they
//! are absent here and the panel renders them from ACP `_meta` alone rather
//! than inventing values.

use indexmap::IndexMap;
pub use xai_grok_shell::extensions::providers::{
    ResolvedCatalogResponse, ResolvedModelEntry, ResolvedModelFacts,
};

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

    /// Index one `x.ai/models/resolved` response. Declaration order is kept, so
    /// the panel groups providers in the order the config declares them.
    pub fn from_response(response: ResolvedCatalogResponse) -> Self {
        let mut entries = IndexMap::with_capacity(response.entries.len());
        let mut provider_of = IndexMap::new();
        for entry in response.entries {
            if let Some(provider) = entry.provider {
                provider_of.insert(entry.key.clone(), provider);
            }
            entries.insert(entry.key, entry.facts);
        }
        Self {
            entries,
            provider_of,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, provider: Option<&str>, wire_slug: &str) -> ResolvedModelEntry {
        ResolvedModelEntry {
            key: key.to_owned(),
            provider: provider.map(str::to_owned),
            facts: ResolvedModelFacts {
                wire_slug: wire_slug.to_owned(),
                ..ResolvedModelFacts::default()
            },
        }
    }

    #[test]
    fn indexes_facts_and_providers_by_key() {
        let catalog = ResolvedCatalog::from_response(ResolvedCatalogResponse {
            entries: vec![
                entry("acme/some-model", Some("acme"), "some-model-wire"),
                entry("standalone", None, "standalone-wire"),
            ],
        });
        assert_eq!(
            catalog.get("acme/some-model").expect("entry").wire_slug,
            "some-model-wire"
        );
        assert_eq!(catalog.provider_for("acme/some-model"), Some("acme"));
        assert!(catalog.get("standalone").is_some());
        assert_eq!(catalog.provider_for("standalone"), None);
        assert!(catalog.get("never-declared").is_none());
        assert!(!catalog.is_empty());
    }

    /// An empty answer is a normal answer: the shell could not read the config,
    /// or there is nothing config-declared to say. The panel still opens.
    #[test]
    fn an_empty_response_is_an_empty_catalog() {
        let catalog = ResolvedCatalog::from_response(ResolvedCatalogResponse::default());
        assert!(catalog.is_empty());
        assert!(catalog.get("anything").is_none());
    }

    /// Config declaration order is what the panel groups by, so indexing must
    /// not reshuffle it.
    #[test]
    fn declaration_order_survives_indexing() {
        let catalog = ResolvedCatalog::from_response(ResolvedCatalogResponse {
            entries: vec![
                entry("z/last", Some("z"), "last"),
                entry("a/first", Some("a"), "first"),
            ],
        });
        let keys: Vec<&str> = catalog.entries.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["z/last", "a/first"]);
    }
}
