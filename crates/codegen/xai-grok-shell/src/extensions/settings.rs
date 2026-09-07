//! `x.ai/settings/*`: the settings view, for any client.
//!
//! Two methods, because a settings view needs two things and neither is worth a
//! second round trip on its own:
//!
//! - `x.ai/settings/list` — the catalog, the values and the locks.
//! - `x.ai/settings/set` — one write.
//!
//! The write goes over the wire rather than to disk. That is the change: the
//! pager used to write `~/.grok/config.toml` itself and let the shell notice by
//! mtime, which is a path no other client has. Routing it here puts every
//! client behind [`crate::util::config::update_config`], the one place that
//! declines to rewrite a `config.toml` grok was not given to rewrite — so a
//! declaratively configured machine is protected from a browser exactly as it
//! is from the terminal, by the same `stat`, in the same process.
//!
//! A declined write is not an error. It comes back as
//! `applied: false` with a [`SettingsRefusal`], because nothing broke and the
//! thing to tell the user is where the value lives.

use agent_client_protocol as acp;
use xai_grok_settings_types::{
    SettingKind, SettingOwner, SettingRow, SettingValue, SettingsListResponse, SettingsRefusalKind,
    SettingsSetRequest, SettingsSetResponse,
};

use crate::agent::MvpAgent;
use crate::util::config::SettingWrite;

