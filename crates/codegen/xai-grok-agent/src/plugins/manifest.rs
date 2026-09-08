//! The canonical manifest location is `plugin.json` at the plugin root.
//! Fallback locations (checked in order when the root manifest is absent):
//! 1. `.grok-plugin/plugin.json`
//! 2. `.claude-plugin/plugin.json`
//!
//! If no manifest is found, the plugin can still function via convention-based discovery (skills/, agents/, .mcp.json, hooks/hooks.json).
//! The plugin name is then derived from the directory name.
//!
//! The parser is forward-compatible: unknown fields are silently ignored so that manifests authored for newer upstream versions still load.

use std::path::{Path, PathBuf};

use serde::Deserialize;

const MAX_PLUGIN_NAME_LEN: usize = 64;

/// Regex pattern for valid plugin names: lowercase alphanumeric and hyphens.
fn is_valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PLUGIN_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PathOrPaths {
    Single(String),
    Multiple(Vec<String>),
}

impl PathOrPaths {
    /// Paths that escape the plugin root (via `..` components) are rejected with a warning and excluded from the result.
    pub fn resolve(&self, plugin_root: &Path) -> Vec<PathBuf> {
        let paths = match self {
            PathOrPaths::Single(p) => vec![plugin_root.join(p)],
            PathOrPaths::Multiple(ps) => ps.iter().map(|p| plugin_root.join(p)).collect(),
        };
        paths
            .into_iter()
            .filter(|resolved| {
                if is_path_contained(resolved, plugin_root) {
                    true
                } else {
                    tracing::warn!(
                        path = %resolved.display(),
                        plugin_root = %plugin_root.display(),
                        "manifest path escapes plugin root; skipping"
                    );
                    false
                }
            })
            .collect()
    }
}

/// Canonicalizes both sides (resolving symlinks and `..`) before the prefix check.
fn is_path_contained(resolved: &Path, plugin_root: &Path) -> bool {
    let canonical_root =
        dunce::canonicalize(plugin_root).unwrap_or_else(|_| plugin_root.to_path_buf());
    let canonical_resolved =
        dunce::canonicalize(resolved).unwrap_or_else(|_| resolved.to_path_buf());
    // dunce keeps the verbatim form for over-260-char paths, so this containment check fails closed; see crates/codegen/clippy.toml
    canonical_resolved.starts_with(&canonical_root)
}

/// Check whether a path *names* a location within the plugin root, without
/// asking the filesystem what is really there.
///
/// `.` is dropped and `..` pops, so `<root>/../etc/passwd` is still refused;
/// what is not refused is a symlink the plugin itself placed inside its own
/// directory. That case is the whole reason this exists: the SDK launcher a
/// TypeScript plugin names as `${GROK_PLUGIN_ROOT}/_sdk/run` is a real copy in
/// a deployed plugin but a symlink into a shared SDK checkout during
/// development, and [`is_path_contained`] would read the second as an escape
/// and refuse to start the plugin.
///
/// Only `exec`'s `argv[0]` uses this. It is not a weakening of a trust
/// boundary, because `exec` never had one: `"exec": "python3"` is a bare
/// `PATH` lookup with no containment at all, and every form runs
/// plugin-supplied code regardless. Component paths (hooks, MCP, LSP, skills)
/// keep the canonicalizing check.
fn is_lexically_contained(resolved: &Path, plugin_root: &Path) -> bool {
    fn normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other),
            }
        }
        out
    }
    normalize(resolved).starts_with(normalize(plugin_root))
}

/// Whether a manifest `exec` program names a path rather than a bare program.
///
/// `/` counts on every platform (manifests are written portably and JSON paths
/// use it even on Windows); the native separator counts too.
fn has_path_separator(program: &str) -> bool {
    program.contains('/') || program.contains(std::path::MAIN_SEPARATOR)
}

/// Whether a resolved `exec` program carries an execute bit. Non-unix has no
/// equivalent bit, so the check is vacuously true there and a bad program
/// surfaces as a spawn error instead.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// Resolve a plugin component path (hooks, MCP, LSP) from a manifest field.
///
/// If the field is `Path(p)`, resolves relative to plugin root with containment check.
/// If `Inline(_)`, returns `None` (caller reads inline value directly).
/// If `None`, checks for `default_file` at the plugin root.
fn resolve_component_path(
    field: &Option<PathOrInline>,
    plugin_root: &Path,
    default_file: &str,
    label: &str,
) -> Option<PathBuf> {
    match field {
        Some(PathOrInline::Path(p)) => {
            let resolved = plugin_root.join(p);
            if !is_path_contained(&resolved, plugin_root) {
                tracing::warn!(
                    path = %resolved.display(),
                    plugin_root = %plugin_root.display(),
                    "{label} path escapes plugin root; skipping"
                );
                return None;
            }
            resolved.is_file().then_some(resolved)
        }
        Some(PathOrInline::Inline(_)) => None,
        None => {
            let default = plugin_root.join(default_file);
            default.is_file().then_some(default)
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PathOrInline {
    Path(String),
    Inline(serde_json::Value),
}

/// A directly-executed sidecar entry (`plugin.json`'s `exec` field).
///
/// Either a bare program (`"exec": "./plugin"`) or a full argv
/// (`"exec": ["python3", "${GROK_PLUGIN_ROOT}/plugin.py"]`). The same
/// string-or-array idiom the manifest already uses for `skills`/`commands`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ExecEntry {
    /// A single program, no arguments.
    Program(String),
    /// Program plus arguments, argv-style.
    Argv(Vec<String>),
}

impl ExecEntry {
    /// The declared argv, before token substitution or resolution.
    fn argv(&self) -> Vec<String> {
        match self {
            ExecEntry::Program(p) => vec![p.clone()],
            ExecEntry::Argv(v) => v.clone(),
        }
    }
}

/// How a sidecar plugin is launched — the resolved form of the manifest's
/// `exec` field.
///
/// A plugin is an executable that speaks the wire contract, in whatever
/// language it is written. A TypeScript plugin reaches this same form through
/// the SDK's `_sdk/run` launcher, which picks a JS runtime on the far side of
/// the `exec`. See [`PluginManifest::sidecar_launch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarLaunch {
    /// Either an absolute path inside the plugin root, or a bare program name
    /// resolved on `PATH` at spawn time.
    pub program: PathBuf,
    /// Remaining argv, passed verbatim after token substitution.
    pub args: Vec<String>,
}

/// Parsed plugin manifest from `plugin.json`.
///
/// Forward-compatible: unknown fields are silently ignored because `#[serde(deny_unknown_fields)]` is not set.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    /// User-facing plugin namespace (kebab-case).  Required.
    pub name: String,
    /// Semver version string.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<Author>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,

    // ── Component path overrides (supplement convention dirs) ──────
    #[serde(default)]
    pub skills: Option<PathOrPaths>,
    #[serde(default)]
    pub commands: Option<PathOrPaths>,
    #[serde(default)]
    pub agents: Option<PathOrPaths>,
    #[serde(default)]
    pub hooks: Option<PathOrInline>,
    #[serde(default)]
    pub mcp_servers: Option<PathOrInline>,
    #[serde(default)]
    pub lsp_servers: Option<PathOrInline>,

    // ── Sidecar plugin ───────────────────────────────────────────────
    /// Program (or argv) executed as the sidecar, for a plugin written in any
    /// language: `"exec": "./plugin"` or
    /// `"exec": ["python3", "${GROK_PLUGIN_ROOT}/plugin.py"]`. A TypeScript
    /// plugin names the SDK launcher:
    /// `"exec": ["${GROK_PLUGIN_ROOT}/_sdk/run", "index.ts"]`.
    /// See [`PluginManifest::sidecar_launch`].
    #[serde(default)]
    pub exec: Option<ExecEntry>,
    /// The withdrawn `plugin` / `runtime` launch form, parsed only so a
    /// manifest still carrying it is refused by name instead of loading into a
    /// plugin whose sidecar silently never starts. The refusal is a
    /// [`ManifestError::WithdrawnLaunchField`], which discovery keeps as the
    /// plugin's
    /// [`load_error`](crate::plugins::discovery::DiscoveredPlugin::load_error)
    /// so `/plugins` can say why the plugin now does nothing.
    #[serde(default, rename = "plugin")]
    pub withdrawn_plugin: Option<serde_json::Value>,
    /// Companion of [`Self::withdrawn_plugin`]; `runtime` only ever qualified
    /// `plugin`, and the launcher on the far side of `exec` takes
    /// `--runtime=` instead.
    #[serde(default)]
    pub runtime: Option<serde_json::Value>,
    /// Whether the sidecar's child process may reach the network.
    /// Defaults to `false`; see [`PluginManifest::network_enabled`].
    ///
    /// Enforcement is a property of the *child process*, not of the program it
    /// runs, and it is keyed on this flag alone — a TypeScript sidecar and a
    /// compiled one are confined identically. What `false` is worth depends on
    /// the host:
    ///
    /// - **Linux**: a per-child seccomp filter denies
    ///   `connect`/`bind`/`sendto`/`sendmsg`/`listen`/`accept` for every
    ///   address family, `AF_UNIX` included.
    /// - **macOS**: the child is re-exec'd through `sandbox-exec` with a
    ///   `(deny network*)` Seatbelt profile, which denies the same set (a
    ///   Unix-socket `connect` counts as network there) and takes DNS with it.
    /// - **Windows and other platforms**: nothing enforces it. The sidecar is
    ///   warned about at load and started anyway, unless a sandbox profile was
    ///   requested, in which case it refuses to start.
    ///
    /// A deno sidecar additionally has `--allow-net` withheld by the SDK
    /// launcher. That is defence in depth and nothing decides anything from
    /// it: a guarantee that held only under one runtime would be a guarantee
    /// about which program the manifest happened to name.
    ///
    /// The sidecar is told the flag in its environment as
    /// `GROK_PLUGIN_NETWORK` (`1` or `0`), so a launcher between the host and
    /// the plugin can line a runtime's own permission model up with it. That
    /// is information for the child, not the enforcement above.
    #[serde(default)]
    pub network: Option<bool>,
    /// Model-visible tools the sidecar serves via `tool_invoke`. The manifest
    /// is the source of truth for the tool catalog (built before any sidecar
    /// starts, so lazy sidecar start survives); the SDK-side handler map is
    /// cross-checked against it at handshake. See
    /// [`PluginManifest::sidecar_tools`].
    #[serde(default)]
    pub tools: Option<Vec<ManifestToolSpec>>,
    /// Slash commands the sidecar serves via `command_invoke` — a plugin
    /// command that runs the plugin's *code*, as opposed to the `commands`
    /// field above, which points at a directory of markdown whose body is
    /// substituted into the user's message.
    ///
    /// Both may be declared at once. On a name collision the markdown command
    /// keeps the bare name (it is the older contract and needs no trust) and
    /// this one is advertised as `<plugin>:<name>`; see
    /// `EffectivePluginCommandCatalog` in the shell. Like `tools`, the manifest
    /// is the source of truth: the catalog is built before any sidecar starts,
    /// so a lazily-started plugin still appears in the `/` menu.
    #[serde(default)]
    pub slash_commands: Option<Vec<ManifestSlashCommandSpec>>,
    /// Default per-plugin configuration object (`plugin.json`'s `config`).
    /// Surfaced to the sidecar at `initialize` and via `config_get`; user
    /// `[plugins.<name>]` entries from config.toml are shallow-merged over
    /// these defaults by the shell's session plugin-host wiring. Defaults to
    /// `{}` when absent. See [`PluginManifest::sidecar_config_defaults`].
    #[serde(default)]
    pub config: Option<serde_json::Value>,

    /// Preferences the plugin contributes to grok's settings modal
    /// (`plugin.json`'s `settings`). Schema only: the *value* of each row is
    /// stored by grok under `[plugins.<name>]` in config.toml — the same table
    /// [`Self::config`] supplies defaults for, and the same object the sidecar
    /// already receives at `initialize` and via `config_get`. See
    /// [`PluginManifest::plugin_settings`].
    #[serde(default)]
    pub settings: Option<Vec<ManifestSettingSpec>>,

    /// Human-facing login label advertised when the plugin offers an
    /// interactive OAuth sign-in. When set AND the plugin subscribes to the
    /// `start_oauth_flow` hook event, the plugin becomes a selectable `/login`
    /// provider whose entry shows this label (e.g. `"Sign in with Acme"`).
    /// Absent by default — a plugin that does not opt in is never advertised as
    /// a login provider. See [`PluginManifest::oauth_login_label`].
    #[serde(default)]
    pub oauth_label: Option<String>,

    /// Accounts the plugin holds for its provider, each advertised as its own
    /// `/login` entry (`{"id": "...", "label": "..."}`). Absent or empty means
    /// the plugin gets exactly one account-less entry — the behaviour before
    /// accounts existed. See [`PluginManifest::oauth_login_accounts`].
    #[serde(default)]
    pub oauth_accounts: Option<Vec<ManifestOauthAccount>>,
}

