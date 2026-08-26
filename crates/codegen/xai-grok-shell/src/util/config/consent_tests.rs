use super::*;
use toml::Value as TomlValue;

/// A single slot would drop one of two live notices and re-prompt forever.
#[test]
fn consent_answers_are_kept_per_notice() {
    let root: TomlValue = toml::from_str(
        r#"
[consent.answers."enterprise-tos-2026-08"]
version = 2
account = "user@example.com"

[consent.answers."consumer-tos-2026-08"]
version = 1
account = "other@example.com"
"#,
    )
    .unwrap();

    let consent = super::super::load_config_from_toml(&root).consent;

    assert_eq!(consent.answers["enterprise-tos-2026-08"].version, 2);
    assert_eq!(
        consent.answers["consumer-tos-2026-08"].account.as_deref(),
        Some("other@example.com")
    );

    let emitted = toml::to_string(&consent).unwrap();
    let reparsed: ConsentConfig = toml::from_str(&emitted).unwrap();
    assert_eq!(reparsed, consent);
}

