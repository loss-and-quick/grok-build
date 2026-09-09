//! Tests for `x.ai/models/resolved`.
//!
//! Everything resolves from a config literal, so no case reads the developer's
//! own `config.toml` or needs a grok home to exist.

use super::*;

fn config_from(toml_src: &str) -> AgentConfig {
    let raw: toml::Value = toml::from_str(toml_src).expect("fixture parses");
    AgentConfig::new_from_toml_cfg(&raw).expect("fixture builds a config")
}

fn facts_for(response: &ResolvedCatalogResponse, key: &str) -> ResolvedModelFacts {
    response
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .unwrap_or_else(|| panic!("no entry for {key}"))
        .facts
        .clone()
}

fn provider_for<'a>(response: &'a ResolvedCatalogResponse, key: &str) -> Option<&'a str> {
    response
        .entries
        .iter()
        .find(|entry| entry.key == key)?
        .provider
        .as_deref()
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
    let response = resolve(&cfg);

    let facts = facts_for(&response, "acme/some-model");
    assert_eq!(facts.wire_slug, "some-model");
    assert_eq!(facts.endpoint, "api.example.test/v1");
    assert_eq!(facts.api_backend, "messages");
    assert_eq!(facts.context_window, 1_000_000);
    assert_eq!(facts.max_output_tokens, Some(64_000));
    assert!(facts.from_provider_table);
    assert!(!facts.from_model_table);
    assert!(facts.overrides.is_empty());
    assert_eq!(provider_for(&response, "acme/some-model"), Some("acme"));
    assert!(response.entries.iter().any(|e| e.key == "acme/other-model"));
}

/// The bug this panel exists for: a per-model table renames the wire slug, so
/// the catalog key is no longer what reaches the vendor.
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
    let facts = facts_for(&resolve(&cfg), "acme/some-model");

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
    let facts = facts_for(&resolve(&cfg), "acme/some-model");
    assert_eq!(facts.api_backend, "responses");
    assert_eq!(facts.overrides, vec!["api_backend"]);
}

/// A standalone `[model.…]` table with no `[[provider]]` behind it is not an
/// override of anything, so nothing is listed as overridden.
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
    let response = resolve(&cfg);
    let facts = facts_for(&response, "some-model");
    assert_eq!(facts.wire_slug, "some-model-wire");
    assert!(!facts.from_provider_table);
    assert!(facts.from_model_table);
    assert!(facts.overrides.is_empty());
    assert_eq!(provider_for(&response, "some-model"), None);
}

/// Credential presence is reported, never the credential — and now that this
/// answer leaves the process, the whole serialized response is checked, not
/// just the struct.
#[test]
fn own_credentials_is_presence_only_and_no_key_reaches_the_wire() {
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
    let response = resolve(&cfg);
    assert!(facts_for(&response, "acme/some-model").own_credentials);
    assert!(!facts_for(&response, "example-provider/some-model").own_credentials);

    let json = serde_json::to_string(&response).expect("serialize");
    assert!(
        !json.contains("not-a-real-key"),
        "the resolved catalog must not carry the credential"
    );
}

/// The env var that would hold a key is a name, but it is a name that says
/// where the secret is; a per-model table that sets one contributes the word
/// `credentials` and nothing else.
#[test]
fn credential_bearing_keys_collapse_to_one_word() {
    let cfg = config_from(
        r#"
        [[provider]]
        id = "acme"
        base_url = "https://api.example.test/v1"
        models = ["some-model"]

        [model."acme/some-model"]
        env_key = "ACME_SECRET_TOKEN"
        auth_provider = "acme-helper"
        "#,
    );
    let response = resolve(&cfg);
    assert_eq!(
        facts_for(&response, "acme/some-model").overrides,
        vec!["credentials"]
    );
    let json = serde_json::to_string(&response).expect("serialize");
    assert!(!json.contains("ACME_SECRET_TOKEN"));
    assert!(!json.contains("acme-helper"));
}

