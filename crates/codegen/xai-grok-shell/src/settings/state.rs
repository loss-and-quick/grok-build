//! The runtime half of the settings view: values and locks.
//!
//! ## Values: what is on disk, not what a client is doing
//!
//! A settings row has two readings. There is what the user's configuration
//! says, and there is what the process is doing right now — the permission mode
//! of the running agent, the model the session picked, whether plan mode is on
//! for this turn. The second is session state that already reaches every client
//! by its own route, and a client that has it should keep showing it.
//!
//! So this resolves the first: the persisted value, with the catalog's declared
//! default standing in wherever the configuration is silent. That makes it
//! well-defined for a client that has no session yet, and it makes the fallback
//! the same fallback in every client, because it comes from the same catalog.
//!
//! One rule does real work here: **an enum value that is not one of the row's
//! own choices falls back to the row's default.** That is what canonicalizes a
//! legacy or hand-edited `config.toml` — `hunk_tracker_mode = "disabled"`,
//! `screen_mode = "default"`, a theme name that no longer exists — without any
//! client needing to know the aliases. Rendering a chooser with nothing
//! selected is the failure this prevents.
//!
//! ## Locks: carried, never recomputed
//!
//! [`SettingLockKind::ReadOnlyConfig`] is a `stat` of a file on this machine.
//! A second client cannot perform it, and a client that assumed the config was
//! writable would offer an edit [`crate::util::config::update_config`] is about
//! to decline. So the lock travels, with the sentence that names the file — the
//! path being the one thing the user needs and the one thing a browser has no
//! way to learn.
//!
//! The rule for which rows it locks is the file's, not the key's: a
//! `config.toml` a configuration manager generates is generated whole, so every
//! row that would be written to it is locked. Rows a client holds itself, group
//! rows that carry no value, and `coding_data_sharing` — which lives in auth
//! metadata, not in `config.toml` — are not.

use std::collections::BTreeMap;

use xai_grok_settings_types::{
    ConfigReadOnly, SettingKind, SettingLock, SettingLockKind, SettingOwner, SettingRow,
    SettingValue, SettingsState,
};
use xai_grok_shared::ui_config::UiConfig;

/// The row whose value lives in auth metadata rather than in `config.toml`.
///
/// Named once: it is the single exception to "a read-only config locks every
/// row that would be written to it", and it is an exception in both directions
/// — a read-only config has no say over it, and the locks it does carry come
/// from the account.
const AUTH_BACKED_KEY: &str = "coding_data_sharing";

/// Rows whose value the shell declines to state.
///
/// Not an oversight and not a gap to be filled with a default: each of these
/// resolves against something that reaches a client by its own route — the
/// session's model catalog, the account's data-sharing choice — and a default
/// served here would be a plausible wrong answer rather than a visible absence.
/// A client renders the row from the catalog and fills the value from what it
/// already has.
const RESOLVED_BY_THE_CLIENT: &[&str] = &["default_model", "fork_secondary_model", AUTH_BACKED_KEY];

/// Resolve values and locks for the whole catalog.
///
/// `effective_config` is the merged configuration TOML, used for the handful of
/// rows that live outside `[ui]`; `None` skips them rather than failing the
/// whole response, because a configuration that will not load is a reason to
/// show defaults, not a reason to show nothing.
pub fn resolve(
    rows: &[SettingRow],
    ui: &UiConfig,
    effective_config: Option<&toml::Value>,
) -> SettingsState {
    let ui_json = serde_json::to_value(ui).unwrap_or(serde_json::Value::Null);
    let read_only = crate::util::config::user_config_readonly();

    let mut values = BTreeMap::new();
    let mut locks = BTreeMap::new();
    for row in rows {
        if let Some(value) = value_for(row, &ui_json, effective_config) {
            values.insert(row.key.clone(), value);
        }
        if read_only.is_some()
            && let Some(lock) = read_only_lock(row)
        {
            locks.insert(row.key.clone(), lock);
        }
    }

    SettingsState {
        values,
        locks,
        config_path: crate::util::config::user_config_display_path(),
        config_read_only: read_only.map(lower_read_only),
    }
}

/// The lock a read-only `config.toml` puts on one row, if any.
fn read_only_lock(row: &SettingRow) -> Option<SettingLock> {
    // A client holds its own value, and a group row carries none: neither
    // reaches the file, so neither is locked by it.
    if row.owner == SettingOwner::Client || matches!(row.kind, SettingKind::Group { .. }) {
        return None;
    }
    if row.key == AUTH_BACKED_KEY {
        return None;
    }
    Some(SettingLock {
        kind: SettingLockKind::ReadOnlyConfig,
        // Says only *where* the value lives, not why grok will not write it:
        // the why varies (file mode, an env override) and belongs in the
        // refusal a write comes back with, while the row's job is to point at
        // the file to edit. `SettingsState::config_read_only` carries the why
        // once, for a client that wants to say it.
        reason: format!(
            "Set in {} — change it there.",
            crate::util::config::user_config_display_path()
        ),
        hides_value: false,
        admin_managed: false,
    })
}

