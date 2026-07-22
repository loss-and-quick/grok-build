//! Plugin manifest parsing and validation.
//!
//! The canonical manifest location is `plugin.json` at the plugin root.
//! Fallback locations (checked in order when the root manifest is absent):
//! 1. `.grok-plugin/plugin.json`
//! 2. `.claude-plugin/plugin.json`
//!
//! If no manifest is found at all, the plugin can still function via
//! convention-based discovery (skills/, agents/, .mcp.json, hooks/hooks.json),
//! with the plugin name derived from the directory name.
//!
//! The parser is forward-compatible: unknown fields are silently ignored
//! so that manifests authored for newer upstream versions still load.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Maximum length of a plugin name (kebab-case identifier).
const MAX_PLUGIN_NAME_LEN: usize = 64;

/// Regex pattern for valid plugin names: lowercase alphanumeric + hyphens.
fn is_valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PLUGIN_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Author metadata from a plugin manifest.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// A path reference that can be either a single path or multiple paths.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PathOrPaths {
    Single(String),
    Multiple(Vec<String>),
}

impl PathOrPaths {
    /// Resolve all contained paths relative to a plugin root.
    ///
    /// Paths that escape the plugin root (via `..` components) are rejected
    /// with a warning and excluded from the result.
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

/// Check whether a resolved path stays within the plugin root.
///
/// Canonicalizes both sides (resolving symlinks and `..`) before the prefix check.
fn is_path_contained(resolved: &Path, plugin_root: &Path) -> bool {
    let canonical_root =
        dunce::canonicalize(plugin_root).unwrap_or_else(|_| plugin_root.to_path_buf());
    let canonical_resolved =
        dunce::canonicalize(resolved).unwrap_or_else(|_| resolved.to_path_buf());
    // Fail-closed >MAX_PATH caveat: see workspace clippy.toml.
    canonical_resolved.starts_with(&canonical_root)
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

/// A value that can be either a file path (string) or an inline JSON object.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PathOrInline {
    Path(String),
    Inline(serde_json::Value),
}

/// TypeScript sidecar plugin runtime selection (`plugin.json`'s `runtime` field).
///
/// `Auto` (the default) defers to the plugin host's own probe order —
/// `bun` → `node` (22+) → `deno` — per the Phase 1 plan. Explicit variants
/// pin the sidecar to a single runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginRuntime {
    #[default]
    Auto,
    Bun,
    Node,
    Deno,
}

/// Parsed plugin manifest from `plugin.json`.
///
/// Forward-compatible: unknown fields are silently ignored via
/// `#[serde(deny_unknown_fields)]` NOT being set.
#[derive(Debug, Clone, Deserialize)]
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

    // ── TypeScript sidecar plugin ────────────────────────────────────
    /// Relative path to the sidecar entry file (e.g. `"./index.ts"`).
    /// Presence of this field marks the plugin as a TS sidecar plugin.
    #[serde(default)]
    pub plugin: Option<String>,
    /// Sidecar runtime selection. Defaults to `Auto` when `plugin` is set
    /// but `runtime` is omitted; see [`PluginManifest::runtime_or_default`].
    #[serde(default)]
    pub runtime: Option<PluginRuntime>,
    /// Whether the sidecar's child process may reach the network.
    /// Defaults to `false`; see [`PluginManifest::network_enabled`].
    #[serde(default)]
    pub network: Option<bool>,
    /// Model-visible tools the sidecar serves via `tool_invoke`. The manifest
    /// is the source of truth for the tool catalog (built before any sidecar
    /// starts, so lazy sidecar start survives); the SDK-side handler map is
    /// cross-checked against it at handshake. See
    /// [`PluginManifest::sidecar_tools`].
    #[serde(default)]
    pub tools: Option<Vec<ManifestToolSpec>>,
    /// Default per-plugin configuration object (`plugin.json`'s `config`).
    /// Surfaced to the sidecar at `initialize` and via `config_get`; user
    /// `[plugins.<name>]` entries from config.toml are shallow-merged over
    /// these defaults by the shell's session plugin-host wiring. Defaults to
    /// `{}` when absent. See [`PluginManifest::sidecar_config_defaults`].
    #[serde(default)]
    pub config: Option<serde_json::Value>,
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

impl PluginManifest {
    /// Validate the parsed manifest.
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

    /// Resolve the hooks path from the manifest.
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

    /// Get inline hooks JSON value, if the manifest uses inline hooks.
    ///
    /// Inline hooks are fully supported — the runtime parses and executes them
    /// via `parse_plugin_hooks_from_value()`. This accessor is used during
    /// `LoadedPlugin` construction and by the hooks adapter.
    pub fn inline_hooks(&self) -> Option<&serde_json::Value> {
        match &self.hooks {
            Some(PathOrInline::Inline(v)) => Some(v),
            _ => None,
        }
    }