/// One entry of a manifest's `oauthAccounts` array.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestOauthAccount {
    /// Stable selector for this account, handed back to the plugin as the
    /// `ownerHint` of the credential events it serves.
    pub id: String,
    /// Human-facing account name shown next to the provider in the picker.
    /// Defaults to `id` when absent or blank.
    #[serde(default)]
    pub label: Option<String>,
}

/// A validated account a plugin advertises as its own `/login` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthAccount {
    pub id: String,
    pub label: String,
}

/// Max length of an OAuth account id.
const MAX_OAUTH_ACCOUNT_ID_LEN: usize = 64;

/// Whether `id` is a usable OAuth account selector: 1-64 chars, not blank, no
/// ASCII control characters, and no `#` — the shell's method id is
/// `plugin-oauth:<plugin>#<account>`, so a `#` inside the account would make it
/// ambiguous to split.
fn is_valid_oauth_account_id(id: &str) -> bool {
    !id.trim().is_empty()
        && id.len() <= MAX_OAUTH_ACCOUNT_ID_LEN
        && !id.contains('#')
        && !id.chars().any(|c| c.is_ascii_control())
}

/// One entry of a manifest's `settings` array: a preference the plugin
/// contributes to grok's settings modal.
///
/// Schema only. The *value* never lives in the manifest — grok stores it under
/// `[plugins.<name>]` in config.toml, the table the manifest's `config`
/// supplies defaults for and the sidecar already receives at `initialize` and
/// via `config_get`. A plugin therefore reads its own setting exactly the way
/// it reads the rest of its config, and nothing new is needed on the wire.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSettingSpec {
    /// Key inside the plugin's `[plugins.<name>]` table.
    pub key: String,
    /// Row label in the settings modal. Defaults to `key`.
    #[serde(default)]
    pub label: Option<String>,
    /// Row description. Defaults to empty.
    #[serde(default)]
    pub description: Option<String>,
    /// `bool` | `string` | `int` | `enum`. Inferred from `default` when absent.
    #[serde(default, rename = "type")]
    pub value_type: Option<String>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Choices for an `enum` setting.
    #[serde(default)]
    pub choices: Option<Vec<ManifestSettingChoice>>,
    /// Inclusive bounds for an `int` setting. Both are required: the modal's
    /// stepper derives its step sizes from `max - min`, so an unbounded int row
    /// has no usable interaction to offer.
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
}

/// One choice of an `enum` plugin setting.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSettingChoice {
    /// Value persisted into `[plugins.<name>]`.
    pub value: String,
    /// Display label. Defaults to `value`.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// A validated choice of an `enum` plugin setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSettingChoice {
    pub value: String,
    pub label: String,
    pub description: String,
}

/// The value shape of a validated plugin setting, carrying its default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSettingKind {
    Bool {
        default: bool,
    },
    String {
        default: String,
    },
    Int {
        default: i64,
        min: i64,
        max: i64,
    },
    Enum {
        default: String,
        choices: Vec<PluginSettingChoice>,
    },
}

/// A validated plugin-contributed setting, ready to become a settings row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSettingSpec {
    pub key: String,
    pub label: String,
    pub description: String,
    pub kind: PluginSettingKind,
}

/// Max number of settings one plugin may contribute. The settings modal is a
/// finite list a person scrolls; a plugin that wants more of it than this is
/// not contributing rows, it is annexing the surface.
const MAX_PLUGIN_SETTINGS: usize = 32;

/// Max number of choices in an `enum` plugin setting. Matches the picker's own
/// bound (`MAX_PICKER_CHOICES` in the settings modal).
const MAX_PLUGIN_SETTING_CHOICES: usize = 32;

/// Max length of a plugin setting key.
const MAX_SETTING_KEY_LEN: usize = 64;

/// Max length of plugin-supplied display text (label, description, choice text).
const MAX_SETTING_TEXT_LEN: usize = 200;

/// Whether `key` is a usable setting key: 1-64 chars of `[a-z0-9_-]`.
///
/// No `.`: the pager's registry key is `plugin.<plugin>.<key>`, and both plugin
/// names and these keys being dot-free is what makes that split unambiguous.
/// Lowercase for the same reason plugin names are, and because this is also the
/// TOML key written under `[plugins.<name>]`.
fn is_valid_setting_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_SETTING_KEY_LEN
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Trim plugin-supplied display text and cap its length.
///
/// Render safety (control/bidi scrubbing) is deliberately NOT done here: the
/// surface that draws the row applies the one shared predicate every other
/// untrusted-text site uses, and duplicating a weaker copy here would be the
/// version that drifts.
fn setting_text(raw: Option<&str>, fallback: &str) -> String {
    let text = raw
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(fallback);
    match text.char_indices().nth(MAX_SETTING_TEXT_LEN) {
        Some((idx, _)) => text[..idx].to_string(),
        None => text.to_string(),
    }
}

/// One model-visible tool declared in a sidecar plugin's manifest (`tools`
/// array). The shell registers it in the session tool catalog under the
/// MCP-style qualified name `<plugin>__<name>`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestToolSpec {
    /// Bare tool name (no plugin prefix). Same charset as an MCP tool name;
    /// must not contain `__` (the qualified-name delimiter).
    pub name: String,
    /// Description shown to the model.
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema for the tool input. Defaults to an open object schema.
    #[serde(default)]
    pub input_schema: Option<serde_json::Value>,
    /// Per-tool `tool_invoke` deadline override in milliseconds (0/absent →
    /// the host default).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// A validated sidecar tool ready for catalog registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// `0` → the host's default tool timeout.
    pub timeout_ms: u64,
}

/// One slash command declared in a sidecar plugin's manifest (`slashCommands`
/// array). The shell advertises it in the `/` menu and dispatches it to the
/// plugin's sidecar over `command_invoke`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSlashCommandSpec {
    /// Bare command name, with no leading `/` and no plugin prefix.
    pub name: String,
    /// One-line description shown in the `/` menu.
    #[serde(default)]
    pub description: Option<String>,
    /// Free-text usage hint rendered beside the name, the same `argument-hint`
    /// idiom skills and builtins use. Purely a display string: arguments reach
    /// the plugin as the unparsed remainder of the line.
    #[serde(default)]
    pub argument_hint: Option<String>,
    /// Per-command `command_invoke` deadline override in milliseconds
    /// (0/absent → the host default).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// A validated sidecar slash command ready for catalog advertising.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarCommandSpec {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    /// `0` → the host's default command timeout.
    pub timeout_ms: u64,
}

/// Max length of a bare sidecar tool name.
const MAX_TOOL_NAME_LEN: usize = 64;

/// Whether `name` is a valid bare sidecar tool name: 1-64 chars of
/// `[a-zA-Z0-9_-]`, without the `__` qualified-name delimiter (which would
/// make `<plugin>__<tool>` ambiguous to split).
fn is_valid_sidecar_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_NAME_LEN
        && !name.contains("__")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Max length of a bare sidecar slash-command name.
const MAX_COMMAND_NAME_LEN: usize = 64;

