//! The settings catalog as something other than Rust.
//!
//! ## What this is for
//!
//! A client renders; it does not compute. The settings view was the largest
//! thing in the pager that broke that rule: the catalog was a const table
//! compiled into this binary, so a second client had no way to learn that
//! `theme` is an enum of six choices, that `scroll_lines` is bounded at 1..=10,
//! or that either of them exists.
//!
//! This module is the one place the catalog crosses out of Rust. It lowers the
//! registry's [`SettingMeta`] into [`xai_grok_settings_types`] and raises it
//! back, and [`SettingsRegistry::defaults`] is built by doing exactly that —
//! lower, then raise. The pager therefore renders the wire representation and
//! nothing else. A field that does not survive the round trip is not a subtle
//! drift; it is a row the TUI stops drawing, in this build, on the first run.
//!
//! ## The guard
//!
//! Three levels, strongest first.
//!
//! 1. **[`lower_row`] destructures [`SettingMeta`] with no `..` rest pattern**,
//!    and [`lower_kind`] does the same for every [`SettingKind`] variant. A new
//!    field or a new variant does not compile until it has a place on the wire
//!    (E0027 / E0004). This is the half that makes it *impossible* to add a
//!    setting the wire does not carry, rather than merely detectable.
//! 2. **[`tests::the_registry_survives_a_round_trip`]** raises what it lowered
//!    and compares, so a field that is lowered but dropped on the way back is
//!    caught even though both halves compile.
//! 3. **[`tests::the_generated_catalog_matches_the_registry`]** compares the
//!    checked-in artifact against the registry and refuses to rewrite it. The
//!    shell serves that file to every other client, so a new setting that has
//!    not been regenerated fails here rather than reaching a browser as a row
//!    that silently does not exist. There is no CI in this repository to run
//!    `git diff --exit-code`, which is why the check is a test and why it
//!    compares instead of overwriting.
//!
//! The pattern is the one the theme palette already uses
//! (`xai_grok_pager_render::theme::tokens`), for the same reason and with the
//! same escape hatch: an intended change is regenerated explicitly.
//!
//! ## What is not here
//!
//! Values and locks. Both are runtime facts, so they are resolved per request
//! by the shell rather than baked into an artifact; see
//! `xai_grok_shell::settings`. The types they travel in are in the same wire
//! crate, and [`lower_value`] and [`lower_lock`] map the pager's own view onto
//! them so the two clients agree on what a lock even is.

use std::path::{Path, PathBuf};

use xai_grok_settings_types as wire;

use super::intern::{intern, intern_choices, intern_strs};
use super::registry::{
    CodingDataSharingLock, DynamicEnumSource, EnumChoice, RowLock, SettingCategory, SettingKind,
    SettingMeta, SettingOwner, SettingSurface, SettingValue, SettingsRegistry, StringValidator,
};

/// Path of the generated catalog, relative to the repository root.
///
/// It lives under `sdk/` beside the exported theme palette because it is an
/// artifact for clients that are not this one; the shell embeds it and serves
/// it verbatim.
pub const ARTIFACT_REL_PATH: &str = "sdk/settings/src/generated/catalog.json";

/// Command that rewrites the artifact when a catalog change is intended.
pub const REGENERATE_CMD: &str =
    "GROK_WRITE_SETTINGS_CATALOG=1 cargo test -p xai-grok-pager settings::wire";

// ---------------------------------------------------------------------------
// Lowering: registry to wire
// ---------------------------------------------------------------------------

/// Lower one registry row.
///
/// **The destructuring pattern carries no `..` on purpose.** Adding a field to
/// [`SettingMeta`] is a compile error here until the field is given a place on
/// the wire. That is the load-bearing half of the drift guard: the tests below
/// catch a stale artifact, this catches a fact the other client would never
/// have heard of.
pub fn lower_row(meta: &SettingMeta) -> wire::SettingRow {
    let SettingMeta {
        key,
        category,
        owner,
        surface,
        label,
        description,
        keywords,
        kind,
        restart_required,
        hidden_in_minimal,
    } = meta;
    wire::SettingRow {
        key: (*key).to_string(),
        category: lower_category(*category),
        owner: lower_owner(*owner),
        surface: lower_surface(*surface),
        label: (*label).to_string(),
        description: (*description).to_string(),
        keywords: keywords.iter().map(|k| (*k).to_string()).collect(),
        kind: lower_kind(kind),
        restart_required: *restart_required,
        hidden_in_minimal: *hidden_in_minimal,
    }
}