    /// Get inline MCP servers JSON value, if the manifest uses inline MCP.
    ///
    /// Inline MCP servers are fully supported — the runtime parses and starts
    /// them via `load_plugin_mcp_servers_from_value()`. This accessor is used
    /// during `LoadedPlugin` construction and by the MCP merger.
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

    /// Whether the manifest declares a TypeScript sidecar entry (`plugin` field).
    pub fn has_sidecar(&self) -> bool {
        self.plugin.is_some()
    }

    /// Effective sidecar runtime: the manifest's `runtime`, or `Auto` when unset.
    pub fn runtime_or_default(&self) -> PluginRuntime {
        self.runtime.unwrap_or_default()
    }

    /// Effective network flag: the manifest's `network`, or `false` when unset.
    pub fn network_enabled(&self) -> bool {
        self.network.unwrap_or(false)
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
                    "manifest declares tools but no sidecar entry (`plugin`); ignoring them"
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

    /// Resolve the sidecar entry path from the manifest `plugin` field.
    ///
    /// Returns `None` when `plugin` is unset, the path is absolute or escapes
    /// the plugin root (via `..` components — reuses the same
    /// [`is_path_contained`] containment check as `hooks_path`/`mcp_config_path`),
    /// or the resolved file does not exist. Like the other component-path
    /// accessors, existence is checked lazily here rather than eagerly in
    /// [`PluginManifest::validate`].
    pub fn sidecar_entry_path(&self, plugin_root: &Path) -> Option<PathBuf> {
        let entry = self.plugin.as_ref()?;
        if Path::new(entry).is_absolute() {
            tracing::warn!(
                path = entry,
                plugin_root = %plugin_root.display(),
                "sidecar plugin path must be relative; skipping"
            );
            return None;
        }
        let resolved = plugin_root.join(entry);
        if !is_path_contained(&resolved, plugin_root) {
            tracing::warn!(
                path = %resolved.display(),
                plugin_root = %plugin_root.display(),
                "sidecar plugin path escapes plugin root; skipping"
            );
            return None;
        }
        resolved.is_file().then_some(resolved)
    }

    /// Log informational messages about manifest features.
    ///
    /// Called during discovery. Inline hooks and MCP servers are now
    /// fully supported; this method logs when they are detected.
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
        if self.has_sidecar() {
            tracing::info!(
                plugin = plugin_name,
                runtime = ?self.runtime_or_default(),
                network = self.network_enabled(),
                "plugin declares a TypeScript sidecar entry"
            );
        }
    }
}

/// Resolve directories from a manifest field or fall back to a default subdirectory.
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

/// Result of attempting to load a manifest from a plugin directory.
#[derive(Debug)]
pub enum ManifestLoadResult {
    /// Manifest found and parsed successfully.
    Found(Box<PluginManifest>),
    /// No manifest file found — plugin uses convention-based discovery.
    NotFound,
}

/// Load a plugin manifest from the given plugin root directory.
///
/// Tries manifest files in priority order (see [`MANIFEST_PATHS`]).
/// If no manifest is found, returns `ManifestLoadResult::NotFound`.
/// The caller can still create a convention-based plugin from the directory.
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

/// Derive a plugin name from a directory name.
///
/// Sanitizes the directory name to match the kebab-case constraint:
/// lowercase, alphanumeric + hyphens, no leading/trailing hyphens.
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