/// Whether `name` is a valid bare sidecar slash-command name: 1-64 chars of
/// `[a-zA-Z0-9_-]`. `:` is excluded because it is the separator of the
/// `<plugin>:<name>` qualified form the catalog falls back to, and a name
/// containing one could not be told apart from an already-qualified spelling.
fn is_valid_sidecar_command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_COMMAND_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl PluginManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !is_valid_plugin_name(&self.name) {
            return Err(ManifestError::InvalidName {
                name: self.name.clone(),
                reason: format!(
                    "must be 1-{MAX_PLUGIN_NAME_LEN} chars, lowercase alphanumeric + hyphens, \
                     no leading/trailing hyphens"
                ),
            });
        }
        // A manifest still on the withdrawn launch form is refused outright.
        // Ignoring the field would load the plugin's skills and hooks while its
        // sidecar — the reason most of these plugins exist — never started, and
        // nothing would say why. The plugin stays listed with this error as its
        // `load_error`, so the refusal is answerable rather than a disappearance.
        if self.withdrawn_plugin.is_some() {
            return Err(ManifestError::WithdrawnLaunchField {
                name: self.name.clone(),
                field: "plugin",
            });
        }
        if self.runtime.is_some() {
            return Err(ManifestError::WithdrawnLaunchField {
                name: self.name.clone(),
                field: "runtime",
            });
        }
        Ok(())
    }

    pub fn skill_dirs(&self, plugin_root: &Path) -> Vec<PathBuf> {
        resolve_dirs(&self.skills, plugin_root, "skills")
    }

    pub fn command_dirs(&self, plugin_root: &Path) -> Vec<PathBuf> {
        resolve_dirs(&self.commands, plugin_root, "commands")
    }

    pub fn agent_dirs(&self, plugin_root: &Path) -> Vec<PathBuf> {
        resolve_dirs(&self.agents, plugin_root, "agents")
    }

    /// Returns the manifest-specified path or the default `hooks/hooks.json`.
    pub fn hooks_path(&self, plugin_root: &Path) -> Option<PathBuf> {
        resolve_component_path(&self.hooks, plugin_root, "hooks/hooks.json", "hooks")
    }

    pub fn mcp_config_path(&self, plugin_root: &Path) -> Option<PathBuf> {
        if matches!(self.mcp_servers, Some(PathOrInline::Inline(_))) {
            let default = plugin_root.join(".mcp.json");
            return default.is_file().then_some(default);
        }
        resolve_component_path(&self.mcp_servers, plugin_root, ".mcp.json", "MCP config")
    }

    /// The runtime parses and executes inline hooks via `parse_plugin_hooks_from_value()`.
    pub fn inline_hooks(&self) -> Option<&serde_json::Value> {
        match &self.hooks {
            Some(PathOrInline::Inline(v)) => Some(v),
            _ => None,
        }
    }

    /// The runtime parses and starts inline MCP servers via `load_plugin_mcp_servers_from_value()`.
    pub fn inline_mcp_servers(&self) -> Option<&serde_json::Value> {
        match &self.mcp_servers {
            Some(PathOrInline::Inline(v)) => Some(v),
            _ => None,
        }
    }

    pub fn lsp_config_path(&self, plugin_root: &Path) -> Option<PathBuf> {
        resolve_component_path(&self.lsp_servers, plugin_root, ".lsp.json", "LSP config")
    }

    pub fn inline_lsp_servers(&self) -> Option<&serde_json::Value> {
        match &self.lsp_servers {
            Some(PathOrInline::Inline(v)) => Some(v),
            _ => None,
        }
    }

    /// Whether the manifest declares a sidecar entry (`exec`).
    pub fn has_sidecar(&self) -> bool {
        self.exec.is_some()
    }

    /// Effective network flag: the manifest's `network`, or `false` when unset.
    pub fn network_enabled(&self) -> bool {
        self.network.unwrap_or(false)
    }

    /// The interactive-login label the plugin advertises, when it opts in via
    /// the manifest `oauthLabel` field. `None` (the default) means the plugin is
    /// not advertised as a `/login` provider.
    pub fn oauth_login_label(&self) -> Option<&str> {
        self.oauth_label.as_deref()
    }

    /// Validated accounts from the manifest's `oauthAccounts` array, in
    /// declaration order.
    ///
    /// Empty means "one account-less `/login` entry" — the behaviour before
    /// accounts existed — which is also what an absent or empty array, or a
    /// manifest that never opted into interactive login (`oauthLabel`), yields.
    ///
    /// Follows the component-loading convention: an entry with an unusable id
    /// (blank, over-long, containing `#` or a control character) or a duplicate
    /// id is warned about and skipped rather than failing the whole plugin. A
    /// missing or blank `label` falls back to the id.
    pub fn oauth_login_accounts(&self) -> Vec<OauthAccount> {
        let Some(accounts) = &self.oauth_accounts else {
            return Vec::new();
        };
        if self.oauth_login_label().is_none() {
            if !accounts.is_empty() {
                tracing::warn!(
                    plugin = %self.name,
                    "manifest declares oauthAccounts but no `oauthLabel`; ignoring them"
                );
            }
            return Vec::new();
        }
        let mut out: Vec<OauthAccount> = Vec::new();
        for account in accounts {
            if !is_valid_oauth_account_id(&account.id) {
                tracing::warn!(
                    plugin = %self.name,
                    account = %account.id,
                    "skipping oauth account with invalid id (1-{MAX_OAUTH_ACCOUNT_ID_LEN} \
                     non-blank chars, no `#`, no control characters)"
                );
                continue;
            }
            if out.iter().any(|a| a.id == account.id) {
                tracing::warn!(plugin = %self.name, account = %account.id,
                    "skipping duplicate oauth account declaration");
                continue;
            }
            let label = account
                .label
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .unwrap_or(&account.id);
            out.push(OauthAccount {
                id: account.id.clone(),
                label: label.to_string(),
            });
        }
        out
    }

    /// Manifest-declared default config object for the sidecar, or `{}` when
    /// absent. A non-object `config` value is ignored (warned), matching the
    /// tolerant component-loading convention — the merge layer and SDK
    /// `ctx.config()` always see a JSON object.
    pub fn sidecar_config_defaults(&self) -> serde_json::Value {
        match &self.config {
            Some(v) if v.is_object() => v.clone(),
            Some(_) => {
                tracing::warn!(
                    plugin = %self.name,
                    "manifest `config` is not a JSON object; ignoring"
                );
                serde_json::json!({})
            }
            None => serde_json::json!({}),
        }
    }

    /// Validated sidecar tools from the manifest's `tools` array.
    ///
    /// Follows the component-loading convention: invalid entries (bad name,
    /// non-object schema) are warned about and skipped rather than failing
    /// the whole plugin, and a `tools` array without a sidecar entry is
    /// meaningless (nothing would serve `tool_invoke`) so it yields nothing.
    pub fn sidecar_tools(&self) -> Vec<SidecarToolSpec> {
        let Some(tools) = &self.tools else {
            return Vec::new();
        };
        if !self.has_sidecar() {
            if !tools.is_empty() {
                tracing::warn!(
                    plugin = %self.name,
                    "manifest declares tools but no sidecar entry (`exec`); ignoring them"
                );
            }
            return Vec::new();
        }
        let mut out: Vec<SidecarToolSpec> = Vec::new();
        for tool in tools {
            if !is_valid_sidecar_tool_name(&tool.name) {
                tracing::warn!(
                    plugin = %self.name,
                    tool = %tool.name,
                    "skipping sidecar tool with invalid name (1-{MAX_TOOL_NAME_LEN} chars of \
                     [a-zA-Z0-9_-], no `__`)"
                );
                continue;
            }
            if out.iter().any(|t| t.name == tool.name) {
                tracing::warn!(plugin = %self.name, tool = %tool.name,
                    "skipping duplicate sidecar tool declaration");
                continue;
            }
            let input_schema = match &tool.input_schema {
                Some(schema) if schema.is_object() => schema.clone(),
                Some(_) => {
                    tracing::warn!(plugin = %self.name, tool = %tool.name,
                        "skipping sidecar tool: inputSchema must be a JSON object");
                    continue;
                }
                None => serde_json::json!({ "type": "object", "properties": {} }),
            };
            out.push(SidecarToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone().unwrap_or_default(),
                input_schema,
                timeout_ms: tool.timeout_ms.unwrap_or(0),
            });
        }
        out
    }

    /// Validated sidecar slash commands from the manifest's `slashCommands`
    /// array.
    ///
    /// Follows the component-loading convention `sidecar_tools` sets: an
    /// invalid entry (bad name, duplicate) is warned about and skipped rather
    /// than failing the whole plugin, and a `slashCommands` array without a
    /// sidecar entry yields nothing — with no `exec` there is nothing that
    /// could serve `command_invoke`, so the menu entry would only ever error.
    pub fn sidecar_commands(&self) -> Vec<SidecarCommandSpec> {
        let Some(commands) = &self.slash_commands else {
            return Vec::new();
        };
        if !self.has_sidecar() {
            if !commands.is_empty() {
                tracing::warn!(
                    plugin = %self.name,
                    "manifest declares slashCommands but no sidecar entry (`exec`); ignoring them"
                );
            }
            return Vec::new();
        }
        let mut out: Vec<SidecarCommandSpec> = Vec::new();
        for command in commands {
            if !is_valid_sidecar_command_name(&command.name) {
                tracing::warn!(
                    plugin = %self.name,
                    command = %command.name,
                    "skipping sidecar slash command with invalid name \
                     (1-{MAX_COMMAND_NAME_LEN} chars of [a-zA-Z0-9_-])"
                );
                continue;
            }
            if out.iter().any(|c| c.name == command.name) {
                tracing::warn!(plugin = %self.name, command = %command.name,
                    "skipping duplicate sidecar slash command declaration");
                continue;
            }
            out.push(SidecarCommandSpec {
                name: command.name.clone(),
                description: command.description.clone().unwrap_or_default(),
                argument_hint: command
                    .argument_hint
                    .as_deref()
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(str::to_string),
                timeout_ms: command.timeout_ms.unwrap_or(0),
            });
        }
        out
    }

    /// Validated preferences from the manifest's `settings` array.
    ///
    /// Follows the component-loading convention: an unusable entry (bad key,
    /// duplicate key, a kind whose default does not match it, an `int` without
    /// bounds, an `enum` without choices) is warned about and skipped rather
    /// than failing the whole plugin.
    ///
    /// A `settings` array without a sidecar entry yields nothing, for the same
    /// reason `tools` does: the value is delivered to the plugin as part of the
    /// object `initialize` and `config_get` hand the sidecar, so with no
    /// sidecar there is nothing that could ever read the row the user just set.
    pub fn plugin_settings(&self) -> Vec<PluginSettingSpec> {
        let Some(settings) = &self.settings else {
            return Vec::new();
        };
        if !self.has_sidecar() {
            if !settings.is_empty() {
                tracing::warn!(
                    plugin = %self.name,
                    "manifest declares settings but no sidecar entry (`exec`); nothing would \
                     read them, ignoring"
                );
            }
            return Vec::new();
        }
        let mut out: Vec<PluginSettingSpec> = Vec::new();
        for spec in settings {
            if out.len() >= MAX_PLUGIN_SETTINGS {
                tracing::warn!(
                    plugin = %self.name,
                    "manifest declares more than {MAX_PLUGIN_SETTINGS} settings; \
                     ignoring the rest"
                );
                break;
            }
            if !is_valid_setting_key(&spec.key) {
                tracing::warn!(
                    plugin = %self.name,
                    setting = %spec.key,
                    "skipping plugin setting with invalid key (1-{MAX_SETTING_KEY_LEN} chars of \
                     [a-z0-9_-])"
                );
                continue;
            }
            if out.iter().any(|s| s.key == spec.key) {
                tracing::warn!(plugin = %self.name, setting = %spec.key,
                    "skipping duplicate plugin setting declaration");
                continue;
            }
            let Some(kind) = self.resolve_setting_kind(spec) else {
                continue;
            };
            out.push(PluginSettingSpec {
                label: setting_text(spec.label.as_deref(), &spec.key),
                description: setting_text(spec.description.as_deref(), ""),
                key: spec.key.clone(),
                kind,
            });
        }
        out
    }

    /// Resolve one `settings` entry's declared (or inferred) value shape.
    /// `None` — with a warning — when the declaration does not describe a row
    /// the modal could render.
    fn resolve_setting_kind(&self, spec: &ManifestSettingSpec) -> Option<PluginSettingKind> {
        let declared = spec.value_type.as_deref().map(str::trim);
        // An absent `type` is inferred from `default`, so the common one-line
        // declaration (`{"key": "verbose", "default": false}`) needs no type.
        let resolved = match declared {
            Some(t) => t.to_ascii_lowercase(),
            None => match &spec.default {
                Some(serde_json::Value::Bool(_)) => "bool".to_string(),
                Some(serde_json::Value::Number(n)) if n.is_i64() => "int".to_string(),
                Some(serde_json::Value::String(_)) if spec.choices.is_some() => "enum".to_string(),
                Some(serde_json::Value::String(_)) => "string".to_string(),
                _ => {
                    tracing::warn!(plugin = %self.name, setting = %spec.key,
                        "skipping plugin setting: no `type` and no `default` to infer one from");
                    return None;
                }
            },
        };
        match resolved.as_str() {
            "bool" | "boolean" => Some(PluginSettingKind::Bool {
                default: spec
                    .default
                    .as_ref()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }),
            "string" => Some(PluginSettingKind::String {
                default: spec
                    .default
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            }),
            "int" | "integer" => {
                let (Some(min), Some(max)) = (spec.min, spec.max) else {
                    tracing::warn!(plugin = %self.name, setting = %spec.key,
                        "skipping int plugin setting: both `min` and `max` are required");
                    return None;
                };
                if min >= max {
                    tracing::warn!(plugin = %self.name, setting = %spec.key,
                        "skipping int plugin setting: `min` must be below `max`");
                    return None;
                }
                let default = spec
                    .default
                    .as_ref()
                    .and_then(|v| v.as_i64())
                    .unwrap_or(min)
                    .clamp(min, max);
                Some(PluginSettingKind::Int { default, min, max })
            }
            "enum" => {
                let choices = self.resolve_setting_choices(spec)?;
                // An out-of-catalog default would render as a value the picker
                // cannot select; the first choice is the one the plugin listed
                // first, which is the closest thing to an intended default.
                let declared_default = spec.default.as_ref().and_then(|v| v.as_str());
                let default = declared_default
                    .filter(|d| choices.iter().any(|c| c.value == *d))
                    .unwrap_or(&choices[0].value)
                    .to_string();
                if declared_default.is_some_and(|d| d != default) {
                    tracing::warn!(plugin = %self.name, setting = %spec.key,
                        "enum plugin setting default is not one of its choices; \
                         using the first choice");
                }
                Some(PluginSettingKind::Enum { default, choices })
            }
            other => {
                tracing::warn!(plugin = %self.name, setting = %spec.key, kind = %other,
                    "skipping plugin setting with unknown `type` \
                     (bool | string | int | enum)");
                None
            }
        }
    }

    /// Validated, de-duplicated choices of an `enum` setting. `None` when the
    /// declaration leaves the picker with nothing to show.
    fn resolve_setting_choices(
        &self,
        spec: &ManifestSettingSpec,
    ) -> Option<Vec<PluginSettingChoice>> {
        let mut out: Vec<PluginSettingChoice> = Vec::new();
        for choice in spec.choices.iter().flatten() {
            if out.len() >= MAX_PLUGIN_SETTING_CHOICES {
                tracing::warn!(plugin = %self.name, setting = %spec.key,
                    "enum plugin setting declares more than \
                     {MAX_PLUGIN_SETTING_CHOICES} choices; ignoring the rest");
                break;
            }
            let value = choice.value.trim();
            if value.is_empty() || value.len() > MAX_SETTING_TEXT_LEN {
                tracing::warn!(plugin = %self.name, setting = %spec.key,
                    "skipping enum choice with an empty or over-long value");
                continue;
            }
            if out.iter().any(|c| c.value == value) {
                continue;
            }
            out.push(PluginSettingChoice {
                label: setting_text(choice.label.as_deref(), value),
                description: setting_text(choice.description.as_deref(), ""),
                value: value.to_string(),
            });
        }
        if out.is_empty() {
            tracing::warn!(plugin = %self.name, setting = %spec.key,
                "skipping enum plugin setting with no usable `choices`");
            return None;
        }
        Some(out)
    }

    /// Resolve how this plugin's sidecar is launched, from the manifest's
    /// `exec` field.
    ///
    /// `plugin_data` is the plugin's data directory, for `${GROK_PLUGIN_DATA}`
    /// substitution inside `exec`; see [`Self::sidecar_exec_command`].
    pub fn sidecar_launch(&self, plugin_root: &Path, plugin_data: &str) -> Option<SidecarLaunch> {
        let (program, args) = self.sidecar_exec_command(plugin_root, plugin_data)?;
        Some(SidecarLaunch { program, args })
    }

    /// Resolve the `exec` field into a `(program, args)` pair.
    ///
    /// Every argv element goes through the shared plugin-token substitution
    /// first, because the sidecar's working directory is the *workspace* root,
    /// not the plugin root: a bare `./plugin.py` argument would be resolved by
    /// the child against the workspace. `${GROK_PLUGIN_ROOT}/plugin.py` is the
    /// way to name a file that ships with the plugin.
    ///
    /// `argv[0]` is then resolved one of two ways:
    ///
    /// - it contains a path separator → a file that must be named inside the
    ///   plugin root and be executable, resolved to an absolute path so the
    ///   workspace cwd cannot reinterpret it;
    /// - it is a bare name (`python3`, `uv`) → left alone and looked up on
    ///   `PATH` at spawn time. A plugin is arbitrary code either way; the trust
    ///   decision is the plugin's, not the argv's — which is also why the
    ///   containment above is lexical (see [`is_lexically_contained`]) and not
    ///   the canonicalizing check the component paths use.
    pub fn sidecar_exec_command(
        &self,
        plugin_root: &Path,
        plugin_data: &str,
    ) -> Option<(PathBuf, Vec<String>)> {
        let exec = self.exec.as_ref()?;
        let root_str = plugin_root.to_string_lossy();
        let argv: Vec<String> = exec
            .argv()
            .iter()
            .map(|a| substitute_env_vars(a, &root_str, plugin_data))
            .collect();
        let Some((raw_program, args)) = argv.split_first() else {
            tracing::warn!(plugin = %self.name, "manifest `exec` is empty; skipping");
            return None;
        };
        if raw_program.is_empty() {
            tracing::warn!(plugin = %self.name, "manifest `exec` program is empty; skipping");
            return None;
        }

        let program = if has_path_separator(raw_program) {
            // Absolute paths land here too: `join` with an absolute path yields
            // it unchanged, and containment then decides — after token
            // substitution `${GROK_PLUGIN_ROOT}/bin/tool` *is* absolute, and
            // containment is the property actually worth enforcing.
            let resolved = plugin_root.join(raw_program);
            if !is_lexically_contained(&resolved, plugin_root) {
                tracing::warn!(
                    plugin = %self.name,
                    path = %resolved.display(),
                    plugin_root = %plugin_root.display(),
                    "`exec` program escapes the plugin root; skipping"
                );
                return None;
            }
            if !resolved.is_file() {
                tracing::warn!(
                    plugin = %self.name,
                    path = %resolved.display(),
                    "`exec` program does not exist; skipping"
                );
                return None;
            }
            if !is_executable(&resolved) {
                tracing::warn!(
                    plugin = %self.name,
                    path = %resolved.display(),
                    "`exec` program is not executable; skipping"
                );
                return None;
            }
            resolved
        } else {
            // Bare name: `Command::new` searches `PATH` at spawn. Resolving it
            // here instead would only move the failure earlier while adding a
            // TOCTOU window.
            PathBuf::from(raw_program)
        };
        Some((program, args.to_vec()))
    }

    /// Log informational messages about manifest features.
    ///
    /// Called during discovery. Inline hooks and MCP servers are now
    /// Called during discovery; logs when inline hooks, MCP servers, or LSP servers are detected.
    pub fn warn_unsupported_features(&self, plugin_name: &str) {
        if self.inline_hooks().is_some() {
            tracing::info!(plugin = plugin_name, "plugin uses inline hooks in manifest");
        }
        if self.inline_mcp_servers().is_some() {
            tracing::info!(
                plugin = plugin_name,
                "plugin uses inline mcpServers in manifest"
            );
        }
        if self.inline_lsp_servers().is_some() {
            tracing::info!(
                plugin = plugin_name,
                "plugin uses inline lspServers in manifest"
            );
        }
        if self.exec.is_some() {
            tracing::info!(
                plugin = plugin_name,
                network = self.network_enabled(),
                "plugin declares a sidecar entry (`exec`)"
            );
        }
    }
}