type ExtResult = Result<acp::ExtResponse, acp::Error>;

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "x.ai/settings/list" => {
            let catalog = crate::settings::catalog();
            // The merged file, for the few rows that live outside `[ui]`. A
            // configuration that will not load is a reason to answer with
            // declared defaults, not a reason to answer with nothing.
            let effective = crate::config::load_effective_config().ok();
            let state = {
                let cfg = agent.cfg.borrow();
                crate::settings::state::resolve(&catalog.rows, &cfg.ui, effective.as_ref())
            };
            super::to_raw_response(&SettingsListResponse {
                catalog: catalog.clone(),
                state,
            })
        }
        "x.ai/settings/set" => {
            let req: SettingsSetRequest = super::parse_params(args)?;
            // A refusal is a normal response; only a write that genuinely broke
            // is an error.
            let response = set(req)
                .await
                .map_err(|e| acp::Error::internal_error().data(e))?;
            super::to_raw_response(&response)
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

/// Validate one write against the catalog, then perform it.
///
/// Validation happens here rather than in each client: two clients that
/// validate separately are two clients that disagree about what is accepted the
/// first time one of them is a version behind.
async fn set(req: SettingsSetRequest) -> Result<SettingsSetResponse, String> {
    // A plugin's rows are discovered from manifests at runtime, so they are not
    // in the generated catalog and cannot be checked against it. The manifest
    // already validated the schema, and `update_config` still has the last word.
    let row = match crate::settings::catalog()
        .rows
        .iter()
        .find(|r| r.key == req.key)
    {
        Some(row) => Some(row),
        None if xai_grok_settings_types::is_plugin_key(&req.key) => None,
        None => {
            return Ok(SettingsSetResponse::refused(
                SettingsRefusalKind::UnknownKey,
                format!("no setting named `{}`", req.key),
            ));
        }
    };

    if let Some(row) = row
        && let Some(refusal) = check(row, &req.value)
    {
        return Ok(refusal);
    }
    perform(&req.key, req.value).await
}

/// Whether the value fits the row it names.
fn check(row: &SettingRow, value: &SettingValue) -> Option<SettingsSetResponse> {
    let wrong_kind = |expected: &str| {
        Some(SettingsSetResponse::refused(
            SettingsRefusalKind::WrongKind,
            format!("`{}` takes {expected}", row.key),
        ))
    };
    // A row the asking client holds itself is not the shell's to write, and
    // reporting a write that did not happen as done would be worse than saying
    // so.
    if row.owner == SettingOwner::Client {
        return Some(SettingsSetResponse::refused(
            SettingsRefusalKind::WrongKind,
            format!("`{}` is held by the client that renders it", row.key),
        ));
    }
    match &row.kind {
        SettingKind::Bool { .. } if value.as_bool().is_none() => wrong_kind("a boolean"),
        SettingKind::String { .. } | SettingKind::DynamicEnum { .. }
            if value.as_str().is_none() =>
        {
            wrong_kind("a string")
        }
        SettingKind::Int { min, max, .. } => match value.as_int() {
            None => wrong_kind("an integer"),
            Some(v) if v < *min || v > *max => Some(SettingsSetResponse::refused(
                SettingsRefusalKind::OutOfRange,
                format!("`{}` accepts {min} to {max}", row.key),
            )),
            Some(_) => None,
        },
        SettingKind::Enum { choices, .. } => match value.as_str() {
            None => wrong_kind("a string"),
            Some(v) if !choices.iter().any(|c| c.canonical == v) => {
                Some(SettingsSetResponse::refused(
                    SettingsRefusalKind::OutOfRange,
                    format!("`{v}` is not one of `{}`'s choices", row.key),
                ))
            }
            Some(_) => None,
        },
        // A navigational row: there is nothing to write to it.
        SettingKind::Group { .. } => Some(SettingsSetResponse::refused(
            SettingsRefusalKind::WrongKind,
            format!("`{}` opens a sub-sheet; it holds no value", row.key),
        )),
        _ => None,
    }
}

/// Perform the write, turning a read-only config into a refusal rather than an
/// error.
async fn perform(key: &str, value: SettingValue) -> Result<SettingsSetResponse, String> {
    // Asked before the write so the message names the setting, which the
    // chokepoint's own refusal cannot do. `update_config` asks again anyway;
    // this one is for the wording.
    if let Some(notice) = crate::util::config::readonly_config_notice(key) {
        return Ok(SettingsSetResponse::refused(
            SettingsRefusalKind::ReadOnlyConfig,
            notice,
        ));
    }
    match crate::util::config::persist_setting(key, value).await? {
        SettingWrite::Persisted => Ok(SettingsSetResponse::applied()),
        // Only reachable for a plugin key, which has no catalog row to have
        // been screened by `check`.
        SettingWrite::ClientOwned => Ok(SettingsSetResponse::refused(
            SettingsRefusalKind::WrongKind,
            format!("`{key}` is held by the client that renders it"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_settings_types::{SettingChoice, SettingSurface};

    fn row(key: &str, owner: SettingOwner, kind: SettingKind) -> SettingRow {
        SettingRow {
            key: key.to_string(),
            category: xai_grok_settings_types::SettingCategory::Advanced,
            owner,
            surface: SettingSurface::Any,
            label: "Test".to_string(),
            description: String::new(),
            keywords: Vec::new(),
            kind,
            restart_required: false,
            hidden_in_minimal: false,
        }
    }

    #[test]
    fn a_value_of_the_declared_shape_passes() {
        let bool_row = row(
            "b",
            SettingOwner::Shell,
            SettingKind::Bool { default: false },
        );
        assert!(check(&bool_row, &SettingValue::Bool(true)).is_none());

        let int_row = row(
            "i",
            SettingOwner::Shell,
            SettingKind::Int {
                default: 3,
                min: 1,
                max: 10,
            },
        );
        assert!(check(&int_row, &SettingValue::Int(10)).is_none());
    }

    #[test]
    fn a_value_of_the_wrong_shape_is_refused_rather_than_written() {
        let bool_row = row(
            "b",
            SettingOwner::Shell,
            SettingKind::Bool { default: false },
        );
        let refusal = check(&bool_row, &SettingValue::String("yes".into()))
            .expect("a string is not a boolean");
        assert_eq!(
            refusal.refusal.unwrap().kind,
            SettingsRefusalKind::WrongKind
        );
    }

    #[test]
    fn an_integer_outside_the_declared_bounds_is_refused() {
        let int_row = row(
            "i",
            SettingOwner::Shell,
            SettingKind::Int {
                default: 3,
                min: 1,
                max: 10,
            },
        );
        for out in [0, 11] {
            let refusal =
                check(&int_row, &SettingValue::Int(out)).expect("{out} is outside 1..=10");
            assert_eq!(
                refusal.refusal.unwrap().kind,
                SettingsRefusalKind::OutOfRange
            );
        }
    }

    /// The bounds a client renders and the bounds the shell enforces are the
    /// same bounds, so a canonical that is not in the row's own choices cannot
    /// reach the config even from a client that never drew the chooser.
    #[test]
    fn a_canonical_that_is_not_a_choice_is_refused() {
        let enum_row = row(
            "e",
            SettingOwner::Shell,
            SettingKind::Enum {
                default: "off".to_string(),
                choices: vec![SettingChoice {
                    canonical: "off".to_string(),
                    display: "Off".to_string(),
                    description: String::new(),
                }],
                supports_preview: false,
            },
        );
        assert!(check(&enum_row, &SettingValue::String("off".into())).is_none());
        let refusal = check(&enum_row, &SettingValue::String("sideways".into()))
            .expect("`sideways` is not a choice");
        assert_eq!(
            refusal.refusal.unwrap().kind,
            SettingsRefusalKind::OutOfRange
        );
    }

    /// A row the asking client holds is not the shell's to write, and saying so
    /// beats reporting a write that never happened as done.
    #[test]
    fn a_client_held_row_is_refused_rather_than_silently_accepted() {
        let client_row = row(
            "multiline_mode",
            SettingOwner::Client,
            SettingKind::Bool { default: false },
        );
        let refusal =
            check(&client_row, &SettingValue::Bool(true)).expect("the shell does not hold it");
        assert!(!refusal.applied);
    }

    /// A group row opens a sub-sheet; there is nothing behind it to write.
    #[test]
    fn a_group_row_holds_no_value_to_write() {
        let group = row(
            "contextual_hints",
            SettingOwner::Shell,
            SettingKind::Group {
                children: vec!["contextual_hints.undo".to_string()],
            },
        );
        assert!(check(&group, &SettingValue::Bool(true)).is_some());
    }
}
