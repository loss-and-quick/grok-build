//! Plugin-contributed settings rows.
//!
//! ## Who owns the value
//!
//! The plugin owns the **schema**; grok owns the **value**. A row's shape comes
//! from the plugin manifest's `settings` array
//! ([`xai_grok_agent::plugins::PluginManifest::plugin_settings`]) and reaches
//! this process over the existing `x.ai/plugins/list` / `PluginsChanged` wire.
//! The value is stored by grok under `[plugins.<name>]` in `config.toml` and is
//! written through the same [`update_config`] chokepoint every other setting
//! goes through.
//!
//! The alternative — the sidecar keeping its own value and grok only drawing a
//! UI for it — was rejected on three counts:
//!
//! - **The read-only config lock.** `update_config` refuses to rewrite a
//!   `config.toml` that grok was not given to rewrite (a home-manager
//!   `/nix/store` symlink, `GROK_CONFIG_READONLY`). A plugin-held value would
//!   be a hole in that: the row would look editable, the sidecar would happily
//!   persist it into its own `~/.grok/plugin-data/<id>` store, and the machine
//!   whose configuration is supposed to be declarative would have acquired
//!   mutable per-plugin state grok's own lock says it must not have.
//! - **Removal.** A value under `[plugins.<name>]` is inert the moment the
//!   plugin stops being loaded, sits in a file the user can already read and
//!   edit, and comes back if the plugin does. Sidecar-held state left behind by
//!   an uninstalled plugin is invisible and unreachable.
//! - **Fresh machines.** A declarative config already reproduces
//!   `[plugins.<name>]` on a new machine, because it is one more table in the
//!   file the config manager writes. Nothing reproduces a sidecar's private
//!   store.
//!
//! Nothing new is needed on the wire for the plugin to read the value back:
//! `session::plugin_host` already shallow-merges `[plugins.<name>]` over the
//! manifest's `config` defaults and hands the result to the sidecar at
//! `initialize` and via `config_get`.
//!
//! [`update_config`]: xai_grok_shell::util::config
//!
//! ## Ownership, category and lock
//!
//! Plugin rows are [`SettingOwner::Shell`] — shell schema, shell-mediated
//! write, no pager-side cache — which is what they are, and which is what makes
//! [`SettingsModalState::row_lock`] treat them like every other on-disk row:
//! a read-only `config.toml` locks them without a special case.
//!
//! They carry `restart_required: true`. Not because the *app* must restart, but
//! because the sidecar is handed its config object once, at `initialize`, and
//! `config_get` answers from that same snapshot — so a value changed now is a
//! value the plugin sees the next time it starts. The flag is derived here
//! rather than declared in the manifest: it is a fact about the host's plugin
//! lifecycle, not something a plugin author is in a position to promise.

use std::collections::{HashMap, HashSet};

use xai_hooks_plugins_types::{PluginInfo, PluginSettingInfo, PluginSettingKindInfo};

use super::intern::{intern, intern_choices, intern_strs};
use super::registry::{
    EnumChoice, SettingCategory, SettingKey, SettingKind, SettingMeta, SettingOwner,
    SettingSurface, SettingValue, StringValidator,
};

// The key namespace and how it splits are wire facts — the shell routes a write
// by them — so they are defined once, in the wire crate, and re-exported here
// for the call sites that already name them.
pub use xai_grok_settings_types::{PLUGIN_KEY_PREFIX, is_plugin_key, split_plugin_key};

/// Strip characters that must never reach a rendered row.
///
/// **SECURITY:** plugin-authored label text lands in the same list as
/// `permission_mode` and `coding_data_sharing`. Uses the one predicate every
/// other untrusted-text site shares, so the unsafe set cannot drift between
/// them.
fn scrub(text: &str) -> String {
    text.chars()
        .filter(|c| !crate::render::line_utils::is_unsafe_display_char(*c))
        .collect()
}

/// A fingerprint of the plugin schema a registry was built from.
///
/// Rebuilding interns new slices (choices, group children, keywords), which the
/// string pool cannot dedupe; comparing this first means a `PluginsChanged`
/// that changed nothing about the settings costs nothing.
pub fn plugin_settings_fingerprint(plugins: &[PluginInfo]) -> String {
    let mut out = String::new();
    for plugin in plugins {
        if plugin.settings.is_empty() {
            continue;
        }
        out.push_str(&plugin.name);
        out.push('\u{1}');
        for setting in &plugin.settings {
            out.push_str(&serde_json::to_string(setting).unwrap_or_default());
            out.push('\u{2}');
        }
    }
    out
}