fn resolve_dirs(
    field: &Option<PathOrPaths>,
    plugin_root: &Path,
    default_name: &str,
) -> Vec<PathBuf> {
    match field {
        Some(paths) => paths.resolve(plugin_root),
        None => {
            let default = plugin_root.join(default_name);
            if default.is_dir() {
                vec![default]
            } else {
                vec![]
            }
        }
    }
}

// ── Manifest loading ──────────────────────────────────────────────────

/// Manifest search order within a plugin directory.
const MANIFEST_PATHS: &[&str] = &[
    "plugin.json",
    ".grok-plugin/plugin.json",
    ".claude-plugin/plugin.json",
];

#[derive(Debug)]
pub enum ManifestLoadResult {
    Found(Box<PluginManifest>),
    /// No manifest file found; the plugin uses convention-based discovery.
    NotFound,
}

/// Tries manifest files in priority order (see [`MANIFEST_PATHS`]).
pub fn load_manifest(plugin_root: &Path) -> Result<ManifestLoadResult, ManifestError> {
    for rel_path in MANIFEST_PATHS {
        let manifest_path = plugin_root.join(rel_path);
        if manifest_path.is_file() {
            let content =
                std::fs::read_to_string(&manifest_path).map_err(|e| ManifestError::IoError {
                    path: manifest_path.clone(),
                    source: e,
                })?;
            let manifest: PluginManifest =
                serde_json::from_str(&content).map_err(|e| ManifestError::ParseError {
                    path: manifest_path.clone(),
                    message: e.to_string(),
                })?;
            manifest.validate()?;
            manifest.warn_unsupported_features(&manifest.name);
            return Ok(ManifestLoadResult::Found(Box::new(manifest)));
        }
    }
    Ok(ManifestLoadResult::NotFound)
}