/// `extra_headers` is where a hand-rolled `Authorization:` ends up. The field
/// name crosses; the header values do not.
#[test]
fn extra_header_values_never_reach_the_wire() {
    let cfg = config_from(
        r#"
        [[provider]]
        id = "acme"
        base_url = "https://api.example.test/v1"
        models = ["some-model"]

        [model."acme/some-model".extra_headers]
        Authorization = "Bearer not-a-real-key"
        "#,
    );
    let response = resolve(&cfg);
    assert_eq!(
        facts_for(&response, "acme/some-model").overrides,
        vec!["extra_headers"]
    );
    let json = serde_json::to_string(&response).expect("serialize");
    assert!(!json.contains("not-a-real-key"));
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

/// A `base_url` is user-written config, and all three of userinfo, query and
/// fragment are places a key ends up. None of them may survive into a value
/// that leaves the process — on the parse path or the fallback one.
#[test]
fn endpoint_label_drops_userinfo_and_query() {
    assert_eq!(
        endpoint_label("https://not-a-real-key@api.example.test/v1"),
        "api.example.test/v1"
    );
    assert_eq!(
        endpoint_label("https://user:not-a-real-key@api.example.test/v1"),
        "api.example.test/v1"
    );
    assert_eq!(
        endpoint_label("https://api.example.test/v1?api_key=not-a-real-key"),
        "api.example.test/v1"
    );
    // The fallback path: not a URL at all, so nothing was parsed away for us.
    assert_eq!(
        endpoint_label("weird://not-a-real-key@host/v1?token=not-a-real-key"),
        "host/v1"
    );
    assert_eq!(endpoint_label("host/v1#not-a-real-key"), "host/v1");
}

/// An older shell answers without whatever was added last; a newer one answers
/// with more than the client knows. Neither may fail to parse.
#[test]
fn responses_tolerate_missing_and_unknown_fields() {
    let older: ResolvedCatalogResponse = serde_json::from_str(
        r#"{"entries":[{"key":"acme/some-model","facts":{"wireSlug":"some-model"}}]}"#,
    )
    .expect("a response missing later fields still parses");
    let facts = facts_for(&older, "acme/some-model");
    assert_eq!(facts.wire_slug, "some-model");
    assert_eq!(facts.context_window, 0);
    assert!(facts.max_output_tokens.is_none());
    assert!(provider_for(&older, "acme/some-model").is_none());

    let newer: ResolvedCatalogResponse = serde_json::from_str(
        r#"{"entries":[{"key":"k","provider":"p","facts":{"wireSlug":"s","futureField":7}}],"futureField":7}"#,
    )
    .expect("unknown fields are ignored");
    assert_eq!(provider_for(&newer, "k"), Some("p"));

    let empty: ResolvedCatalogResponse =
        serde_json::from_str("{}").expect("an empty response parses");
    assert!(empty.entries.is_empty());
}

/// The response is the client's parsing contract; a field that quietly changed
/// name would be a field the client stops reading.
#[test]
fn responses_serialize_with_the_names_the_clients_read() {
    let response = ResolvedCatalogResponse {
        entries: vec![ResolvedModelEntry {
            key: "acme/some-model".to_owned(),
            provider: Some("acme".to_owned()),
            facts: ResolvedModelFacts {
                wire_slug: "some-model".to_owned(),
                endpoint: "api.example.test/v1".to_owned(),
                api_backend: "messages".to_owned(),
                context_window: 1000,
                max_output_tokens: Some(64),
                agent_type: "grok".to_owned(),
                own_credentials: true,
                from_provider_table: true,
                from_model_table: false,
                overrides: vec!["model".to_owned()],
            },
        }],
    };
    let json = serde_json::to_value(&response).expect("serialize");
    let entry = &json["entries"][0];
    assert_eq!(entry["key"], "acme/some-model");
    assert_eq!(entry["provider"], "acme");
    assert_eq!(entry["facts"]["wireSlug"], "some-model");
    assert_eq!(entry["facts"]["maxOutputTokens"], 64);
    assert_eq!(entry["facts"]["ownCredentials"], true);
    assert_eq!(entry["facts"]["fromProviderTable"], true);
}