/// Build the settings rows contributed by `plugins`.
///
/// One [`SettingKind::Group`] row per contributing plugin — the attribution:
/// the plugin's name is the row a user sees, and its settings live behind it —
/// followed by that plugin's own rows, which the modal renders only inside the
/// group's sub-sheet.
///
/// Plugins are visited in the order the shell listed them (scope, then name);
/// a plugin with no settings contributes nothing at all, not an empty group.
pub fn plugin_setting_rows(plugins: &[PluginInfo]) -> Vec<SettingMeta> {
    let mut rows: Vec<SettingMeta> = Vec::new();
    let mut seen_plugins: HashSet<&str> = HashSet::new();
    for plugin in plugins {
        if plugin.settings.is_empty() {
            continue;
        }
        // Two loaded plugins can share a name (one shadows the other and
        // carries a `conflict`); their rows would collide on the same key and
        // `assert_unique_keys` would panic the process. First listed wins,
        // matching how the shadowed plugin's other components are treated.
        if !seen_plugins.insert(plugin.name.as_str()) {
            tracing::warn!(
                plugin = %plugin.name,
                "duplicate plugin name in settings contribution; keeping the first"
            );
            continue;
        }
        let mut children: Vec<SettingKey> = Vec::new();
        let mut child_rows: Vec<SettingMeta> = Vec::new();
        for setting in &plugin.settings {
            let Some(meta) = plugin_row(&plugin.name, setting) else {
                continue;
            };
            children.push(meta.key);
            child_rows.push(meta);
        }
        if children.is_empty() {
            continue;
        }
        rows.push(SettingMeta {
            key: intern(&format!("{PLUGIN_KEY_PREFIX}{}", plugin.name)),
            category: SettingCategory::Plugins,
            // A group row carries no value of its own, so nothing about it is
            // written to disk — the same reason the registry's other group rows
            // are not locked by a read-only config.
            owner: SettingOwner::Pager,
            // A plugin author is in no position to say a preference is a
            // terminal concept, and nothing in a manifest can express it, so
            // every contributed row is offered to every client.
            surface: SettingSurface::Any,
            label: intern(&scrub(&plugin.name)),
            description: intern(&scrub(
                plugin
                    .description
                    .as_deref()
                    .unwrap_or("Settings contributed by this plugin."),
            )),
            keywords: intern_strs(&[intern("plugin"), intern(&scrub(&plugin.name))]),
            kind: SettingKind::Group {
                children: intern_strs(&children),
            },
            restart_required: false,
            hidden_in_minimal: false,
        });
        rows.extend(child_rows);
    }
    rows
}

/// Build one plugin setting's row. `None` when the declaration cannot become a
/// row this modal can render.
fn plugin_row(plugin: &str, setting: &PluginSettingInfo) -> Option<SettingMeta> {
    let kind = match &setting.kind {
        PluginSettingKindInfo::Bool { default } => SettingKind::Bool { default: *default },
        PluginSettingKindInfo::String { default } => SettingKind::String {
            default: intern(&scrub(default)),
            // The plugin's own value vocabulary; grok has no basis to reject
            // any UTF-8 the user types into it.
            validator: StringValidator::Any,
        },
        PluginSettingKindInfo::Int { default, min, max } => {
            if min >= max {
                return None;
            }
            SettingKind::Int {
                default: (*default).clamp(*min, *max),
                min: *min,
                max: *max,
            }
        }
        PluginSettingKindInfo::Enum { default, choices } => {
            let choices: Vec<EnumChoice> = choices
                .iter()
                .map(|c| EnumChoice {
                    canonical: intern(&c.value),
                    display: intern(&scrub(&c.label)),
                    description: intern(&scrub(&c.description)),
                })
                .collect();
            let default = choices
                .iter()
                .find(|c| c.canonical == default)
                .or(choices.first())?
                .canonical;
            SettingKind::Enum {
                default,
                choices: intern_choices(&choices),
                // Previewing a plugin's value would mean persisting it — there
                // is no live visual to preview and no way to revert what the
                // sidecar may already have acted on.
                supports_preview: false,
            }
        }
    };
    Some(SettingMeta {
        key: intern(&format!("{PLUGIN_KEY_PREFIX}{plugin}.{}", setting.key)),
        category: SettingCategory::Plugins,
        owner: SettingOwner::Shell,
        surface: SettingSurface::Any,
        label: intern(&scrub(&setting.label)),
        description: intern(&scrub(&setting.description)),
        keywords: intern_strs(&[
            intern("plugin"),
            intern(&scrub(plugin)),
            intern(&setting.key),
        ]),
        kind,
        // See the module docs: the sidecar takes its config at `initialize`.
        restart_required: true,
        hidden_in_minimal: false,
    })
}

