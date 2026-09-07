//! Wire representation of the settings catalog, its values and its locks.
//!
//! ## Why this crate exists
//!
//! The settings catalog was a Rust const table compiled into the pager, its
//! values were read straight out of `UiConfig` and process-wide render caches,
//! and its `ReadOnlyConfig` lock was a `stat` of the leader's own disk. None of
//! that is reachable by a client that is not the pager, so settings were the
//! one large surface a second client could not implement — not for want of
//! effort, but because the facts it needed were never on the wire.
//!
//! Everything here is data. There are no function pointers, no closures and no
//! host types: a client renders these structures and sends actions back.
//!
//! ## What the wire has to say, and why each part is here
//!
//! - **The catalog** ([`SettingsCatalog`]) — the schema. Static for a given
//!   build, so it is generated from the pager's declaration into a checked-in
//!   artifact and served verbatim; see `xai_grok_pager::settings::wire`.
//! - **The values** ([`SettingsState::values`]) — runtime, so they are resolved
//!   per request. A row whose value is absent is one the client itself owns
//!   (see [`SettingOwner::Client`]).
//! - **The locks** ([`SettingsState::locks`]) — a lock is *carried*, never
//!   recomputed. Two of the three sources are account state and the third is
//!   the mode of a file on the leader's machine; a browser can observe none of
//!   them, and a second client that guessed would offer to write a file the
//!   leader is going to refuse.
//!
//! ## Naming
//!
//! No name here carries a client's prefix. `SettingOwner::Client` is not "the
//! pager": it is "whichever client is rendering this row", which is what the
//! distinction actually means once there is more than one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Version of the catalog *shape*, bumped when a client that understood the
/// previous shape would misread this one.
///
/// Adding a row, a choice or a category does not bump it — those are data. It
/// is carried so a client can refuse a catalog it cannot render instead of
/// rendering it wrongly.
pub const CATALOG_VERSION: u32 = 1;

/// Prefix of every key a plugin contributes: `plugin.<plugin>.<setting>`.
///
/// A key namespace is a wire fact, not a client's private convention: the shell
/// routes a write by it and every client reads a row's provenance out of it, so
/// it is spelled once, here.
pub const PLUGIN_KEY_PREFIX: &str = "plugin.";

/// Split a plugin key into `(plugin, setting)`.
///
/// `None` for a plugin's group row (`plugin.<plugin>`, which carries no value)
/// and for every key that is not a plugin's. Plugin names are `[a-z0-9-]` and
/// setting keys `[a-z0-9_-]`, both enforced by the manifest parser, so neither
/// half carries a `.` and the split is unambiguous.
pub fn split_plugin_key(key: &str) -> Option<(&str, &str)> {
    key.strip_prefix(PLUGIN_KEY_PREFIX)?.split_once('.')
}

/// Whether `key` names a plugin-contributed row, its group row included.
pub fn is_plugin_key(key: &str) -> bool {
    key.starts_with(PLUGIN_KEY_PREFIX)
}

// ---------------------------------------------------------------------------
// Catalog: the schema half
// ---------------------------------------------------------------------------

/// Every registered setting, in render order, plus the section headers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsCatalog {
    pub version: u32,
    /// Sections in render order. A client renders one section per entry and
    /// skips a section no row lands in.
    pub categories: Vec<SettingCategoryInfo>,
    /// Rows in declaration order. Rows are grouped by [`SettingRow::category`]
    /// at render time, not here, so declaration order survives on the wire and
    /// two clients list a section's rows the same way.
    pub rows: Vec<SettingRow>,
}

/// One section of the settings list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingCategoryInfo {
    pub id: SettingCategory,
    /// Section header as rendered. Carried rather than derived from `id` so a
    /// client does not keep a second copy of the labels.
    pub label: String,
}

/// Section a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingCategory {
    Appearance,
    Mouse,
    Editor,
    Agent,
    Privacy,
    Models,
    Session,
    Advanced,
    /// Rows contributed by plugin manifests. Empty unless a loaded, trusted
    /// plugin declares `settings`.
    Plugins,
}

/// Who holds the value behind a row.
///
/// This is about storage, not about who may change it: every row a client
/// renders is a row a client may ask to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingOwner {
    /// In-memory state of the client that is rendering, never written to disk
    /// (multiline input, plan mode). No value arrives for these rows and none
    /// is expected back: the client both holds and renders them.
    Client,
    /// The shell's schema, written by the shell, read back over the wire.
    Shell,
    /// [`SettingOwner::Shell`], plus a licence for the client to keep a local
    /// cache of the value for its render hot path. The shell is still the
    /// source of truth; the cache exists so a per-frame read is not a lookup
    /// through the config.
    ShellCached,
}

