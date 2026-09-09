//! `x.ai/personas/*`: the persona catalog, for any client.
//!
//! Four methods, because a persona browser needs four different things and
//! none of them is the same shape as another:
//!
//! - `x.ai/personas/list` — every persona the agent can see, merged across the
//!   bundled cache, `~/.grok/personas` and `{workspace}/.grok/personas`.
//! - `x.ai/personas/get` — one persona's fields, plus the revision an edit
//!   has to quote.
//! - `x.ai/personas/save` — one create-or-update.
//! - `x.ai/personas/delete` — one removal.
//!
//! List and get are split for the same reason `x.ai/bundle/status` and
//! `x.ai/bundle/entry/get` are split: a catalog of names and one-line
//! descriptions is cheap to send for every persona, and a persona's
//! instructions are not.
//!
//! ## Writes
//!
//! A persona file is rewritten whole, so two writers do not interleave the way
//! two appends do — they overwrite. The failure mode is a silently lost edit,
//! and there are two writers to worry about: a second client, and the user's
//! own `$EDITOR` sitting on the same file.
//!
//! Two things answer that, and one thing deliberately does not:
//!
//! - Every read hands back a `revision`, the hash of the exact bytes it read.
//!   A save quotes the revision it started from, and if the file no longer
//!   hashes to it the save is refused — with the current revision, so the
//!   client can say what happened — rather than applied over someone else's
//!   work. A create quotes nothing and is refused when the name is taken.
//! - The write is a temp file and a rename, so no reader ever sees half a TOML
//!   document and a write that fails leaves the old one intact. It does not
//!   `fsync`, so a power loss is out of scope: this is about concurrent readers
//!   and writers, not about the disk.
//! - What is *not* fixed: the revision check and the rename are two steps, so a
//!   writer landing between them still wins silently. The window is one `stat`
//!   wide and both writers are the same user on the same machine. That is a
//!   lost edit, never a corrupted file, and it is written down here rather than
//!   papered over.
//!
//! No method takes a path. The scope (`user` or `project`) and the name decide
//! where the file lives, and `project` resolves against the session's own
//! workspace — so no caller can aim a write at a file of its choosing.
//!
//! ## Parsing
//!
//! Persona TOML is read field by field rather than through
//! [`crate::config::SubagentPersona`], which rejects an unknown key inside an
//! `inputs` entry and would drop the whole persona for it. A browsing client
//! wants to see a persona it can then fix; strict parsing belongs to the agent
//! that has to run it.

use agent_client_protocol as acp;
use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;

/// Directory name under every persona root.
const PERSONAS_DIR: &str = "personas";

/// Where a persona file lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersonaScope {
    /// The downloaded bundle cache under `~/.grok/bundled`. Read-only: it is
    /// replaced wholesale by the next sync, so an edit there is not an edit.
    Bundled,
    /// `~/.grok/personas`.
    User,
    /// `{workspace}/.grok/personas`.
    Project,
}

impl PersonaScope {
    fn label(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::User => "user",
            Self::Project => "project",
        }
    }

    /// Whether a client may save or delete in this scope.
    fn writable(self) -> bool {
        !matches!(self, Self::Bundled)
    }
}

impl Default for PersonaScope {
    /// The read-only one. A scope that arrived unparseable must not be the one
    /// that accepts writes.
    fn default() -> Self {
        Self::Bundled
    }
}

/// One persona as the catalog sees it: enough to draw a row, not enough to
/// edit one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonaSummary {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub has_inputs: bool,
    pub has_outputs: bool,
    pub scope: PersonaScope,
    /// Absolute path of the file it came from. Display and `$EDITOR` only — no
    /// method accepts a path back.
    pub source_path: String,
    /// Whether `save` and `delete` will act on it.
    pub editable: bool,
    /// Hash of the bytes this summary was built from.
    pub revision: String,
}

/// A declared input or output, read leniently so one malformed entry costs one
/// entry rather than the persona.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonaIo {
    pub name: String,
    pub io_type: String,
    pub required: bool,
    pub description: String,
}

/// Every field the persona editor shows, plus the revision a save must quote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonaDocument {
    /// Catalog name — the file stem, and the name `save`/`delete` take.
    pub name: String,
    /// The persona's own `name` key, when it declares one.
    pub declared_name: String,
    pub description: String,
    pub model: String,
    pub reasoning_effort: String,
    pub default_isolation: String,
    pub instructions: String,
    pub instructions_file: String,
    pub inputs: Vec<PersonaIo>,
    pub outputs: Vec<PersonaIo>,
    pub scope: PersonaScope,
    pub source_path: String,
    pub editable: bool,
    pub revision: String,
}