/// Perform plugin-token substitution in a string.
///
/// Replaces `${GROK_PLUGIN_ROOT}`, `${CLAUDE_PLUGIN_ROOT}`,
/// `${GROK_PLUGIN_DATA}`, and `${CLAUDE_PLUGIN_DATA}` with the provided values.
///
/// Delegates to [`xai_grok_tools::util::substitute_plugin_tokens`], the single
/// source of truth shared with plugin skill/command body substitution.
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

        // Write manifest in .grok-plugin/ fallback location
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
            plugin: None,
            runtime: None,
            network: None,
            tools: None,
            config: None,
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
            plugin: None,
            runtime: None,
            network: None,
            tools: None,
            config: None,
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
            plugin: None,
            runtime: None,
            network: None,
            tools: None,
            config: None,
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
            plugin: None,
            runtime: None,
            network: None,
            tools: None,
            config: None,
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
            plugin: None,
            runtime: None,
            network: None,
            tools: None,
            config: None,
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
            plugin: None,
            runtime: None,
            network: None,
            tools: None,
            config: None,
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
            plugin: None,
            runtime: None,
            network: None,
            tools: None,
            config: None,
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

    // ── TS sidecar plugin (`plugin`/`runtime`/`network`) ────────────────

    #[test]
    fn parse_manifest_with_sidecar_plugin() {
        let json = r#"{
            "name": "ts-plugin",
            "plugin": "./index.ts",
            "runtime": "bun",
            "network": true
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.plugin.as_deref(), Some("./index.ts"));
        assert_eq!(manifest.runtime, Some(PluginRuntime::Bun));
        assert_eq!(manifest.network, Some(true));
        assert!(manifest.has_sidecar());
        assert!(manifest.network_enabled());
        manifest.validate().unwrap();
    }

    #[test]
    fn parse_manifest_sidecar_runtime_spellings() {
        for (spelling, expected) in [
            ("auto", PluginRuntime::Auto),
            ("bun", PluginRuntime::Bun),
            ("node", PluginRuntime::Node),
            ("deno", PluginRuntime::Deno),
        ] {
            let json = format!(
                r#"{{"name": "ts-plugin", "plugin": "./index.ts", "runtime": "{spelling}"}}"#
            );
            let manifest: PluginManifest = serde_json::from_str(&json).unwrap();
            assert_eq!(manifest.runtime, Some(expected), "spelling: {spelling}");
        }
    }

    #[test]
    fn sidecar_runtime_defaults_to_auto_when_plugin_set() {
        let json = r#"{"name": "ts-plugin", "plugin": "./index.ts"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.runtime.is_none());
        assert_eq!(manifest.runtime_or_default(), PluginRuntime::Auto);
    }

    #[test]
    fn sidecar_network_defaults_to_false() {
        let json = r#"{"name": "ts-plugin", "plugin": "./index.ts"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.network.is_none());
        assert!(!manifest.network_enabled());
    }

    #[test]
    fn existing_manifest_without_plugin_field_unchanged() {
        // Manifests authored before the sidecar fields existed must still
        // parse identically: `plugin`/`runtime`/`network` all default to None.
        let json = r#"{"name": "my-plugin", "version": "1.0.0"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(!manifest.has_sidecar());
        assert!(manifest.plugin.is_none());
        assert!(manifest.runtime.is_none());
        assert!(manifest.network.is_none());
        manifest.validate().unwrap();
    }

    #[test]
    fn sidecar_entry_path_resolves_within_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ts-plugin");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.ts"), "export default {};").unwrap();

        let json = r#"{"name": "ts-plugin", "plugin": "./index.ts"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let resolved = manifest.sidecar_entry_path(&root);
        assert_eq!(resolved, Some(root.join("index.ts")));
    }

    #[test]
    fn sidecar_entry_path_missing_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ts-plugin");
        std::fs::create_dir_all(&root).unwrap();
        // index.ts is never written.

        let json = r#"{"name": "ts-plugin", "plugin": "./index.ts"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.sidecar_entry_path(&root).is_none());
    }

    #[test]
    fn sidecar_entry_path_escape_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ts-plugin");
        std::fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("outside-index.ts");
        std::fs::write(&outside, "export default {};").unwrap();

        let json = r#"{"name": "ts-plugin", "plugin": "../outside-index.ts"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(
            manifest.sidecar_entry_path(&root).is_none(),
            "sidecar path escaping plugin root should be rejected"
        );
    }

    #[test]
    fn sidecar_entry_path_absolute_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ts-plugin");
        std::fs::create_dir_all(&root).unwrap();
        // Create the file at the absolute path the manifest points to, so a
        // permissive implementation would otherwise happily resolve it.
        let absolute_target = tmp.path().join("abs-index.ts");
        std::fs::write(&absolute_target, "export default {};").unwrap();

        let json = serde_json::json!({
            "name": "ts-plugin",
            "plugin": absolute_target.to_string_lossy(),
        })
        .to_string();
        let manifest: PluginManifest = serde_json::from_str(&json).unwrap();
        assert!(
            manifest.sidecar_entry_path(&root).is_none(),
            "absolute sidecar path should be rejected"
        );
    }

    #[test]
    fn sidecar_tools_parse_with_defaults_and_camel_case() {
        let json = r#"{
            "name": "toolful",
            "plugin": "./index.ts",
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
            "plugin": "./index.ts",
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

    // ── Manifest default config (`config`) ──────────────────────────────

    #[test]
    fn manifest_config_object_is_surfaced_as_defaults() {
        let json = r#"{
            "name": "council",
            "plugin": "./index.ts",
            "config": { "participants": ["a", "b"], "rounds": 2 }
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        let defaults = manifest.sidecar_config_defaults();
        assert_eq!(defaults["participants"], serde_json::json!(["a", "b"]));
        assert_eq!(defaults["rounds"], 2);
    }

    #[test]
    fn manifest_config_absent_defaults_to_empty_object() {
        let json = r#"{ "name": "plain", "plugin": "./index.ts" }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.config.is_none());
        assert_eq!(manifest.sidecar_config_defaults(), serde_json::json!({}));
    }

    #[test]
    fn manifest_config_non_object_is_ignored() {
        // A non-object `config` (array/string/number) is dropped to `{}` so the
        // SDK's `ctx.config()` always sees an object.
        let json = r#"{ "name": "bad", "plugin": "./index.ts", "config": [1, 2, 3] }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.config.is_some());
        assert_eq!(manifest.sidecar_config_defaults(), serde_json::json!({}));
    }
}