/// Sanitizes the directory name to match the kebab-case constraint: lowercase, alphanumeric and hyphens, no leading/trailing hyphens.
pub fn name_from_dirname(dir: &Path) -> Option<String> {
    let dirname = dir.file_name()?.to_str()?;
    let sanitized: String = dirname
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('-').to_string();
    if trimmed.is_empty() || trimmed.len() > MAX_PLUGIN_NAME_LEN {
        return None;
    }
    Some(trimmed)
}

/// Replaces `${GROK_PLUGIN_ROOT}`, `${CLAUDE_PLUGIN_ROOT}`, `${GROK_PLUGIN_DATA}`, and `${CLAUDE_PLUGIN_DATA}` with the provided values.
/// Delegates to [`xai_grok_tools::util::substitute_plugin_tokens`], which plugin skill and command bodies also use.
pub fn substitute_env_vars(s: &str, plugin_root: &str, plugin_data: &str) -> String {
    xai_grok_tools::util::substitute_plugin_tokens(s, Some(plugin_root), Some(plugin_data))
}

pub fn normalize_inline_mcp_servers(value: &serde_json::Value) -> serde_json::Value {
    let inner = match value.get("mcpServers") {
        Some(servers) if servers.is_object() => servers.clone(),
        _ => value.clone(),
    };
    serde_json::json!({ "mcpServers": inner })
}