/// Resolve every plugin row's current value from the `[plugins]` table.
///
/// `plugins_table` is the `[plugins]` section of the effective config, i.e. a
/// map of plugin name to that plugin's config object. A key that is absent, or
/// present with the wrong JSON type, reads as the registry default — the same
/// treatment an unparseable `[ui]` value gets.
pub fn resolve_plugin_values(
    rows: &[SettingMeta],
    plugins_table: &serde_json::Value,
) -> HashMap<SettingKey, SettingValue> {
    let mut out = HashMap::new();
    for meta in rows {
        let Some((plugin, key)) = split_plugin_key(meta.key) else {
            continue;
        };
        let stored = plugins_table.get(plugin).and_then(|t| t.get(key));
        let value = match (&meta.kind, stored) {
            (SettingKind::Bool { default }, stored) => {
                SettingValue::Bool(stored.and_then(|v| v.as_bool()).unwrap_or(*default))
            }
            (SettingKind::String { default, .. }, stored) => SettingValue::String(
                stored
                    .and_then(|v| v.as_str())
                    .map(scrub)
                    .unwrap_or_else(|| (*default).to_string()),
            ),
            (SettingKind::Int { default, min, max }, stored) => SettingValue::Int(
                stored
                    .and_then(|v| v.as_i64())
                    .unwrap_or(*default)
                    .clamp(*min, *max),
            ),
            (
                SettingKind::Enum {
                    default, choices, ..
                },
                stored,
            ) => SettingValue::Enum(
                stored
                    .and_then(|v| v.as_str())
                    // An on-disk value outside the catalog reads as the
                    // default; `SettingValue::Enum` is a catalog canonical, and
                    // a picker cannot show a choice it does not have.
                    .and_then(|s| choices.iter().find(|c| c.canonical == s))
                    .map(|c| c.canonical)
                    .unwrap_or(default),
            ),
            _ => continue,
        };
        out.insert(meta.key, value);
    }
    out
}