/// Why a write did not happen. A refusal is a normal response, not an error:
/// nothing broke, and what the user needs to be told is which of them won.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PersonaRefusalKind {
    /// The file changed since the revision the caller quoted.
    Conflict,
    /// A create was asked for and that name is already taken.
    AlreadyExists,
    /// No persona of that name in that scope.
    NotFound,
    /// The scope does not accept writes.
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonaRefusal {
    pub kind: PersonaRefusalKind,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonaListResponse {
    pub personas: Vec<PersonaSummary>,
    /// False when no session was given, so `{workspace}/.grok/personas` was not
    /// searched and a `project` write is not available.
    pub project_scope_available: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonaWriteResponse {
    pub applied: bool,
    /// The name the write actually used, after sanitizing. A client that asked
    /// for `my persona` learns it got `my-persona` from here.
    pub name: String,
    pub scope: PersonaScope,
    /// Path written, or the path that would have been written.
    pub path: String,
    /// The revision on disk now: after a successful write, what to quote next;
    /// after a conflict, what the caller failed to quote. Absent once the file
    /// is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<PersonaRefusal>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PersonaListRequest {
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersonaGetRequest {
    #[serde(default)]
    session_id: Option<String>,
    name: String,
    /// Restrict the lookup to one scope. Omitted, the same precedence the list
    /// uses picks the winner.
    #[serde(default)]
    scope: Option<PersonaScope>,
}

/// The editable fields, each one three-valued: absent leaves the key alone,
/// empty removes it, anything else sets it.
///
/// Absent-means-untouched is what keeps an older client from wiping a field it
/// has never heard of, and a newer client from needing every field to exist.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PersonaFields {
    /// The persona's own `name` key, not its filename.
    name: Option<String>,
    description: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    default_isolation: Option<String>,
    instructions: Option<String>,
    instructions_file: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersonaSaveRequest {
    #[serde(default)]
    session_id: Option<String>,
    scope: PersonaScope,
    name: String,
    /// The revision the editor loaded. Absent asserts the persona is new.
    #[serde(default)]
    base_revision: Option<String>,
    #[serde(default)]
    fields: PersonaFields,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersonaDeleteRequest {
    #[serde(default)]
    session_id: Option<String>,
    scope: PersonaScope,
    name: String,
    /// When given, a persona that changed since is left alone.
    #[serde(default)]
    base_revision: Option<String>,
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "x.ai/personas/list" => {
            let req: PersonaListRequest = parse_params(args)?;
            let roots = roots_for(agent, req.session_id.as_deref());
            to_raw_response(&PersonaListResponse {
                personas: list_at(&roots),
                project_scope_available: roots.project.is_some(),
            })
        }
        "x.ai/personas/get" => {
            let req: PersonaGetRequest = parse_params(args)?;
            let roots = roots_for(agent, req.session_id.as_deref());
            let doc = get_at(&roots, &req.name, req.scope).ok_or_else(|| {
                acp::Error::invalid_params().data(format!("persona not found: {}", req.name))
            })?;
            to_raw_response(&doc)
        }
        "x.ai/personas/save" => {
            let req: PersonaSaveRequest = parse_params(args)?;
            let dir = write_dir_for(agent, req.scope, req.session_id.as_deref())?;
            let response = tokio::task::spawn_blocking(move || {
                save_at(
                    &dir,
                    req.scope,
                    &req.name,
                    req.base_revision.as_deref(),
                    &req.fields,
                )
            })
            .await
            .map_err(|e| acp::Error::internal_error().data(format!("persona write panicked: {e}")))?
            .map_err(|e| acp::Error::internal_error().data(format!("{e}")))?;
            to_raw_response(&response)
        }
        "x.ai/personas/delete" => {
            let req: PersonaDeleteRequest = parse_params(args)?;
            let dir = write_dir_for(agent, req.scope, req.session_id.as_deref())?;
            let response = tokio::task::spawn_blocking(move || {
                delete_at(&dir, req.scope, &req.name, req.base_revision.as_deref())
            })
            .await
            .map_err(|e| {
                acp::Error::internal_error().data(format!("persona delete panicked: {e}"))
            })?
            .map_err(|e| acp::Error::internal_error().data(format!("{e}")))?;
            to_raw_response(&response)
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

/// The three directories a persona can come from. `project` is `None` when the
/// caller named no session, because there is then no workspace to resolve it
/// against — and guessing one would be aiming a read at a directory nobody
/// asked for.
struct PersonaRoots {
    bundled: PathBuf,
    user: PathBuf,
    project: Option<PathBuf>,
}

fn roots_for(agent: &MvpAgent, session_id: Option<&str>) -> PersonaRoots {
    PersonaRoots {
        bundled: crate::bundle::bundled_root().join(PERSONAS_DIR),
        user: xai_grok_config::grok_home().join(PERSONAS_DIR),
        project: session_cwd(agent, session_id).map(|cwd| cwd.join(".grok").join(PERSONAS_DIR)),
    }
}

fn session_cwd(agent: &MvpAgent, session_id: Option<&str>) -> Option<PathBuf> {
    let sid: acp::SessionId = session_id?.to_owned().into();
    let session = agent.resident_handle(&sid)?;
    Some(PathBuf::from(session.info.cwd.clone()))
}

/// The directory a write lands in, or the reason there is not one.
fn write_dir_for(
    agent: &MvpAgent,
    scope: PersonaScope,
    session_id: Option<&str>,
) -> Result<PathBuf, acp::Error> {
    match scope {
        PersonaScope::Bundled => Err(acp::Error::invalid_params()
            .data("bundled personas are replaced by the next sync and cannot be edited")),
        PersonaScope::User => Ok(xai_grok_config::grok_home().join(PERSONAS_DIR)),
        PersonaScope::Project => session_cwd(agent, session_id)
            .map(|cwd| cwd.join(".grok").join(PERSONAS_DIR))
            .ok_or_else(|| {
                acp::Error::invalid_params()
                    .data("a project persona needs a session to say which workspace")
            }),
    }
}

/// Hash of a file's exact bytes, short enough to read in a log line and long
/// enough that two different personas will not share one.
fn revision_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_owned()
}

/// Reject a name that would escape its directory. Sanitizing happens on write;
/// this is the guard that runs on every path we build.
fn valid_stem(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
}

/// Fold a user-typed name into a filename: everything that is not
/// alphanumeric, `-` or `_` becomes `-`, and a name with no alphanumeric
/// character at all is refused rather than silently turned into dashes.
fn sanitize_name(name: &str) -> anyhow::Result<String> {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if !sanitized.chars().any(char::is_alphanumeric) {
        bail!("persona name must contain at least one alphanumeric character");
    }
    if !valid_stem(&sanitized) {
        bail!("invalid persona name: {name}");
    }
    Ok(sanitized)
}

fn toml_str(table: &toml::Value, key: &str) -> String {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn toml_io(table: &toml::Value, key: &str) -> Vec<PersonaIo> {
    table
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|array| {
            array
                .iter()
                .map(|item| PersonaIo {
                    name: item
                        .get("name")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("?")
                        .to_owned(),
                    io_type: item
                        .get("io_type")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("file")
                        .to_owned(),
                    required: item
                        .get("required")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(false),
                    description: item
                        .get("description")
                        .and_then(toml::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Read one persona file into a summary. `None` when it is missing or is not
/// TOML at all — the same silence the pager kept, so an unparseable file is a
/// row that does not appear rather than a catalog that does not load.
fn summary_at(path: &Path, name: &str, scope: PersonaScope) -> Option<PersonaSummary> {
    let bytes = std::fs::read(path).ok()?;
    let content = String::from_utf8(bytes).ok()?;
    let table: toml::Value = toml::from_str(&content).ok()?;
    let description = table
        .get("description")
        .and_then(toml::Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            table
                .get("instructions")
                .and_then(toml::Value::as_str)
                .and_then(
                    xai_grok_tools::implementations::skills::discovery::extract_first_paragraph,
                )
        });
    Some(PersonaSummary {
        name: name.to_owned(),
        description,
        has_inputs: !toml_io(&table, "inputs").is_empty(),
        has_outputs: !toml_io(&table, "outputs").is_empty(),
        scope,
        source_path: path.display().to_string(),
        editable: scope.writable(),
        revision: revision_of(content.as_bytes()),
    })
}

/// Persona names in one directory, sorted, `.toml` only.
fn stems_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stems: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_owned();
            valid_stem(&stem).then_some(stem)
        })
        .collect();
    stems.sort();
    stems
}

/// Bundled persona names, gated on the manifest so a file left behind by an
/// older bundle is not offered as if it were current.
fn bundled_stems(dir: &Path) -> Vec<String> {
    let root = match dir.parent() {
        Some(root) => root,
        None => return Vec::new(),
    };
    let Ok(Some(manifest)) = crate::bundle::read_cached_manifest(root) else {
        return Vec::new();
    };
    stems_in(dir)
        .into_iter()
        .filter(|stem| {
            manifest
                .checksums
                .contains_key(&format!("{PERSONAS_DIR}/{stem}.toml"))
        })
        .collect()
}

/// The merged catalog. Bundled names win, then project, then user — the order
/// the pager's own merge used, kept so switching it to this method does not
/// quietly reorder anybody's list.
fn list_at(roots: &PersonaRoots) -> Vec<PersonaSummary> {
    let mut out: Vec<PersonaSummary> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in bundled_stems(&roots.bundled) {
        if let Some(summary) = summary_at(
            &roots.bundled.join(format!("{name}.toml")),
            &name,
            PersonaScope::Bundled,
        ) {
            seen.insert(name);
            out.push(summary);
        }
    }
    let locals = [
        (PersonaScope::Project, roots.project.as_deref()),
        (PersonaScope::User, Some(roots.user.as_path())),
    ];
    for (scope, dir) in locals {
        let Some(dir) = dir else { continue };
        for name in stems_in(dir) {
            if seen.contains(&name) {
                continue;
            }
            if let Some(summary) = summary_at(&dir.join(format!("{name}.toml")), &name, scope) {
                seen.insert(name);
                out.push(summary);
            }
        }
    }
    out
}

/// One persona's full contents. Without an explicit scope this follows the same
/// precedence the list does, so `get` returns the persona the list showed.
fn get_at(
    roots: &PersonaRoots,
    name: &str,
    scope: Option<PersonaScope>,
) -> Option<PersonaDocument> {
    if !valid_stem(name) {
        return None;
    }
    let candidates: Vec<(PersonaScope, &Path)> = match scope {
        Some(PersonaScope::Bundled) => vec![(PersonaScope::Bundled, roots.bundled.as_path())],
        Some(PersonaScope::User) => vec![(PersonaScope::User, roots.user.as_path())],
        Some(PersonaScope::Project) => roots
            .project
            .as_deref()
            .map(|dir| vec![(PersonaScope::Project, dir)])
            .unwrap_or_default(),
        None => {
            let mut all: Vec<(PersonaScope, &Path)> =
                vec![(PersonaScope::Bundled, roots.bundled.as_path())];
            if let Some(dir) = roots.project.as_deref() {
                all.push((PersonaScope::Project, dir));
            }
            all.push((PersonaScope::User, roots.user.as_path()));
            all
        }
    };
    for (scope, dir) in candidates {
        let path = dir.join(format!("{name}.toml"));
        if scope == PersonaScope::Bundled && !bundled_stems(dir).iter().any(|s| s == name) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(table) = toml::from_str::<toml::Value>(&content) else {
            continue;
        };
        return Some(PersonaDocument {
            name: name.to_owned(),
            declared_name: toml_str(&table, "name"),
            description: toml_str(&table, "description"),
            model: toml_str(&table, "model"),
            reasoning_effort: toml_str(&table, "reasoning_effort"),
            default_isolation: toml_str(&table, "default_isolation"),
            instructions: toml_str(&table, "instructions"),
            instructions_file: toml_str(&table, "instructions_file"),
            inputs: toml_io(&table, "inputs"),
            outputs: toml_io(&table, "outputs"),
            scope,
            source_path: path.display().to_string(),
            editable: scope.writable(),
            revision: revision_of(content.as_bytes()),
        });
    }
    None
}

fn refuse(
    kind: PersonaRefusalKind,
    message: impl Into<String>,
    name: &str,
    scope: PersonaScope,
    path: &Path,
    revision: Option<String>,
) -> PersonaWriteResponse {
    PersonaWriteResponse {
        applied: false,
        name: name.to_owned(),
        scope,
        path: path.display().to_string(),
        revision,
        refusal: Some(PersonaRefusal {
            kind,
            message: message.into(),
        }),
    }
}

/// Current bytes and revision of a persona file, or `None` when it is absent.
fn on_disk(path: &Path) -> Option<(String, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let revision = revision_of(content.as_bytes());
    Some((content, revision))
}

/// Create or update one persona.
///
/// `base_revision` is the whole concurrency story: absent means "this is new",
/// present means "this is the version I read". Either assertion failing is a
/// refusal, never a write.
fn save_at(
    dir: &Path,
    scope: PersonaScope,
    requested_name: &str,
    base_revision: Option<&str>,
    fields: &PersonaFields,
) -> anyhow::Result<PersonaWriteResponse> {
    if !scope.writable() {
        let path = dir.join(format!("{requested_name}.toml"));
        return Ok(refuse(
            PersonaRefusalKind::ReadOnly,
            "bundled personas are replaced by the next sync and cannot be edited",
            requested_name,
            scope,
            &path,
            None,
        ));
    }
    let name = sanitize_name(requested_name)?;
    let path = dir.join(format!("{name}.toml"));
    let current = on_disk(&path);

    let mut doc = match (&current, base_revision) {
        (Some(_), None) => {
            return Ok(refuse(
                PersonaRefusalKind::AlreadyExists,
                format!("persona '{name}' already exists"),
                &name,
                scope,
                &path,
                current.map(|(_, rev)| rev),
            ));
        }
        (None, Some(_)) => {
            return Ok(refuse(
                PersonaRefusalKind::NotFound,
                format!("persona '{name}' no longer exists"),
                &name,
                scope,
                &path,
                None,
            ));
        }
        (Some((content, revision)), Some(base)) => {
            if revision != base {
                return Ok(refuse(
                    PersonaRefusalKind::Conflict,
                    format!("persona '{name}' changed on disk since it was read"),
                    &name,
                    scope,
                    &path,
                    Some(revision.clone()),
                ));
            }
            // Edited as a document rather than re-serialized, so comments,
            // key order and the tables this method does not expose (`inputs`,
            // `outputs`) survive an edit that never mentioned them.
            content.parse::<toml_edit::DocumentMut>()?
        }
        (None, None) => toml_edit::DocumentMut::new(),
    };

    let assignments: [(&str, Option<&String>); 7] = [
        ("name", fields.name.as_ref()),
        ("description", fields.description.as_ref()),
        ("model", fields.model.as_ref()),
        ("reasoning_effort", fields.reasoning_effort.as_ref()),
        ("default_isolation", fields.default_isolation.as_ref()),
        ("instructions", fields.instructions.as_ref()),
        ("instructions_file", fields.instructions_file.as_ref()),
    ];
    for (key, value) in assignments {
        match value.map(|v| v.trim()) {
            None => {}
            Some("") => {
                doc.remove(key);
            }
            Some(text) => doc[key] = toml_edit::value(text),
        }
    }

    let rendered = doc.to_string();
    std::fs::create_dir_all(dir)?;
    xai_grok_config::fs_atomic::write_atomically(&path, &rendered, None)?;
    Ok(PersonaWriteResponse {
        applied: true,
        name,
        scope,
        path: path.display().to_string(),
        revision: Some(revision_of(rendered.as_bytes())),
        refusal: None,
    })
}

/// Remove one persona. A quoted revision that no longer matches is a refusal,
/// so a delete aimed at what the user saw does not carry off what someone else
/// has since written.
fn delete_at(
    dir: &Path,
    scope: PersonaScope,
    requested_name: &str,
    base_revision: Option<&str>,
) -> anyhow::Result<PersonaWriteResponse> {
    if !scope.writable() {
        let path = dir.join(format!("{requested_name}.toml"));
        return Ok(refuse(
            PersonaRefusalKind::ReadOnly,
            "bundled personas belong to the bundle cache and cannot be deleted",
            requested_name,
            scope,
            &path,
            None,
        ));
    }
    let name = sanitize_name(requested_name)?;
    let path = dir.join(format!("{name}.toml"));
    let Some((_, revision)) = on_disk(&path) else {
        return Ok(refuse(
            PersonaRefusalKind::NotFound,
            format!("persona '{name}' not found"),
            &name,
            scope,
            &path,
            None,
        ));
    };
    if let Some(base) = base_revision
        && base != revision
    {
        return Ok(refuse(
            PersonaRefusalKind::Conflict,
            format!("persona '{name}' changed on disk since it was read"),
            &name,
            scope,
            &path,
            Some(revision),
        ));
    }
    std::fs::remove_file(&path)?;
    Ok(PersonaWriteResponse {
        applied: true,
        name,
        scope,
        path: path.display().to_string(),
        revision: None,
        refusal: None,
    })
}

#[cfg(test)]
#[path = "personas_tests.rs"]
mod tests;