/// Where a row makes sense.
///
/// Parity between clients is about *function*, not about rendering rows that
/// mean nothing. A browser has no scroll wheel to invert and no alternate
/// screen buffer to enter, so those rows are marked here rather than left for
/// each client to recognise by name — recognising by name is exactly the
/// knowledge the wire is supposed to be giving out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingSurface {
    /// Every client renders it.
    Any,
    /// The row configures the terminal front-end itself: the alternate screen,
    /// the mouse, a key chord, a column width, the frame cadence. A client that
    /// is not a terminal skips it, and skipping it is not a divergence.
    Terminal,
}

/// One choice of an [`SettingKind::Enum`] row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingChoice {
    /// The persisted string. This is what goes back in a write.
    pub canonical: String,
    /// What the chooser shows.
    pub display: String,
    /// Sub-text under the choice; empty collapses the choice to one line.
    #[serde(default)]
    pub description: String,
}

/// Constraint applied to a [`SettingKind::String`] row before it is written.
///
/// Carried so a client can reject bad input at the keystroke instead of
/// discovering it in a refusal, and so both clients reject the same input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StringValidator {
    /// Non-empty, no whitespace.
    NonEmptyToken,
    /// Must name a model in the live catalog. Empty clears the override.
    KnownModel,
    /// Any UTF-8.
    Any,
}

/// Runtime catalog a [`SettingKind::DynamicEnum`] draws its choices from.
///
/// The choices themselves are not in the catalog because they change while the
/// session runs; the client asks the named source for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DynamicEnumSource {
    /// Models of the active session, with a leading "no override" choice whose
    /// canonical is the empty string.
    ActiveModelCatalog,
}

/// Shape of a row's value, with its default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SettingKind {
    Bool {
        default: bool,
    },
    String {
        default: String,
        validator: StringValidator,
    },
    Enum {
        default: String,
        choices: Vec<SettingChoice>,
        /// Moving through the chooser applies the value live; leaving without
        /// committing reverts it. `false` means commit-on-confirm only.
        supports_preview: bool,
    },
    Int {
        default: i64,
        min: i64,
        max: i64,
    },
    DynamicEnum {
        default: String,
        source: DynamicEnumSource,
        supports_preview: bool,
    },
    /// A row that opens a sub-sheet of other rows, named by key. It carries no
    /// value of its own: no value arrives for it and none may be written to it.
    /// Its children are rendered only inside the sub-sheet.
    Group {
        children: Vec<String>,
    },
}

/// One row of the settings list.
///
/// The field set is the whole of what the pager's own row metadata carries —
/// deliberately, and enforced: the lowering that produces this type
/// destructures the pager's `SettingMeta` with no `..` rest pattern, so a field
/// added there does not compile until it is given a place here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingRow {
    /// Stable id. Also the config field name for shell-owned rows, and the key
    /// a write names.
    pub key: String,
    pub category: SettingCategory,
    pub owner: SettingOwner,
    pub surface: SettingSurface,
    pub label: String,
    pub description: String,
    /// Extra terms the search filter matches on, beyond label, description and
    /// key. Lowercase, never empty.
    pub keywords: Vec<String>,
    pub kind: SettingKind,
    /// The value applies at the next session start, not now. A client says so
    /// on the row.
    pub restart_required: bool,
    /// Hidden by the pager's minimal screen mode. The setting still exists and
    /// still applies; a client with no such mode ignores this.
    pub hidden_in_minimal: bool,
}

// ---------------------------------------------------------------------------
// State: the runtime half
// ---------------------------------------------------------------------------

/// A row's current value.
///
/// Untagged because a setting value has no shape a client could confuse: the
/// row's [`SettingKind`] already says which of these to expect, and the JSON is
/// then the value a user would recognise (`true`, `42`, `"groknight"`) rather
/// than a wrapper around it. `Enum` and `DynamicEnum` values arrive as
/// [`SettingValue::String`]; their canonical is a string on disk too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    String(String),
}

impl SettingValue {
    /// The bool behind a `Bool`, or `None` for the other shapes.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// The integer behind an `Int`, or `None` for the other shapes.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// The string behind a `String`, or `None` for the other shapes.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(v) => Some(v),
            _ => None,
        }
    }
}

/// Why the leader will not rewrite the user `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConfigReadOnly {
    /// The file denies its owner a write — what a declarative installer
    /// (home-manager, chezmoi, ansible) leaves behind.
    FileMode,
    /// `GROK_CONFIG_READONLY` asked for it.
    Env,
}