/// One row's persisted value.
fn value_for(
    row: &SettingRow,
    ui_json: &serde_json::Value,
    effective_config: Option<&toml::Value>,
) -> Option<SettingValue> {
    // A group row has no value of its own, and neither does a row whose value
    // the asking client is the one holding.
    if matches!(row.kind, SettingKind::Group { .. }) || row.owner == SettingOwner::Client {
        return None;
    }
    if RESOLVED_BY_THE_CLIENT.contains(&row.key.as_str()) {
        return None;
    }
    let stored = stored_value(row, ui_json, effective_config);
    coerce(row, stored)
}

/// Where a row's value is written, and what is there now.
fn stored_value(
    row: &SettingRow,
    ui_json: &serde_json::Value,
    effective_config: Option<&toml::Value>,
) -> Option<serde_json::Value> {
    match row.key.as_str() {
        // `[cli]`, not `[ui]`: they predate the settings modal and are shared
        // with the launcher.
        "show_tips" | "auto_update" => {
            toml_path(effective_config?, &["cli", &row.key]).and_then(toml_to_json)
        }
        // Already a full path from the config root.
        key if key.starts_with("toolset.") => {
            let segments: Vec<&str> = key.split('.').collect();
            toml_path(effective_config?, &segments).and_then(toml_to_json)
        }
        // The key is the row's identity, not the field's: the value lives one
        // table down.
        "display_refresh_auto_cadence" => {
            json_path(ui_json, &["display_refresh", "auto_cadence_enabled"]).cloned()
        }
        // Everything else: the key is the `[ui]` field name, dotted for a
        // nested table. That is not a coincidence to be relied on quietly —
        // it is the registry's stated contract for a shell-owned key.
        key => json_path(ui_json, &key.split('.').collect::<Vec<_>>()).cloned(),
    }
}

/// Fit a stored value to the row's declared kind, falling back to its default.
fn coerce(row: &SettingRow, stored: Option<serde_json::Value>) -> Option<SettingValue> {
    match &row.kind {
        SettingKind::Bool { default } => Some(SettingValue::Bool(
            stored
                .as_ref()
                .and_then(|v| v.as_bool())
                .unwrap_or(*default),
        )),
        SettingKind::Int { default, min, max } => {
            let raw = stored.as_ref().and_then(|v| v.as_i64()).unwrap_or(*default);
            Some(SettingValue::Int(raw.clamp(*min, *max)))
        }
        SettingKind::String { default, .. } => Some(SettingValue::String(
            stored
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or(default)
                .to_string(),
        )),
        SettingKind::Enum {
            default, choices, ..
        } => {
            let raw = stored.as_ref().and_then(|v| v.as_str());
            let canonical = raw
                .filter(|s| choices.iter().any(|c| c.canonical == *s))
                .unwrap_or(default);
            Some(SettingValue::String(canonical.to_string()))
        }
        // Its choices come from a runtime catalog, so a value is only meaningful
        // to a client that has one.
        SettingKind::DynamicEnum { .. } => None,
        SettingKind::Group { .. } => None,
    }
}

/// Walk a dotted path through a JSON object.
fn json_path<'a>(root: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut node = root;
    for segment in path {
        node = node.get(segment)?;
    }
    Some(node).filter(|v| !v.is_null())
}

/// Walk a dotted path through a TOML table.
fn toml_path<'a>(root: &'a toml::Value, path: &[&str]) -> Option<&'a toml::Value> {
    let mut node = root;
    for segment in path {
        node = node.get(segment)?;
    }
    Some(node)
}

/// The scalar shapes a settings value can take, as JSON.
fn toml_to_json(value: &toml::Value) -> Option<serde_json::Value> {
    match value {
        toml::Value::Boolean(b) => Some(serde_json::Value::Bool(*b)),
        toml::Value::Integer(i) => Some(serde_json::Value::from(*i)),
        toml::Value::String(s) => Some(serde_json::Value::String(s.clone())),
        _ => None,
    }
}

/// Local alias so the signature above reads without importing the shell's own
/// enum into the wire vocabulary.
type ConfigReadOnlySource = crate::util::config::ConfigReadOnly;

fn lower_read_only(source: ConfigReadOnlySource) -> ConfigReadOnly {
    match source {
        ConfigReadOnlySource::FileMode => ConfigReadOnly::FileMode,
        ConfigReadOnlySource::Env => ConfigReadOnly::Env,
    }
}