// ── Errors ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("invalid plugin name {name:?}: {reason}")]
    InvalidName { name: String, reason: String },

    #[error("failed to read {path}: {source}")]
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {message}")]
    ParseError { path: PathBuf, message: String },

    #[error(
        "plugin {name:?} declares the withdrawn manifest field `{field}`: a sidecar is now \
         launched by `exec` alone. Replace `\"plugin\": \"./index.ts\"` (and any `\"runtime\"`) \
         with `\"exec\": [\"${{GROK_PLUGIN_ROOT}}/_sdk/run\", \"index.ts\"]`, which finds a JS \
         runtime the same way — add `--runtime=bun` before the entry to pin one"
    )]
    WithdrawnLaunchField { name: String, field: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_plugin_names() {
        assert!(is_valid_plugin_name("my-plugin"));
        assert!(is_valid_plugin_name("a"));
        assert!(is_valid_plugin_name("deployment-tools"));
        assert!(is_valid_plugin_name("plugin123"));
        assert!(is_valid_plugin_name("a-b-c"));
    }

    #[test]
    fn invalid_plugin_names() {
        assert!(!is_valid_plugin_name(""));
        assert!(!is_valid_plugin_name("-start"));
        assert!(!is_valid_plugin_name("end-"));
        assert!(!is_valid_plugin_name("UPPER"));
        assert!(!is_valid_plugin_name("has space"));
        assert!(!is_valid_plugin_name("has_underscore"));
        assert!(!is_valid_plugin_name("has.dot"));
        assert!(!is_valid_plugin_name(&"a".repeat(65)));
    }

    #[test]
    fn parse_minimal_manifest() {
        let json = r#"{"name": "my-plugin"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "my-plugin");
        assert!(manifest.version.is_none());
        assert!(manifest.description.is_none());
        assert!(manifest.skills.is_none());
        manifest.validate().unwrap();
    }

    #[test]
    fn parse_full_manifest() {
        let json = r#"{
            "name": "deployment-tools",
            "version": "1.2.0",
            "description": "Tools for deployment",
            "author": {"name": "Test", "email": "test@example.com"},
            "homepage": "https://example.com",
            "repository": "https://github.com/example/plugin",
            "license": "MIT",
            "keywords": ["ci-cd", "deploy"],
            "skills": "./custom/skills/",
            "agents": "./custom-agents/",
            "hooks": "./config/hooks.json",
            "mcpServers": "./mcp-config.json"
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "deployment-tools");
        assert_eq!(manifest.version.as_deref(), Some("1.2.0"));
        assert_eq!(manifest.keywords, vec!["ci-cd", "deploy"]);
        assert!(matches!(manifest.skills, Some(PathOrPaths::Single(_))));
        manifest.validate().unwrap();
    }

    #[test]
    fn parse_manifest_ignores_unknown_fields() {
        let json = r#"{
            "name": "my-plugin",
            "marketplace": true,
            "installState": "active",
            "futureField": {"nested": "value"},
            "outputStyles": "./styles/"
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "my-plugin");
        manifest.validate().unwrap();
    }

    #[test]
    fn parse_manifest_inline_hooks() {
        let json = r#"{
            "name": "my-plugin",
            "hooks": {
                "hooks": {
                    "PostToolUse": [{"hooks": [{"type": "command", "command": "lint"}]}]
                }
            }
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.inline_hooks().is_some());
    }

    #[test]
    fn parse_manifest_inline_mcp() {
        let json = r#"{
            "name": "my-plugin",
            "mcpServers": {
                "mcpServers": {
                    "database": {
                        "command": "./servers/db-server",
                        "args": ["--config", "./config.json"]
                    }
                }
            }
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.inline_mcp_servers().is_some());
    }

    #[test]
    fn parse_manifest_multiple_skill_paths() {
        let json = r#"{
            "name": "my-plugin",
            "skills": ["./skills-a/", "./skills-b/"]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        match manifest.skills.unwrap() {
            PathOrPaths::Multiple(paths) => {
                assert_eq!(paths.len(), 2);
                assert_eq!(paths[0], "./skills-a/");
                assert_eq!(paths[1], "./skills-b/");
            }
            _ => panic!("expected Multiple"),
        }
    }

    #[test]
    fn name_from_dirname_basic() {
        assert_eq!(
            name_from_dirname(Path::new("/home/user/my-plugin")),
            Some("my-plugin".to_string())
        );
        assert_eq!(
            name_from_dirname(Path::new("/path/to/MyPlugin")),
            Some("myplugin".to_string())
        );
        assert_eq!(
            name_from_dirname(Path::new("/path/to/my_plugin")),
            Some("my-plugin".to_string())
        );
        assert_eq!(
            name_from_dirname(Path::new("/path/to/---")),
            None // all hyphens after trim
        );
    }

    #[test]
    fn load_manifest_from_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_root = tmp.path().join("my-plugin");
        std::fs::create_dir_all(&plugin_root).unwrap();

        // No manifest file
        match load_manifest(&plugin_root).unwrap() {
            ManifestLoadResult::NotFound => {}
            _ => panic!("expected NotFound"),
        }

        // Write root plugin.json
        let manifest_path = plugin_root.join("plugin.json");
        std::fs::write(
            &manifest_path,
            r#"{"name": "my-plugin", "version": "0.1.0"}"#,
        )
        .unwrap();

        match load_manifest(&plugin_root).unwrap() {
            ManifestLoadResult::Found(m) => {
                assert_eq!(m.name, "my-plugin");
                assert_eq!(m.version.as_deref(), Some("0.1.0"));
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn load_manifest_fallback_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_root = tmp.path().join("fallback-plugin");
        std::fs::create_dir_all(plugin_root.join(".grok-plugin")).unwrap();

        std::fs::write(
            plugin_root.join(".grok-plugin/plugin.json"),
            r#"{"name": "fallback-plugin"}"#,
        )
        .unwrap();

        match load_manifest(&plugin_root).unwrap() {
            ManifestLoadResult::Found(m) => assert_eq!(m.name, "fallback-plugin"),
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn load_manifest_root_wins_over_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_root = tmp.path().join("priority-test");
        std::fs::create_dir_all(plugin_root.join(".grok-plugin")).unwrap();

        // Write both root and fallback
        std::fs::write(plugin_root.join("plugin.json"), r#"{"name": "root-wins"}"#).unwrap();
        std::fs::write(
            plugin_root.join(".grok-plugin/plugin.json"),
            r#"{"name": "fallback-loses"}"#,
        )
        .unwrap();

        match load_manifest(&plugin_root).unwrap() {
            ManifestLoadResult::Found(m) => assert_eq!(m.name, "root-wins"),
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn manifest_rejects_invalid_name() {
        let json = r#"{"name": "INVALID_NAME"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn substitute_env_vars_replaces_all() {
        let input = "${GROK_PLUGIN_ROOT}/bin:${CLAUDE_PLUGIN_ROOT}/lib:${GROK_PLUGIN_DATA}/cache";
        let result = substitute_env_vars(input, "/home/user/plugin", "/home/user/.data/plugin");
        assert_eq!(
            result,
            "/home/user/plugin/bin:/home/user/plugin/lib:/home/user/.data/plugin/cache"
        );
    }

    #[test]
    fn skill_dirs_default_convention() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("test-plugin");
        std::fs::create_dir_all(root.join("skills")).unwrap();

        let manifest = PluginManifest {
            name: "test-plugin".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: None,
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: None,
            lsp_servers: None,
            withdrawn_plugin: None,
            runtime: None,
            exec: None,
            network: None,
            tools: None,
            slash_commands: None,
            config: None,
            oauth_label: None,
            settings: None,
            oauth_accounts: None,
        };
        let dirs = manifest.skill_dirs(&root);
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("skills"));
    }

    #[test]
    fn skill_dirs_no_default_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("no-skills");
        std::fs::create_dir_all(&root).unwrap();

        let manifest = PluginManifest {
            name: "no-skills".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: None,
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: None,
            lsp_servers: None,
            withdrawn_plugin: None,
            runtime: None,
            exec: None,
            network: None,
            tools: None,
            slash_commands: None,
            config: None,
            oauth_label: None,
            settings: None,
            oauth_accounts: None,
        };
        let dirs = manifest.skill_dirs(&root);
        assert!(dirs.is_empty());
    }

    #[test]
    fn path_escape_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("contained");
        std::fs::create_dir_all(&root).unwrap();
        // Create an outside directory
        let outside = tmp.path().join("outside-skills");
        std::fs::create_dir_all(&outside).unwrap();

        let manifest = PluginManifest {
            name: "escape-test".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: Some(PathOrPaths::Single("../outside-skills".to_string())),
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: None,
            lsp_servers: None,
            withdrawn_plugin: None,
            runtime: None,
            exec: None,
            network: None,
            tools: None,
            slash_commands: None,
            config: None,
            oauth_label: None,
            settings: None,
            oauth_accounts: None,
        };
        let dirs = manifest.skill_dirs(&root);
        assert!(
            dirs.is_empty(),
            "path escaping plugin root should be rejected"
        );
    }

    #[test]
    fn path_within_root_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("plugin");
        std::fs::create_dir_all(root.join("custom-skills")).unwrap();

        let manifest = PluginManifest {
            name: "within-test".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: Some(PathOrPaths::Single("custom-skills".to_string())),
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: None,
            lsp_servers: None,
            withdrawn_plugin: None,
            runtime: None,
            exec: None,
            network: None,
            tools: None,
            slash_commands: None,
            config: None,
            oauth_label: None,
            settings: None,
            oauth_accounts: None,
        };
        let dirs = manifest.skill_dirs(&root);
        assert_eq!(dirs.len(), 1, "path within plugin root should be accepted");
    }

    #[test]
    fn hooks_path_escape_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        // Create a hooks file outside the plugin root
        let outside = tmp.path().join("outside-hooks.json");
        std::fs::write(&outside, r#"{"hooks":{}}"#).unwrap();

        let manifest = PluginManifest {
            name: "escape-hooks".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: None,
            commands: None,
            agents: None,
            hooks: Some(PathOrInline::Path("../outside-hooks.json".to_string())),
            mcp_servers: None,
            lsp_servers: None,
            withdrawn_plugin: None,
            runtime: None,
            exec: None,
            network: None,
            tools: None,
            slash_commands: None,
            config: None,
            oauth_label: None,
            settings: None,
            oauth_accounts: None,
        };
        assert!(
            manifest.hooks_path(&root).is_none(),
            "hooks path escaping plugin root should be rejected"
        );
    }

    #[test]
    fn mcp_path_escape_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("outside-mcp.json");
        std::fs::write(&outside, r#"{"mcpServers":{}}"#).unwrap();

        let manifest = PluginManifest {
            name: "escape-mcp".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: None,
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: Some(PathOrInline::Path("../outside-mcp.json".to_string())),
            lsp_servers: None,
            withdrawn_plugin: None,
            runtime: None,
            exec: None,
            network: None,
            tools: None,
            slash_commands: None,
            config: None,
            oauth_label: None,
            settings: None,
            oauth_accounts: None,
        };
        assert!(
            manifest.mcp_config_path(&root).is_none(),
            "MCP path escaping plugin root should be rejected"
        );
    }

    fn manifest_with_inline_mcp(servers: serde_json::Value) -> PluginManifest {
        PluginManifest {
            name: "sentry".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: None,
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: Some(PathOrInline::Inline(servers)),
            lsp_servers: None,
            withdrawn_plugin: None,
            runtime: None,
            exec: None,
            network: None,
            tools: None,
            slash_commands: None,
            config: None,
            oauth_label: None,
            settings: None,
            oauth_accounts: None,
        }
    }

    #[test]
    fn normalize_inline_mcp_servers_wraps_direct_map() {
        let direct = serde_json::json!({
            "sentry": { "type": "http", "url": "https://mcp.sentry.dev/mcp" }
        });
        let normalized = normalize_inline_mcp_servers(&direct);
        let servers = normalized
            .get("mcpServers")
            .and_then(|v| v.as_object())
            .unwrap();
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("sentry"));
    }

    #[test]
    fn normalize_inline_mcp_servers_idempotent_for_wrapped() {
        let wrapped = serde_json::json!({
            "mcpServers": { "sentry": { "type": "http", "url": "https://mcp.sentry.dev/mcp" } }
        });
        assert_eq!(normalize_inline_mcp_servers(&wrapped), wrapped);
    }

    #[test]
    fn mcp_config_path_inline_does_not_suppress_sibling_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sentry");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"sentry":{"type":"http","url":"https://mcp.sentry.dev/mcp"}}}"#,
        )
        .unwrap();

        let manifest = manifest_with_inline_mcp(serde_json::json!({
            "sentry": { "type": "http", "url": "https://mcp.sentry.dev/mcp" }
        }));

        let resolved = manifest.mcp_config_path(&root);
        assert!(
            resolved.as_ref().is_some_and(|p| p.ends_with(".mcp.json")),
            "inline mcpServers must not hide a sibling .mcp.json"
        );
        assert!(manifest.inline_mcp_servers().is_some());
    }

    #[test]
    fn mcp_config_path_inline_without_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("inline-only");
        std::fs::create_dir_all(&root).unwrap();

        let manifest = manifest_with_inline_mcp(serde_json::json!({
            "foo": { "command": "./server" }
        }));
        assert!(manifest.mcp_config_path(&root).is_none());
    }

    // ── sidecar plugin (`exec`/`network`) ───────────────────────────────

    #[test]
    fn parse_manifest_with_sidecar_exec() {
        let json = r#"{
            "name": "ts-plugin",
            "exec": ["${GROK_PLUGIN_ROOT}/_sdk/run", "index.ts"],
            "network": true
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(
            manifest.exec,
            Some(ExecEntry::Argv(vec![
                "${GROK_PLUGIN_ROOT}/_sdk/run".into(),
                "index.ts".into(),
            ]))
        );
        assert_eq!(manifest.network, Some(true));
        assert!(manifest.has_sidecar());
        assert!(manifest.network_enabled());
        manifest.validate().unwrap();
    }

    #[test]
    fn sidecar_network_defaults_to_false() {
        let json = r#"{"name": "ts-plugin", "exec": "./plugin"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.network.is_none());
        assert!(!manifest.network_enabled());
    }

    #[test]
    fn existing_manifest_without_sidecar_fields_unchanged() {
        // A manifest that declares no sidecar at all must still parse: `exec`
        // and `network` both default to None.
        let json = r#"{"name": "my-plugin", "version": "1.0.0"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(!manifest.has_sidecar());
        assert!(manifest.exec.is_none());
        assert!(manifest.network.is_none());
        manifest.validate().unwrap();
    }

    #[test]
    fn sidecar_tools_parse_with_defaults_and_camel_case() {
        let json = r#"{
            "name": "toolful",
            "exec": "./index.ts",
            "tools": [
                {
                    "name": "planner",
                    "description": "Plan a change",
                    "inputSchema": { "type": "object", "properties": { "q": { "type": "string" } } },
                    "timeoutMs": 300000
                },
                { "name": "bare_tool" }
            ]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let tools = manifest.sidecar_tools();
        assert_eq!(tools.len(), 2);

        assert_eq!(tools[0].name, "planner");
        assert_eq!(tools[0].description, "Plan a change");
        assert_eq!(tools[0].timeout_ms, 300_000);
        assert!(tools[0].input_schema["properties"]["q"].is_object());

        // Omitted fields default: empty description, open object schema,
        // timeout 0 (host default).
        assert_eq!(tools[1].name, "bare_tool");
        assert_eq!(tools[1].description, "");
        assert_eq!(tools[1].timeout_ms, 0);
        assert_eq!(tools[1].input_schema["type"], "object");
    }

    #[test]
    fn sidecar_tools_skip_invalid_entries() {
        let json = r#"{
            "name": "toolful",
            "exec": "./index.ts",
            "tools": [
                { "name": "ok-tool" },
                { "name": "" },
                { "name": "has__delimiter" },
                { "name": "bad name!" },
                { "name": "ok-tool" },
                { "name": "bad-schema", "inputSchema": "not-an-object" }
            ]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let tools = manifest.sidecar_tools();
        // Only the first `ok-tool` survives: empty / `__` / bad charset /
        // duplicate / non-object schema are each warned and skipped.
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["ok-tool"]
        );
    }

    #[test]
    fn sidecar_tools_without_sidecar_entry_are_ignored() {
        let json = r#"{ "name": "no-sidecar", "tools": [{ "name": "ghost" }] }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(
            manifest.sidecar_tools().is_empty(),
            "tools without a `plugin` sidecar entry have nothing to serve them"
        );
    }

    // ── Sidecar slash commands (`slashCommands`) ────────────────────────

    #[test]
    fn sidecar_commands_parse_with_defaults_and_camel_case() {
        let json = r#"{
            "name": "deployer",
            "exec": "./index.ts",
            "commands": "./prompt-macros/",
            "slashCommands": [
                {
                    "name": "deploy",
                    "description": "Deploy the current branch",
                    "argumentHint": "<env> [--dry-run]",
                    "timeoutMs": 30000
                },
                { "name": "bare_command" }
            ]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let commands = manifest.sidecar_commands();
        assert_eq!(commands.len(), 2);

        assert_eq!(commands[0].name, "deploy");
        assert_eq!(commands[0].description, "Deploy the current branch");
        assert_eq!(
            commands[0].argument_hint.as_deref(),
            Some("<env> [--dry-run]")
        );
        assert_eq!(commands[0].timeout_ms, 30_000);

        // Omitted fields default: empty description, no hint, timeout 0 (host
        // default).
        assert_eq!(commands[1].name, "bare_command");
        assert_eq!(commands[1].description, "");
        assert_eq!(commands[1].argument_hint, None);
        assert_eq!(commands[1].timeout_ms, 0);

        // The markdown `commands` directory is a separate field and is
        // untouched by the handler declaration.
        assert!(matches!(manifest.commands, Some(PathOrPaths::Single(_))));
    }

    #[test]
    fn sidecar_commands_skip_invalid_entries() {
        let json = r#"{
            "name": "deployer",
            "exec": "./index.ts",
            "slashCommands": [
                { "name": "ok-command" },
                { "name": "" },
                { "name": "deployer:qualified" },
                { "name": "bad name!" },
                { "name": "ok-command" }
            ]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let commands = manifest.sidecar_commands();
        // Only the first `ok-command` survives: empty / `:` (the qualified-name
        // separator) / bad charset / duplicate are each warned and skipped.
        assert_eq!(
            commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["ok-command"]
        );
    }

    #[test]
    fn sidecar_commands_without_sidecar_entry_are_ignored() {
        let json = r#"{ "name": "no-sidecar", "slashCommands": [{ "name": "ghost" }] }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(
            manifest.sidecar_commands().is_empty(),
            "a slash command with no `exec` has nothing to serve `command_invoke`"
        );
    }

    // ── Contributed settings (`settings`) ───────────────────────────────

    #[test]
    fn plugin_settings_parse_every_kind_and_infer_missing_types() {
        let json = r#"{
            "name": "council",
            "exec": "./index.ts",
            "settings": [
                { "key": "verbose", "label": "Verbose logs", "default": false },
                { "key": "endpoint", "type": "string", "default": "https://a.example" },
                { "key": "rounds", "default": 3, "min": 1, "max": 10 },
                {
                    "key": "mode",
                    "default": "fast",
                    "choices": [
                        { "value": "fast", "label": "Fast", "description": "Fewer rounds" },
                        { "value": "thorough" }
                    ]
                }
            ]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let settings = manifest.plugin_settings();
        assert_eq!(settings.len(), 4);

        assert_eq!(settings[0].key, "verbose");
        assert_eq!(settings[0].label, "Verbose logs");
        assert_eq!(settings[0].kind, PluginSettingKind::Bool { default: false });

        assert_eq!(
            settings[1].kind,
            PluginSettingKind::String {
                default: "https://a.example".to_string()
            }
        );
        // Absent `label` falls back to the key.
        assert_eq!(settings[1].label, "endpoint");

        assert_eq!(
            settings[2].kind,
            PluginSettingKind::Int {
                default: 3,
                min: 1,
                max: 10
            }
        );

        let PluginSettingKind::Enum { default, choices } = &settings[3].kind else {
            panic!("mode should infer as an enum from `choices`");
        };
        assert_eq!(default, "fast");
        assert_eq!(choices.len(), 2);
        // Absent choice `label` falls back to the value.
        assert_eq!(choices[1].label, "thorough");
    }

    #[test]
    fn plugin_settings_skip_unusable_declarations() {
        let json = r#"{
            "name": "sloppy",
            "exec": "./index.ts",
            "settings": [
                { "key": "Bad.Key", "default": true },
                { "key": "dupe", "default": true },
                { "key": "dupe", "default": false },
                { "key": "unbounded", "type": "int", "default": 1 },
                { "key": "choiceless", "type": "enum", "default": "x" },
                { "key": "mystery" },
                { "key": "weird", "type": "colour", "default": "red" },
                { "key": "ok", "default": true }
            ]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let settings = manifest.plugin_settings();
        let keys: Vec<&str> = settings.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["dupe", "ok"],
            "an unusable entry is skipped, not fatal to the plugin"
        );
    }

    #[test]
    fn plugin_settings_without_sidecar_entry_are_ignored() {
        let json = r#"{
            "name": "no-sidecar",
            "settings": [{ "key": "verbose", "default": true }]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(
            manifest.plugin_settings().is_empty(),
            "a settings row nothing can read is not a setting"
        );
    }

    #[test]
    fn plugin_settings_enum_default_outside_choices_falls_back_to_first() {
        let json = r#"{
            "name": "council",
            "exec": "./index.ts",
            "settings": [{
                "key": "mode",
                "type": "enum",
                "default": "nonexistent",
                "choices": [{ "value": "fast" }, { "value": "thorough" }]
            }]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let settings = manifest.plugin_settings();
        let PluginSettingKind::Enum { default, .. } = &settings[0].kind else {
            panic!("expected an enum");
        };
        assert_eq!(default, "fast");
    }

    #[test]
    fn plugin_settings_are_capped_per_plugin() {
        let entries: Vec<String> = (0..MAX_PLUGIN_SETTINGS + 5)
            .map(|i| format!(r#"{{ "key": "k{i}", "default": true }}"#))
            .collect();
        let json = format!(
            r#"{{ "name": "greedy", "exec": "./index.ts", "settings": [{}] }}"#,
            entries.join(",")
        );
        let manifest: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.plugin_settings().len(), MAX_PLUGIN_SETTINGS);
    }

    #[test]
    fn manifest_without_settings_parses_unchanged() {
        let json = r#"{ "name": "my-plugin", "exec": "./index.ts" }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.settings.is_none());
        assert!(manifest.plugin_settings().is_empty());
    }

    // ── Manifest default config (`config`) ──────────────────────────────

    #[test]
    fn manifest_config_object_is_surfaced_as_defaults() {
        let json = r#"{
            "name": "council",
            "exec": "./index.ts",
            "config": { "participants": ["a", "b"], "rounds": 2 }
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let defaults = manifest.sidecar_config_defaults();
        assert_eq!(defaults["participants"], serde_json::json!(["a", "b"]));
        assert_eq!(defaults["rounds"], 2);
    }

    #[test]
    fn manifest_config_absent_defaults_to_empty_object() {
        let json = r#"{ "name": "plain", "exec": "./index.ts" }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.config.is_none());
        assert_eq!(manifest.sidecar_config_defaults(), serde_json::json!({}));
    }

    #[test]
    fn manifest_config_non_object_is_ignored() {
        // A non-object `config` (array/string/number) is dropped to `{}` so the
        // SDK's `ctx.config()` always sees an object.
        let json = r#"{ "name": "bad", "exec": "./index.ts", "config": [1, 2, 3] }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.config.is_some());
        assert_eq!(manifest.sidecar_config_defaults(), serde_json::json!({}));
    }

    // ── Interactive-login opt-in (`oauthLabel`) ─────────────────────────

    #[test]
    fn oauth_label_parses_from_camel_case() {
        let json = r#"{
            "name": "sign-in-plugin",
            "exec": "./index.ts",
            "oauthLabel": "Sign in with Acme"
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.oauth_label.as_deref(), Some("Sign in with Acme"));
        assert_eq!(manifest.oauth_login_label(), Some("Sign in with Acme"));
    }

    #[test]
    fn oauth_label_absent_defaults_to_none() {
        let json = r#"{ "name": "plain", "exec": "./index.ts" }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.oauth_label.is_none());
        assert_eq!(manifest.oauth_login_label(), None);
    }

    #[test]
    fn oauth_label_snake_case_is_ignored_as_unknown() {
        // The wire field is camelCase `oauthLabel`; a snake_case spelling is an
        // unknown field (silently ignored) and does not opt the plugin in.
        let json = r#"{ "name": "plain", "exec": "./index.ts", "oauth_label": "Nope" }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.oauth_login_label().is_none());
    }

    // ── Per-account login entries (`oauthAccounts`) ─────────────────────

    #[test]
    fn oauth_accounts_parse_in_declaration_order() {
        let json = r#"{
            "name": "example-auth",
            "exec": "./index.ts",
            "oauthLabel": "Acme",
            "oauthAccounts": [
                { "id": "work", "label": "work@example.com" },
                { "id": "personal", "label": "personal@example.com" }
            ]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let accounts = manifest.oauth_login_accounts();
        assert_eq!(
            accounts,
            vec![
                OauthAccount {
                    id: "work".to_string(),
                    label: "work@example.com".to_string(),
                },
                OauthAccount {
                    id: "personal".to_string(),
                    label: "personal@example.com".to_string(),
                },
            ]
        );
    }

    #[test]
    fn oauth_accounts_absent_or_empty_means_no_accounts() {
        // Both spellings of "this plugin has no accounts" behave identically,
        // and identically to a manifest authored before the field existed.
        for json in [
            r#"{ "name": "example-auth", "exec": "./index.ts", "oauthLabel": "Acme" }"#,
            r#"{ "name": "example-auth", "exec": "./index.ts", "oauthLabel": "Acme",
                 "oauthAccounts": [] }"#,
        ] {
            let manifest: PluginManifest = serde_json::from_str(json).unwrap();
            assert!(
                manifest.oauth_login_accounts().is_empty(),
                "no accounts means a single account-less login entry"
            );
            assert_eq!(manifest.oauth_login_label(), Some("Acme"));
        }
    }

    #[test]
    fn oauth_accounts_label_defaults_to_id() {
        let json = r#"{
            "name": "example-auth",
            "exec": "./index.ts",
            "oauthLabel": "Acme",
            "oauthAccounts": [{ "id": "work" }, { "id": "personal", "label": "  " }]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let labels: Vec<String> = manifest
            .oauth_login_accounts()
            .into_iter()
            .map(|a| a.label)
            .collect();
        assert_eq!(labels, vec!["work", "personal"]);
    }

    #[test]
    fn oauth_accounts_skip_invalid_entries() {
        let long_id = "a".repeat(MAX_OAUTH_ACCOUNT_ID_LEN + 1);
        let json = format!(
            r#"{{
                "name": "example-auth",
                "exec": "./index.ts",
                "oauthLabel": "Acme",
                "oauthAccounts": [
                    {{ "id": "work" }},
                    {{ "id": "" }},
                    {{ "id": "   " }},
                    {{ "id": "has#hash" }},
                    {{ "id": "ctrl\u0007char" }},
                    {{ "id": "{long_id}" }},
                    {{ "id": "work", "label": "duplicate" }}
                ]
            }}"#
        );
        let manifest: PluginManifest = serde_json::from_str(&json).unwrap();
        // Only the first `work` survives: blank / whitespace-only / `#` /
        // control char / over-long / duplicate are each warned and skipped.
        assert_eq!(
            manifest
                .oauth_login_accounts()
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            vec!["work"]
        );
    }

    #[test]
    fn oauth_accounts_entry_missing_id_fails_to_parse() {
        // `id` is the selector handed to the plugin, so it is required; a
        // manifest that omits it is a parse error, not a silent empty id.
        let json = r#"{
            "name": "example-auth",
            "exec": "./index.ts",
            "oauthLabel": "Acme",
            "oauthAccounts": [{ "label": "work@example.com" }]
        }"#;
        assert!(serde_json::from_str::<PluginManifest>(json).is_err());
    }

    #[test]
    fn oauth_accounts_without_oauth_label_are_ignored() {
        // Accounts narrow an advertised sign-in; without `oauthLabel` there is
        // no login entry for them to narrow.
        let json = r#"{
            "name": "example-auth",
            "exec": "./index.ts",
            "oauthAccounts": [{ "id": "work" }]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.oauth_login_accounts().is_empty());
    }

    #[test]
    fn oauth_accounts_snake_case_is_ignored_as_unknown() {
        // The wire field is camelCase `oauthAccounts`; a snake_case spelling is
        // an unknown field (silently ignored) and declares no accounts.
        let json = r#"{
            "name": "example-auth",
            "exec": "./index.ts",
            "oauthLabel": "Acme",
            "oauth_accounts": [{ "id": "work" }]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.oauth_accounts.is_none());
        assert!(manifest.oauth_login_accounts().is_empty());
    }

    // ── `exec`: a plugin is an executable that speaks the protocol ────

    /// A plugin root holding one executable file, for the `exec` tests.
    #[cfg(unix)]
    fn exec_root(file: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("any-lang");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(file);
        std::fs::write(&path, "#!/bin/sh\nexec cat\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (tmp, root)
    }

    fn exec_manifest(exec: ExecEntry) -> PluginManifest {
        PluginManifest {
            name: "any-lang".into(),
            exec: Some(exec),
            ..Default::default()
        }
    }

    #[test]
    fn exec_deserializes_both_string_and_argv_forms() {
        let one: PluginManifest =
            serde_json::from_str(r#"{"name":"p","exec":"./plugin"}"#).unwrap();
        assert_eq!(one.exec, Some(ExecEntry::Program("./plugin".into())));
        let many: PluginManifest =
            serde_json::from_str(r#"{"name":"p","exec":["python3","./plugin.py"]}"#).unwrap();
        assert_eq!(
            many.exec,
            Some(ExecEntry::Argv(vec![
                "python3".into(),
                "./plugin.py".into()
            ]))
        );
        // A manifest with neither field is still a plain (non-sidecar) plugin.
        let none: PluginManifest = serde_json::from_str(r#"{"name":"p"}"#).unwrap();
        assert!(none.exec.is_none());
        assert!(!none.has_sidecar());
    }

    #[test]
    #[cfg(unix)]
    fn exec_program_resolves_to_an_absolute_path_inside_the_root() {
        // The sidecar's cwd is the workspace, not the plugin root, so a relative
        // program must be made absolute here or the child would never find it.
        let (_tmp, root) = exec_root("plugin");
        let manifest = exec_manifest(ExecEntry::Program("./plugin".into()));
        let launch = manifest.sidecar_launch(&root, "/data").unwrap();
        assert_eq!(
            launch,
            SidecarLaunch {
                program: root.join("plugin"),
                args: vec![],
            }
        );
    }

    #[test]
    fn exec_bare_program_is_left_for_a_path_lookup_at_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = exec_manifest(ExecEntry::Argv(vec![
            "python3".into(),
            "--".into(),
            "-".into(),
        ]));
        let launch = manifest.sidecar_launch(tmp.path(), "/data").unwrap();
        assert_eq!(
            launch,
            SidecarLaunch {
                program: PathBuf::from("python3"),
                args: vec!["--".into(), "-".into()],
            }
        );
    }

    #[test]
    fn exec_argv_substitutes_the_plugin_root_and_data_tokens() {
        // The token is the supported way to name a file that ships with the
        // plugin: a bare `./plugin.py` argument would be resolved by the child
        // against the *workspace* root instead.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("any-lang");
        std::fs::create_dir_all(&root).unwrap();
        let manifest = exec_manifest(ExecEntry::Argv(vec![
            "python3".into(),
            "${GROK_PLUGIN_ROOT}/plugin.py".into(),
            "--state=${GROK_PLUGIN_DATA}/db".into(),
        ]));
        let SidecarLaunch { args, .. } = manifest.sidecar_launch(&root, "/data/any-lang").unwrap();
        assert_eq!(
            args,
            vec![
                format!("{}/plugin.py", root.display()),
                "--state=/data/any-lang/db".to_string(),
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn exec_program_may_be_absolute_once_it_is_inside_the_root() {
        // `${GROK_PLUGIN_ROOT}/bin/tool` is absolute after substitution, so the
        // rule for `exec` is containment rather than `plugin`'s blanket
        // rejection of absolute paths.
        let (_tmp, root) = exec_root("plugin");
        let manifest = exec_manifest(ExecEntry::Program("${GROK_PLUGIN_ROOT}/plugin".into()));
        let launch = manifest.sidecar_launch(&root, "/data").unwrap();
        assert_eq!(
            launch,
            SidecarLaunch {
                program: root.join("plugin"),
                args: vec![],
            }
        );
    }

    #[test]
    fn exec_program_escaping_the_root_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("any-lang");
        std::fs::create_dir_all(&root).unwrap();
        for escape in ["../evil", "/bin/sh"] {
            let manifest = exec_manifest(ExecEntry::Program(escape.into()));
            assert!(
                manifest.sidecar_launch(&root, "/data").is_none(),
                "{escape} should not resolve"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn exec_program_must_exist_and_carry_an_execute_bit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("any-lang");
        std::fs::create_dir_all(&root).unwrap();
        let manifest = exec_manifest(ExecEntry::Program("./plugin".into()));
        assert!(manifest.sidecar_launch(&root, "/data").is_none());

        std::fs::write(root.join("plugin"), "#!/bin/sh\n").unwrap();
        assert!(
            manifest.sidecar_launch(&root, "/data").is_none(),
            "a non-executable file must not be launched"
        );
    }

    #[test]
    fn exec_empty_argv_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        for empty in [ExecEntry::Argv(vec![]), ExecEntry::Program(String::new())] {
            assert!(
                exec_manifest(empty)
                    .sidecar_launch(tmp.path(), "/d")
                    .is_none()
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn exec_program_may_be_a_symlink_the_plugin_ships() {
        // The SDK launcher is `${GROK_PLUGIN_ROOT}/_sdk/run`: a real copy in a
        // deployed plugin, a symlink into a shared SDK checkout during
        // development. Canonicalizing would read the second as an escape and
        // refuse to start a plugin that is perfectly well formed.
        let (_tmp, root) = exec_root("real-launcher");
        let elsewhere = root.parent().unwrap().join("sdk");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::rename(root.join("real-launcher"), elsewhere.join("run")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("_sdk")).unwrap();

        let manifest = exec_manifest(ExecEntry::Argv(vec![
            "${GROK_PLUGIN_ROOT}/_sdk/run".into(),
            "index.ts".into(),
        ]));
        assert_eq!(
            manifest.sidecar_launch(&root, "/data"),
            Some(SidecarLaunch {
                program: root.join("_sdk/run"),
                args: vec!["index.ts".to_string()],
            })
        );
    }

    #[test]
    fn the_withdrawn_launch_fields_are_refused_by_name() {
        // A manifest still on `plugin` + `runtime` must fail loudly. Ignoring
        // the fields would load the plugin with a sidecar that never starts,
        // and nothing would say why.
        for field in ["plugin", "runtime"] {
            let json = format!(r#"{{"name": "stale", "{field}": "./index.ts"}}"#);
            let manifest: PluginManifest = serde_json::from_str(&json).unwrap();
            let err = manifest.validate().expect_err("must be refused");
            let text = err.to_string();
            assert!(text.contains(field), "error must name `{field}`: {text}");
            assert!(
                text.contains("exec"),
                "error must say what to write instead: {text}"
            );
        }
    }

    #[test]
    fn exec_plugin_keeps_its_declared_tools() {
        // The tool catalog is gated on `has_sidecar`, which must recognise the
        // command form — otherwise a non-TS plugin's tools vanish silently.
        let manifest = PluginManifest {
            name: "any-lang".into(),
            exec: Some(ExecEntry::Program("./plugin".into())),
            tools: Some(vec![ManifestToolSpec {
                name: "echo".into(),
                description: Some("echo back".into()),
                input_schema: None,
                timeout_ms: None,
            }]),
            ..Default::default()
        };
        let tools = manifest.sidecar_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }
}