/// Why a row cannot be changed from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingLockKind {
    /// The value would be written to a `config.toml` the leader will not
    /// rewrite. Per *file*, so it locks every row that lands in that file.
    ReadOnlyConfig,
    /// The account has Zero Data Retention; the value is not the user's to set.
    CodingDataSharingZdr,
    /// A team administrator set the value.
    CodingDataSharingTeamManaged,
}

/// A lock on one row, as the client should render it.
///
/// The `reason` travels with the lock because it names a path on the leader's
/// machine — the one thing a second client is guaranteed not to know, and the
/// one thing the message has to say for the user to know where to go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingLock {
    pub kind: SettingLockKind,
    /// Replaces the row's description while the row is expanded.
    pub reason: String,
    /// The value column is replaced outright rather than shown greyed.
    pub hides_value: bool,
    /// The value is real, but an administrator set it elsewhere.
    pub admin_managed: bool,
}

/// The runtime half of the settings view: what the values are now, and which
/// rows cannot be changed.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsState {
    /// Current value per key. A key is absent when its row is
    /// [`SettingOwner::Client`] (the client holds it), when it is a
    /// [`SettingKind::Group`] (no value of its own), or when the value is not
    /// resolvable yet — a model override before the catalog has loaded.
    #[serde(default)]
    pub values: BTreeMap<String, SettingValue>,
    /// Locks per key. Absent means editable.
    #[serde(default)]
    pub locks: BTreeMap<String, SettingLock>,
    /// The user `config.toml` as a UI would show it (`~`-relative). Shown by
    /// the read-only reason and by anything else that points a user at the
    /// file.
    pub config_path: String,
    /// Set when the user `config.toml` is not the leader's to rewrite. Every
    /// row that would be written to it carries a
    /// [`SettingLockKind::ReadOnlyConfig`] lock; this says why, once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_read_only: Option<ConfigReadOnly>,
}

/// Response to `x.ai/settings/list`: the schema and the runtime state together,
/// so one round trip is enough to render the view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsListResponse {
    pub catalog: SettingsCatalog,
    pub state: SettingsState,
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// `x.ai/settings/set`: change one row's value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSetRequest {
    /// The session the change is made from. Carried for the session-scoped
    /// rows and for logging; the write itself is per user, not per session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub key: String,
    pub value: SettingValue,
}

/// Why a write did not happen.
///
/// A refusal is not an error: nothing broke, and the client should say where
/// the value lives rather than that the save failed. Errors — a disk that went
/// away mid-write — still come back as ACP errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsRefusalKind {
    /// The `config.toml` the value lives in is not the leader's to rewrite.
    /// This is the refusal a declaratively-configured machine depends on, and
    /// it is decided by the leader, never by the client.
    ReadOnlyConfig,
    /// No row by that key.
    UnknownKey,
    /// The value's shape does not match the row's kind.
    WrongKind,
    /// An `Int` outside `[min, max]`, or an `Enum` canonical that is not one of
    /// the row's choices.
    OutOfRange,
}

/// A declined write, with the sentence to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsRefusal {
    pub kind: SettingsRefusalKind,
    pub message: String,
}

/// Response to `x.ai/settings/set`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSetResponse {
    /// Whether the value was written. `false` always comes with a `refusal`.
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<SettingsRefusal>,
}

impl SettingsSetResponse {
    /// The value was written.
    pub fn applied() -> Self {
        Self {
            applied: true,
            refusal: None,
        }
    }

    /// The value was not written, and here is what to tell the user.
    pub fn refused(kind: SettingsRefusalKind, message: impl Into<String>) -> Self {
        Self {
            applied: false,
            refusal: Some(SettingsRefusal {
                kind,
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_as_the_bare_json_scalar() {
        for (value, json) in [
            (SettingValue::Bool(true), "true"),
            (SettingValue::Int(42), "42"),
            (SettingValue::String("groknight".into()), "\"groknight\""),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SettingValue>(json).unwrap(),
                value,
                "untagged variant order picked the wrong arm for {json}"
            );
        }
    }

    #[test]
    fn a_kind_names_itself_on_the_wire() {
        let json = serde_json::to_string(&SettingKind::Int {
            default: 3,
            min: 1,
            max: 10,
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"int","default":3,"min":1,"max":10}"#);
    }

    #[test]
    fn a_refusal_carries_its_message() {
        let response = SettingsSetResponse::refused(
            SettingsRefusalKind::ReadOnlyConfig,
            "Theme is set in ~/.grok/config.toml, which is read-only — change it there",
        );
        assert!(!response.applied);
        let round_tripped: SettingsSetResponse =
            serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert_eq!(round_tripped, response);
    }

    #[test]
    fn an_applied_write_carries_no_refusal_field() {
        assert_eq!(
            serde_json::to_string(&SettingsSetResponse::applied()).unwrap(),
            r#"{"applied":true}"#
        );
    }
}