/// The JSON value written to `[plugins.<name>].<key>` for a settings value.
pub fn plugin_json_value(value: &SettingValue) -> serde_json::Value {
    match value {
        SettingValue::Bool(b) => serde_json::Value::Bool(*b),
        SettingValue::Int(i) => serde_json::Value::Number((*i).into()),
        SettingValue::String(s) => serde_json::Value::String(s.clone()),
        SettingValue::Enum(s) => serde_json::Value::String((*s).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_hooks_plugins_types::{
        HookStatus, McpStatus, PluginScope, PluginSettingChoiceInfo, PluginSettingInfo,
    };

    fn plugin(name: &str, settings: Vec<PluginSettingInfo>) -> PluginInfo {
        PluginInfo {
            name: name.to_string(),
            id: format!("user/deadbeef/{name}"),
            root: format!("/plugins/{name}"),
            scope: PluginScope::User,
            trusted: true,
            enabled: true,
            version: None,
            description: Some("A test plugin".to_string()),
            skill_count: 0,
            skill_names: vec![],
            agent_count: 0,
            agent_names: vec![],
            hook_status: HookStatus::None,
            hook_count: 0,
            mcp_server_count: 0,
            mcp_status: McpStatus::None,
            marketplace_source: None,
            origin: None,
            conflict: None,
            load_error: None,
            settings,
        }
    }

    fn bool_setting(key: &str, default: bool) -> PluginSettingInfo {
        PluginSettingInfo {
            key: key.to_string(),
            label: key.to_string(),
            description: String::new(),
            kind: PluginSettingKindInfo::Bool { default },
        }
    }

    #[test]
    fn group_row_precedes_its_children_and_lists_them() {
        let rows = plugin_setting_rows(&[plugin(
            "council",
            vec![
                bool_setting("verbose", false),
                bool_setting("dry_run", true),
            ],
        )]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].key, "plugin.council");
        assert_eq!(rows[0].label, "council");
        let SettingKind::Group { children } = rows[0].kind else {
            panic!("first row must be the plugin's group row");
        };
        assert_eq!(
            children,
            ["plugin.council.verbose", "plugin.council.dry_run"]
        );
        assert_eq!(rows[1].key, "plugin.council.verbose");
        assert_eq!(rows[2].key, "plugin.council.dry_run");
    }

    /// A plugin row is SHELL-owned so the read-only-config lock catches it
    /// without a plugin-specific case, and restart-flagged because the sidecar
    /// only reads its config at `initialize`.
    #[test]
    fn plugin_rows_are_shell_owned_and_restart_flagged() {
        let rows = plugin_setting_rows(&[plugin("council", vec![bool_setting("verbose", false)])]);
        let child = rows
            .iter()
            .find(|r| r.key == "plugin.council.verbose")
            .unwrap();
        assert_eq!(child.owner, SettingOwner::Shell);
        assert!(child.restart_required);
        assert_eq!(child.category, SettingCategory::Plugins);
    }

    #[test]
    fn a_plugin_with_no_settings_contributes_no_group_row() {
        assert!(plugin_setting_rows(&[plugin("quiet", vec![])]).is_empty());
    }

    #[test]
    fn duplicate_plugin_names_do_not_collide_on_one_key() {
        let rows = plugin_setting_rows(&[
            plugin("council", vec![bool_setting("verbose", false)]),
            plugin("council", vec![bool_setting("verbose", true)]),
        ]);
        assert_eq!(rows.len(), 2, "the shadowed plugin contributes nothing");
    }

    #[test]
    fn label_text_is_scrubbed_of_bidi_and_control_characters() {
        let rows = plugin_setting_rows(&[plugin(
            "spoofer",
            vec![PluginSettingInfo {
                key: "innocent".to_string(),
                label: "Safe\u{202E}gnittes suoregnad".to_string(),
                description: "line\u{7}one".to_string(),
                kind: PluginSettingKindInfo::Bool { default: false },
            }],
        )]);
        let child = &rows[1];
        assert!(!child.label.contains('\u{202E}'));
        assert!(!child.description.contains('\u{7}'));
    }

    #[test]
    fn values_come_from_the_plugins_table_and_fall_back_to_defaults() {
        let rows = plugin_setting_rows(&[plugin(
            "council",
            vec![
                bool_setting("verbose", false),
                PluginSettingInfo {
                    key: "rounds".to_string(),
                    label: "Rounds".to_string(),
                    description: String::new(),
                    kind: PluginSettingKindInfo::Int {
                        default: 2,
                        min: 1,
                        max: 5,
                    },
                },
                PluginSettingInfo {
                    key: "mode".to_string(),
                    label: "Mode".to_string(),
                    description: String::new(),
                    kind: PluginSettingKindInfo::Enum {
                        default: "fast".to_string(),
                        choices: vec![
                            PluginSettingChoiceInfo {
                                value: "fast".to_string(),
                                label: "Fast".to_string(),
                                description: String::new(),
                            },
                            PluginSettingChoiceInfo {
                                value: "thorough".to_string(),
                                label: "Thorough".to_string(),
                                description: String::new(),
                            },
                        ],
                    },
                },
            ],
        )]);
        let table = serde_json::json!({
            "council": { "verbose": true, "rounds": 99, "mode": "nonsense" }
        });
        let values = resolve_plugin_values(&rows, &table);
        assert_eq!(
            values.get("plugin.council.verbose"),
            Some(&SettingValue::Bool(true))
        );
        // Out-of-range clamps; out-of-catalog falls back to the default.
        assert_eq!(
            values.get("plugin.council.rounds"),
            Some(&SettingValue::Int(5))
        );
        assert_eq!(
            values.get("plugin.council.mode"),
            Some(&SettingValue::Enum("fast"))
        );
    }

    #[test]
    fn split_plugin_key_separates_plugin_from_setting() {
        assert_eq!(
            split_plugin_key("plugin.council.dry_run"),
            Some(("council", "dry_run"))
        );
        assert_eq!(split_plugin_key("plugin.council"), None);
        assert_eq!(split_plugin_key("compact_mode"), None);
    }

    #[test]
    fn interning_reuses_one_allocation_per_distinct_string() {
        // Two distinct allocations with the same contents must intern to one
        // `&'static str`, or a plugin reload would leak a fresh key every time.
        let one = format!("plugin.{}.{}", "council", "verbose");
        let two = format!("plugin.{}.{}", "council", "verbose");
        assert!(!std::ptr::eq(one.as_str(), two.as_str()));
        assert!(std::ptr::eq(intern(&one), intern(&two)));
    }

    #[test]
    fn fingerprint_ignores_plugins_without_settings() {
        let with = plugin_settings_fingerprint(&[
            plugin("council", vec![bool_setting("verbose", false)]),
            plugin("quiet", vec![]),
        ]);
        let without =
            plugin_settings_fingerprint(&[plugin("council", vec![bool_setting("verbose", false)])]);
        assert_eq!(with, without);
    }
}