/// Lower a row's value shape. Exhaustive: a new [`SettingKind`] does not
/// compile until the wire learns to carry it.
pub fn lower_kind(kind: &SettingKind) -> wire::SettingKind {
    match kind {
        SettingKind::Bool { default } => wire::SettingKind::Bool { default: *default },
        SettingKind::String { default, validator } => wire::SettingKind::String {
            default: (*default).to_string(),
            validator: lower_validator(*validator),
        },
        SettingKind::Enum {
            default,
            choices,
            supports_preview,
        } => wire::SettingKind::Enum {
            default: (*default).to_string(),
            choices: choices.iter().map(lower_choice).collect(),
            supports_preview: *supports_preview,
        },
        SettingKind::Int { default, min, max } => wire::SettingKind::Int {
            default: *default,
            min: *min,
            max: *max,
        },
        SettingKind::DynamicEnum {
            default,
            source,
            supports_preview,
        } => wire::SettingKind::DynamicEnum {
            default: (*default).to_string(),
            source: lower_dynamic_source(*source),
            supports_preview: *supports_preview,
        },
        SettingKind::Group { children } => wire::SettingKind::Group {
            children: children.iter().map(|c| (*c).to_string()).collect(),
        },
    }
}

fn lower_choice(choice: &EnumChoice) -> wire::SettingChoice {
    let EnumChoice {
        canonical,
        display,
        description,
    } = choice;
    wire::SettingChoice {
        canonical: (*canonical).to_string(),
        display: (*display).to_string(),
        description: (*description).to_string(),
    }
}

fn lower_category(category: SettingCategory) -> wire::SettingCategory {
    match category {
        SettingCategory::Appearance => wire::SettingCategory::Appearance,
        SettingCategory::Mouse => wire::SettingCategory::Mouse,
        SettingCategory::Editor => wire::SettingCategory::Editor,
        SettingCategory::Agent => wire::SettingCategory::Agent,
        SettingCategory::Privacy => wire::SettingCategory::Privacy,
        SettingCategory::Models => wire::SettingCategory::Models,
        SettingCategory::Session => wire::SettingCategory::Session,
        SettingCategory::Advanced => wire::SettingCategory::Advanced,
        SettingCategory::Plugins => wire::SettingCategory::Plugins,
    }
}

/// `Pager` becomes `Client` on the wire: the distinction it draws is "the
/// process rendering this row holds the value", which stops being about the
/// pager the moment a second client renders the same row.
fn lower_owner(owner: SettingOwner) -> wire::SettingOwner {
    match owner {
        SettingOwner::Pager => wire::SettingOwner::Client,
        SettingOwner::Shell => wire::SettingOwner::Shell,
        SettingOwner::Shared => wire::SettingOwner::ShellCached,
    }
}

fn lower_surface(surface: SettingSurface) -> wire::SettingSurface {
    match surface {
        SettingSurface::Any => wire::SettingSurface::Any,
        SettingSurface::Terminal => wire::SettingSurface::Terminal,
    }
}

fn lower_validator(validator: StringValidator) -> wire::StringValidator {
    match validator {
        StringValidator::NonEmptyToken => wire::StringValidator::NonEmptyToken,
        StringValidator::KnownModel => wire::StringValidator::KnownModel,
        StringValidator::Any => wire::StringValidator::Any,
    }
}

fn lower_dynamic_source(source: DynamicEnumSource) -> wire::DynamicEnumSource {
    match source {
        DynamicEnumSource::ActiveModelCatalog => wire::DynamicEnumSource::ActiveModelCatalog,
    }
}

/// Lower a resolved value.
///
/// `Enum` and `String` collapse to one wire shape because a canonical is a
/// string on disk too; the row's kind already says which one a client is
/// looking at.
pub fn lower_value(value: &SettingValue) -> wire::SettingValue {
    match value {
        SettingValue::Bool(v) => wire::SettingValue::Bool(*v),
        SettingValue::Int(v) => wire::SettingValue::Int(*v),
        SettingValue::String(v) => wire::SettingValue::String(v.clone()),
        SettingValue::Enum(v) => wire::SettingValue::String((*v).to_string()),
    }
}

