//! Serving the settings catalog, its values and its locks to any client.
//!
//! ## Where the catalog comes from
//!
//! The catalog is declared once, as the pager's registry, and generated from
//! there into `sdk/settings/src/generated/catalog.json` by
//! `xai_grok_pager::settings::wire`. The shell embeds that file and serves it
//! verbatim.
//!
//! The declaration did not move into this crate because the shell cannot see
//! the pager (the dependency runs the other way, and moving fifty rows and the
//! terminal enums they name would drag a renderer in with them). Embedding the
//! generated artifact gets the same guarantee for a fraction of the risk: a
//! setting that exists but has not been regenerated fails the pager's own test
//! before it can reach a second client as a row that quietly is not there.
//!
//! ## What is resolved here instead of baked in
//!
//! Values and locks, because both are runtime facts.
//!
//! - **Values** are the *persisted* ones: what `config.toml` says, with the
//!   catalog's declared default standing in for anything unset. A client's own
//!   live state still wins in its own view — the pager shows the permission
//!   mode of the running agent, not the one on disk — but that is the client
//!   overlaying what it already knows, not the client inventing what it does
//!   not.
//! - **Locks** are carried, never recomputed. `ReadOnlyConfig` is a `stat` of a
//!   file on this machine; a browser cannot perform it, and a client that
//!   guessed would offer an edit this process is about to refuse.

pub mod state;

use std::sync::OnceLock;

use xai_grok_settings_types::SettingsCatalog;

/// The generated catalog, as checked in.
///
/// `include_str!` rather than a read at startup: the file is part of this
/// binary's contract with every other client, and a deployment that lost it
/// should fail to build rather than to answer.
const CATALOG_JSON: &str = include_str!("../../../../../sdk/settings/src/generated/catalog.json");

/// The catalog, parsed once.
///
/// Parsing is not skipped in favour of forwarding the bytes: a malformed
/// artifact should be one panic here, in a test run, rather than a parse error
/// in every client that asks.
pub fn catalog() -> &'static SettingsCatalog {
    static PARSED: OnceLock<SettingsCatalog> = OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str(CATALOG_JSON).expect("the generated settings catalog parses")
    })
}

/// The catalog as it goes on the wire.
pub fn catalog_json() -> &'static str {
    CATALOG_JSON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_catalog_parses_and_is_not_empty() {
        let catalog = catalog();
        assert_eq!(catalog.version, xai_grok_settings_types::CATALOG_VERSION);
        assert!(
            !catalog.rows.is_empty(),
            "the generated catalog carries no rows"
        );
        assert!(!catalog.categories.is_empty());
    }

    #[test]
    fn every_row_has_a_unique_key() {
        let mut keys: Vec<&str> = catalog().rows.iter().map(|r| r.key.as_str()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "the catalog carries a duplicate key");
    }
}