/// Lower a row lock, carrying the sentence with it.
///
/// The reason is not derived on the far side: `ReadOnlyConfig` names a path on
/// the leader's machine, which is the one thing a second client is guaranteed
/// not to know and the one thing the user needs the message to say.
pub fn lower_lock(lock: RowLock) -> wire::SettingLock {
    let kind = match lock {
        RowLock::ReadOnlyConfig => wire::SettingLockKind::ReadOnlyConfig,
        RowLock::CodingDataSharing(CodingDataSharingLock::Zdr) => {
            wire::SettingLockKind::CodingDataSharingZdr
        }
        RowLock::CodingDataSharing(CodingDataSharingLock::TeamManaged) => {
            wire::SettingLockKind::CodingDataSharingTeamManaged
        }
    };
    wire::SettingLock {
        kind,
        reason: lock.reason().to_string(),
        hides_value: lock.hides_value(),
        admin_managed: lock.is_admin_managed(),
    }
}

/// The whole catalog, ready to serialize.
pub fn catalog_from(entries: &[SettingMeta]) -> wire::SettingsCatalog {
    wire::SettingsCatalog {
        version: wire::CATALOG_VERSION,
        categories: SettingCategory::ALL
            .iter()
            .map(|cat| wire::SettingCategoryInfo {
                id: lower_category(*cat),
                label: cat.label().to_string(),
            })
            .collect(),
        rows: entries.iter().map(lower_row).collect(),
    }
}

// ---------------------------------------------------------------------------
// Raising: wire to registry
// ---------------------------------------------------------------------------

/// Rebuild the registry's own row from a wire row.
///
/// `None` for a row this build cannot render — which today means only a
/// category or kind a newer shell knows about. Dropping the row is deliberate:
/// an unknown row cannot be drawn, and taking the TUI down over one is a denial
/// of service, not a check. The same call is why every string here goes through
/// the intern pool rather than being leaked.
pub fn raise_row(row: &wire::SettingRow) -> Option<SettingMeta> {
    Some(SettingMeta {
        key: intern(&row.key),
        category: raise_category(row.category),
        owner: raise_owner(row.owner),
        surface: raise_surface(row.surface),
        label: intern(&row.label),
        description: intern(&row.description),
        keywords: intern_strs(
            &row.keywords
                .iter()
                .map(|k| intern(k))
                .collect::<Vec<&'static str>>(),
        ),
        kind: raise_kind(&row.kind)?,
        restart_required: row.restart_required,
        hidden_in_minimal: row.hidden_in_minimal,
    })
}

fn raise_kind(kind: &wire::SettingKind) -> Option<SettingKind> {
    Some(match kind {
        wire::SettingKind::Bool { default } => SettingKind::Bool { default: *default },
        wire::SettingKind::String { default, validator } => SettingKind::String {
            default: intern(default),
            validator: raise_validator(*validator),
        },
        wire::SettingKind::Enum {
            default,
            choices,
            supports_preview,
        } => SettingKind::Enum {
            default: intern(default),
            choices: intern_choices(&choices.iter().map(raise_choice).collect::<Vec<_>>()),
            supports_preview: *supports_preview,
        },
        wire::SettingKind::Int { default, min, max } => SettingKind::Int {
            default: *default,
            min: *min,
            max: *max,
        },
        wire::SettingKind::DynamicEnum {
            default,
            source,
            supports_preview,
        } => SettingKind::DynamicEnum {
            default: intern(default),
            source: raise_dynamic_source(*source),
            supports_preview: *supports_preview,
        },
        wire::SettingKind::Group { children } => SettingKind::Group {
            children: intern_strs(
                &children
                    .iter()
                    .map(|c| intern(c))
                    .collect::<Vec<&'static str>>(),
            ),
        },
    })
}

fn raise_choice(choice: &wire::SettingChoice) -> EnumChoice {
    EnumChoice {
        canonical: intern(&choice.canonical),
        display: intern(&choice.display),
        description: intern(&choice.description),
    }
}

fn raise_category(category: wire::SettingCategory) -> SettingCategory {
    match category {
        wire::SettingCategory::Appearance => SettingCategory::Appearance,
        wire::SettingCategory::Mouse => SettingCategory::Mouse,
        wire::SettingCategory::Editor => SettingCategory::Editor,
        wire::SettingCategory::Agent => SettingCategory::Agent,
        wire::SettingCategory::Privacy => SettingCategory::Privacy,
        wire::SettingCategory::Models => SettingCategory::Models,
        wire::SettingCategory::Session => SettingCategory::Session,
        wire::SettingCategory::Advanced => SettingCategory::Advanced,
        wire::SettingCategory::Plugins => SettingCategory::Plugins,
    }
}

fn raise_owner(owner: wire::SettingOwner) -> SettingOwner {
    match owner {
        wire::SettingOwner::Client => SettingOwner::Pager,
        wire::SettingOwner::Shell => SettingOwner::Shell,
        wire::SettingOwner::ShellCached => SettingOwner::Shared,
    }
}

fn raise_surface(surface: wire::SettingSurface) -> SettingSurface {
    match surface {
        wire::SettingSurface::Any => SettingSurface::Any,
        wire::SettingSurface::Terminal => SettingSurface::Terminal,
    }
}

fn raise_validator(validator: wire::StringValidator) -> StringValidator {
    match validator {
        wire::StringValidator::NonEmptyToken => StringValidator::NonEmptyToken,
        wire::StringValidator::KnownModel => StringValidator::KnownModel,
        wire::StringValidator::Any => StringValidator::Any,
    }
}

fn raise_dynamic_source(source: wire::DynamicEnumSource) -> DynamicEnumSource {
    match source {
        wire::DynamicEnumSource::ActiveModelCatalog => DynamicEnumSource::ActiveModelCatalog,
    }
}

/// Raise every row of a catalog, dropping the ones this build cannot render.
pub fn raise_rows(catalog: &wire::SettingsCatalog) -> Vec<SettingMeta> {
    catalog.rows.iter().filter_map(raise_row).collect()
}

// ---------------------------------------------------------------------------
// The artifact
// ---------------------------------------------------------------------------

/// The catalog as the shell serves it, pretty-printed with a trailing newline.
///
/// Pretty rather than compact because the file is reviewed as a diff: a
/// one-line catalog would make every change look like the whole file changed,
/// which is the review failure this artifact exists to avoid.
pub fn generate() -> String {
    let catalog = catalog_from(&crate::settings::defs::default_settings());
    let mut json = serde_json::to_string_pretty(&catalog).expect("the catalog serializes");
    json.push('\n');
    json
}

/// Absolute path of the checked-in artifact.
///
/// Test-only: `CARGO_MANIFEST_DIR` points into the source tree, which a shipped
/// binary has no business resolving.
#[cfg(test)]
fn artifact_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/codegen/xai-grok-pager.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(ARTIFACT_REL_PATH)
}

#[cfg(not(test))]
#[allow(dead_code)]
fn artifact_path() -> PathBuf {
    PathBuf::from(ARTIFACT_REL_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing the TUI renders is lost on the way out and back.
    ///
    /// [`SettingsRegistry::defaults`] is literally this round trip, so a field
    /// that lowers but does not raise would silently blank a row in the modal.
    /// Comparing the two lowered forms rather than the `SettingMeta`s compares
    /// exactly what a client would see, and needs no `PartialEq` on a type
    /// whose fields are all `&'static`.
    #[test]
    fn the_registry_survives_a_round_trip() {
        let declared = catalog_from(&crate::settings::defs::default_settings());
        let raised = catalog_from(&raise_rows(&declared));
        assert_eq!(
            raised.rows.len(),
            declared.rows.len(),
            "a declared row did not survive `raise_row`"
        );
        for (a, b) in raised.rows.iter().zip(declared.rows.iter()) {
            assert_eq!(a, b, "`{}` changed on the way through the wire", b.key);
        }
        assert_eq!(raised.categories, declared.categories);
        assert_eq!(raised.version, wire::CATALOG_VERSION);
    }

    /// The registry the pager actually renders is the raised one.
    ///
    /// Without this the round trip above could be true of a code path nothing
    /// uses.
    #[test]
    fn the_default_registry_is_the_raised_catalog() {
        let rendered = catalog_from(SettingsRegistry::defaults().all());
        assert_eq!(
            rendered,
            catalog_from(&crate::settings::defs::default_settings())
        );
    }

    /// The drift guard.
    ///
    /// It compares rather than rewrites: a generator that quietly rewrites its
    /// own output turns drift into an unreviewed diff, and there is no CI here
    /// to run `git diff --exit-code` after it. Adding a setting fails this
    /// test; hand-editing the JSON fails it too. Both are fixed by the command
    /// in the message.
    #[test]
    fn the_generated_catalog_matches_the_registry() {
        let expected = generate();
        let path = artifact_path();

        if std::env::var_os("GROK_WRITE_SETTINGS_CATALOG").is_some() {
            let dir = path.parent().expect("artifact path has a parent");
            std::fs::create_dir_all(dir).expect("create the generated directory");
            std::fs::write(&path, &expected).expect("write the generated catalog");
            return;
        }

        let actual = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!("{ARTIFACT_REL_PATH} is missing ({err}); regenerate with: {REGENERATE_CMD}")
        });
        if actual == expected {
            return;
        }

        // A whole-file diff of a few thousand generated lines buries the
        // change, so report the first line that disagrees.
        let mismatch = actual
            .lines()
            .zip(expected.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b);
        let detail = match mismatch {
            Some((index, (found, want))) => format!(
                "line {}:\n  checked in: {found}\n  from Rust:  {want}",
                index + 1
            ),
            None => format!(
                "line count differs: checked in {}, from Rust {}",
                actual.lines().count(),
                expected.lines().count()
            ),
        };
        panic!(
            "{ARTIFACT_REL_PATH} disagrees with the settings registry at {detail}\n\n\
             The registry is the definition and the shell serves this file to every \
             other client. If the catalog change is intended, regenerate with: \
             {REGENERATE_CMD}"
        );
    }

    /// The catalog's shape, pinned the way `media_gen_limits` pins its variant
    /// count.
    ///
    /// The artifact test above is satisfied by regenerating, so on its own it
    /// would wave through a row deleted by accident as a tidy diff. These are
    /// the numbers a reviewer would notice moving.
    #[test]
    fn the_catalog_shape_is_pinned() {
        let catalog = catalog_from(&crate::settings::defs::default_settings());
        assert_eq!(catalog.rows.len(), 50, "a setting was added or removed");
        assert_eq!(
            catalog.categories.len(),
            SettingCategory::ALL.len(),
            "a category exists that the catalog does not list"
        );
        let terminal = catalog
            .rows
            .iter()
            .filter(|r| r.surface == wire::SettingSurface::Terminal)
            .count();
        assert_eq!(
            terminal, 15,
            "the terminal-only set changed; a row moved in or out of every \
             non-terminal client's settings view"
        );
    }

    /// Every group row's children exist, and every row is reachable.
    ///
    /// A client builds the sub-sheet from `children`, so a dangling key there
    /// is a sub-sheet with a hole in it that only the far client would see.
    #[test]
    fn group_children_name_rows_that_exist() {
        let catalog = catalog_from(&crate::settings::defs::default_settings());
        let keys: std::collections::HashSet<&str> =
            catalog.rows.iter().map(|r| r.key.as_str()).collect();
        for row in &catalog.rows {
            if let wire::SettingKind::Group { children } = &row.kind {
                for child in children {
                    assert!(
                        keys.contains(child.as_str()),
                        "group `{}` names a child `{child}` that is not in the catalog",
                        row.key
                    );
                }
            }
        }
    }

    /// An enum row's default is one of its own choices.
    ///
    /// A client that renders the chooser from `choices` and preselects
    /// `default` would otherwise show nothing selected, and the pager would not
    /// notice because it resolves the live value instead.
    #[test]
    fn an_enum_default_is_one_of_its_choices() {
        for row in catalog_from(&crate::settings::defs::default_settings()).rows {
            if let wire::SettingKind::Enum {
                default, choices, ..
            } = &row.kind
            {
                assert!(
                    choices.iter().any(|c| &c.canonical == default),
                    "`{}` defaults to `{default}`, which is not one of its choices",
                    row.key
                );
            }
        }
    }

    /// Locks arrive with the sentence to show, not with a code to look up.
    #[test]
    fn a_lock_carries_its_own_reason() {
        let zdr = lower_lock(RowLock::CodingDataSharing(CodingDataSharingLock::Zdr));
        assert_eq!(zdr.kind, wire::SettingLockKind::CodingDataSharingZdr);
        assert!(zdr.hides_value);
        assert!(!zdr.admin_managed);
        assert!(!zdr.reason.is_empty());

        let managed = lower_lock(RowLock::CodingDataSharing(
            CodingDataSharingLock::TeamManaged,
        ));
        assert!(managed.admin_managed);
        assert!(!managed.hides_value);

        let read_only = lower_lock(RowLock::ReadOnlyConfig);
        assert_eq!(read_only.kind, wire::SettingLockKind::ReadOnlyConfig);
        // The path is the whole point of carrying the sentence.
        assert!(
            read_only.reason.contains("config.toml"),
            "the read-only reason no longer names the file: {}",
            read_only.reason
        );
    }

    #[test]
    fn a_canonical_enum_value_lowers_to_a_plain_string() {
        assert_eq!(
            lower_value(&SettingValue::Enum("groknight")),
            wire::SettingValue::String("groknight".to_string())
        );
        assert_eq!(
            lower_value(&SettingValue::Int(120)),
            wire::SettingValue::Int(120)
        );
    }

    /// The shell serves the same catalog the pager renders.
    ///
    /// The shell embeds the generated artifact and cannot see the registry, so
    /// this is the only place both are in scope. Without it the artifact test
    /// above would prove the file is current and prove nothing about what the
    /// shell does with it.
    #[test]
    fn the_shell_serves_the_registry_the_pager_renders() {
        assert_eq!(
            xai_grok_shell::settings::catalog(),
            &catalog_from(&crate::settings::defs::default_settings()),
            "the shell's embedded catalog is not the registry; regenerate with: \
             {REGENERATE_CMD}"
        );
    }

    /// Every row the shell is expected to answer for, it answers for.
    ///
    /// The shell resolves a value from the key alone — the registry's stated
    /// contract is that a shell-owned key *is* the config field name — so a new
    /// row that breaks that contract shows up here as a missing value rather
    /// than as a blank row in a browser. The three keys named below are the
    /// ones where the wire deliberately says nothing: their value is session
    /// state (a model catalog, the account) that reaches a client by its own
    /// route.
    #[test]
    fn the_shell_resolves_a_value_for_every_row_it_owns() {
        use xai_grok_shell::agent::config::UiConfig;

        const RESOLVED_BY_THE_CLIENT: &[&str] = &[
            "default_model",
            "fork_secondary_model",
            "coding_data_sharing",
        ];

        let catalog = catalog_from(&crate::settings::defs::default_settings());
        let ui = UiConfig::default();
        let state = xai_grok_shell::settings::state::resolve(&catalog.rows, &ui, None);
        for row in &catalog.rows {
            let expected = row.owner != wire::SettingOwner::Client
                && !matches!(row.kind, wire::SettingKind::Group { .. })
                && !matches!(row.kind, wire::SettingKind::DynamicEnum { .. })
                && !RESOLVED_BY_THE_CLIENT.contains(&row.key.as_str());
            assert_eq!(
                state.values.contains_key(&row.key),
                expected,
                "`{}` is {} in the shell's answer",
                row.key,
                if expected { "missing" } else { "unexpected" }
            );
        }
    }

    /// A shell-resolved default is the default the pager would show.
    ///
    /// Two resolvers over one catalog is the seam this whole change is about,
    /// so it is pinned where both are visible. It compares the default
    /// configuration only: past that the pager deliberately shows live session
    /// state (the running agent's permission mode, the session's model) where
    /// the shell reports what is on disk.
    #[test]
    fn the_two_sides_agree_on_a_default_configuration() {
        use xai_grok_shell::agent::config::UiConfig;

        let catalog = catalog_from(&crate::settings::defs::default_settings());
        let ui = UiConfig::default();
        let state = xai_grok_shell::settings::state::resolve(&catalog.rows, &ui, None);
        let pager = super::super::PagerLocalSnapshot::default();

        // Rows the pager reads from live process state rather than from the
        // configuration, so the two are answering different questions.
        const LIVE_IN_THE_PAGER: &[&str] = &[
            "permission_mode",
            "default_model",
            "fork_secondary_model",
            "coding_data_sharing",
            "voice_stt_language",
            "plan_mode",
        ];

        for row in &catalog.rows {
            if LIVE_IN_THE_PAGER.contains(&row.key.as_str()) {
                continue;
            }
            let Some(from_shell) = state.values.get(&row.key) else {
                continue;
            };
            let Some(from_pager) = super::super::current_value_for(intern(&row.key), &ui, &pager)
            else {
                continue;
            };
            assert_eq!(
                &lower_value(&from_pager),
                from_shell,
                "`{}` reads differently on the two sides of the wire",
                row.key
            );
        }
    }
}
