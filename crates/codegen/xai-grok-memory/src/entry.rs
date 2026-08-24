//! Single-fact memory entries in the Claude Code memory format.
//!
//! An entry is one file holding one fact, with YAML frontmatter carrying the
//! `name`, a one-line `description` used to judge relevance during recall, and
//! `metadata.type`. Entries live in a `memories/` directory next to a
//! `MEMORY.md` that is a pure *index* — one pointer line per entry, never the
//! entry content itself:
//!
//! ```text
//! ~/.grok/memory/
//!   ├── MEMORY.md              # curated prose, owned by dream/flush (unchanged)
//!   ├── memories/              # ← this module
//!   │   ├── MEMORY.md          # index: `- [Title](name.md) — description`
//!   │   └── prefers-tabs.md    # one fact, frontmatter + body
//!   └── {workspace}/
//!       ├── MEMORY.md          # curated prose, overwritten wholesale by dream
//!       ├── memories/          # ← this module, workspace scope
//!       └── sessions/
//! ```
//!
//! The `memories/` subdirectory exists because the two layouts disagree about
//! what `MEMORY.md` *is*. Grok's is a content file that `dream` rewrites from
//! scratch on every consolidation ([`crate::dream`]); Claude Code's is an index
//! that must survive. Putting the index one level down lets both keep their own
//! `MEMORY.md` semantics, needs no migration of an existing store, and makes a
//! Claude Code memory directory a verbatim drop-in — its `MEMORY.md` lands as
//! the index it already is.
//!
//! Bodies may reference other entries with `[[name]]`. Nothing resolves those
//! links; they are convention, passed through verbatim, exactly as Claude Code
//! treats them.

use std::path::{Path, PathBuf};

use crate::storage::{MemoryScope, MemoryStorage, slugify};

/// Directory holding single-fact entries plus their `MEMORY.md` index,
/// relative to a scope root.
///
/// Cannot collide with a workspace directory under the global root: those are
/// always `{slug}-{hash8}` (see `compute_workspace_hash`), and this is not.
pub const MEMORIES_SUBDIR: &str = "memories";

/// Index file inside [`MEMORIES_SUBDIR`].
pub const MEMORIES_INDEX_FILE: &str = "MEMORY.md";

/// Maximum length of an entry `name` (the kebab-case slug and file stem).
pub const MAX_NAME_CHARS: usize = 64;

/// Maximum length of an entry `description`.
///
/// The description is a single line that does double duty: it is the recall
/// hook the model reads to judge relevance, and it is the text after the em
/// dash on the entry's index line. Both want it short.
pub const MAX_DESCRIPTION_CHARS: usize = 200;

/// Maximum length of an entry body.
///
/// Deliberately not a config knob, unlike `[memory.flush].max_flush_write_chars`
/// (8000). That cap bounds a whole-session summary, whose right size varies with
/// how the user works. This one bounds *one fact*, and the format — one file per
/// fact, one line of description — already fixes what a right-sized entry looks
/// like. A body that does not fit is not a tuning problem, it is two entries.
pub const MAX_BODY_CHARS: usize = 4000;

/// What kind of fact an entry records. Mirrors Claude Code's `metadata.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// A durable fact about the user: preferences, environment, working style.
    User,
    /// A correction the user gave, to avoid repeating a mistake.
    Feedback,
    /// A fact about the codebase being worked in.
    Project,
    /// Durable technical reference learned while working.
    Reference,
}

impl EntryType {
    /// The wire/frontmatter spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }

    /// Parse a frontmatter/tool-input spelling.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }

    /// Scope to use when the caller does not name one.
    ///
    /// `user` and `feedback` describe the person, so they follow them across
    /// repositories. `project` and `reference` are learned inside one codebase,
    /// and a wrongly-workspace-scoped entry is contained where a wrongly-global
    /// one pollutes every future session — so the ambiguous case defaults in.
    pub fn default_scope(&self) -> MemoryScope {
        match self {
            Self::User | Self::Feedback => MemoryScope::Global,
            Self::Project | Self::Reference => MemoryScope::Workspace,
        }
    }

    /// Every variant, for building error messages and tool schemas.
    pub const ALL: [EntryType; 4] = [
        EntryType::User,
        EntryType::Feedback,
        EntryType::Project,
        EntryType::Reference,
    ];
}

/// Why an entry could not be built or written.
#[derive(Debug)]
pub enum EntryError {
    /// `name` contained nothing that survives slugification.
    UnusableName(String),
    /// `type` was not one of [`EntryType::ALL`].
    UnknownType(String),
    /// A required field was empty after trimming.
    Empty(&'static str),
    /// A field exceeded its documented cap.
    TooLong {
        field: &'static str,
        len: usize,
        max: usize,
    },
    /// Workspace scope in a temp-dir cwd, where workspace memory is not kept.
    Ephemeral,
    /// Filesystem failure.
    Io(std::io::Error),
}

impl std::fmt::Display for EntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnusableName(raw) => write!(
                f,
                "name {raw:?} has no letters or digits to build a slug from"
            ),
            Self::UnknownType(raw) => {
                let known: Vec<&str> = EntryType::ALL.iter().map(EntryType::as_str).collect();
                write!(
                    f,
                    "unknown type {raw:?}; expected one of {}",
                    known.join(", ")
                )
            }
            Self::Empty(field) => write!(f, "{field} must not be empty"),
            Self::TooLong { field, len, max } => {
                write!(f, "{field} is {len} characters, over the {max} limit")
            }
            Self::Ephemeral => write!(
                f,
                "workspace memory is not kept for temporary directories; use the global scope"
            ),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EntryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for EntryError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// One fact, validated and ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEntry {
    /// Kebab-case slug; also the file stem.
    pub name: String,
    /// Human-readable link text on the index line.
    pub title: String,
    /// One-line relevance hook.
    pub description: String,
    /// `metadata.type`.
    pub entry_type: EntryType,
    /// Markdown body.
    pub body: String,
}

impl MemoryEntry {
    /// Validate and normalize the parts of an entry.
    ///
    /// `name` is slugified rather than rejected for casing or spaces, so
    /// `"Prefers Tabs"` and `"prefers-tabs"` name the same entry — writing the
    /// same name twice is how an entry is updated, and that should not hinge on
    /// reproducing punctuation exactly.
    pub fn new(
        name: &str,
        title: Option<&str>,
        description: &str,
        entry_type: EntryType,
        body: &str,
    ) -> Result<Self, EntryError> {
        let raw_name = name.trim();
        if raw_name.is_empty() {
            return Err(EntryError::Empty("name"));
        }
        check_len("name", raw_name, MAX_NAME_CHARS)?;
        let name = slugify(raw_name, MAX_NAME_CHARS);
        if name.is_empty() {
            return Err(EntryError::UnusableName(raw_name.to_string()));
        }

        let description = one_line(description);
        if description.is_empty() {
            return Err(EntryError::Empty("description"));
        }
        check_len("description", &description, MAX_DESCRIPTION_CHARS)?;

        let body = body.trim();
        if body.is_empty() {
            return Err(EntryError::Empty("content"));
        }
        check_len("content", body, MAX_BODY_CHARS)?;

        let title = match title.map(one_line) {
            Some(t) if !t.is_empty() => t,
            _ => default_title(&name),
        };

        Ok(Self {
            name,
            title,
            description,
            entry_type,
            body: body.to_string(),
        })
    }

    /// File name of this entry within `memories/`.
    pub fn file_name(&self) -> String {
        format!("{}.md", self.name)
    }

    /// The full file contents: YAML frontmatter followed by the body.
    ///
    /// `modified` is emitted because Claude Code emits it; nothing here reads
    /// it back, and a rewrite refreshes it.
    pub fn render(&self, modified: &str) -> String {
        format!(
            "---\nname: {}\ndescription: {}\nmetadata:\n  type: {}\n  modified: {}\n---\n\n{}\n",
            yaml_scalar(&self.name),
            yaml_scalar(&self.description),
            self.entry_type.as_str(),
            yaml_scalar(modified),
            self.body,
        )
    }

    /// The pointer line this entry contributes to `memories/MEMORY.md`.
    pub fn index_line(&self) -> String {
        pointer_line(&self.title, &self.file_name(), &self.description)
    }
}

/// Where an entry was written, and whether it displaced an existing one.
#[derive(Debug, Clone)]
pub struct WrittenEntry {
    /// The entry file.
    pub path: PathBuf,
    /// The `memories/MEMORY.md` index that now points at it.
    pub index_path: PathBuf,
    /// Scope the entry landed in.
    pub scope: MemoryScope,
    /// `false` when an entry of the same name was overwritten.
    pub created: bool,
}

impl MemoryStorage {
    /// The `memories/` directory for a scope.
    pub fn memories_dir(&self, scope: MemoryScope) -> PathBuf {
        let root = match scope {
            MemoryScope::Global => self.global_dir(),
            MemoryScope::Workspace => self.workspace_dir(),
        };
        root.join(MEMORIES_SUBDIR)
    }

    /// The index file for a scope's entries.
    pub fn memories_index_file(&self, scope: MemoryScope) -> PathBuf {
        self.memories_dir(scope).join(MEMORIES_INDEX_FILE)
    }

    /// `true` if `path` is an entry file or index under either scope's
    /// `memories/` directory.
    pub fn is_memories_path(&self, path: &Path) -> bool {
        path.parent().is_some_and(|parent| {
            parent == self.memories_dir(MemoryScope::Global)
                || parent == self.memories_dir(MemoryScope::Workspace)
        })
    }

    /// List the `.md` files under a scope's `memories/` directory, sorted by
    /// name with the index first.
    pub fn list_memories(&self, scope: MemoryScope) -> Vec<PathBuf> {
        let dir = self.memories_dir(scope);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut index = None;
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            if path.file_name().is_some_and(|n| n == MEMORIES_INDEX_FILE) {
                index = Some(path);
            } else {
                files.push(path);
            }
        }
        files.sort();
        index.into_iter().chain(files).collect()
    }

    /// Write one entry and point the scope's index at it.
    ///
    /// Overwrites any entry of the same name — writing the same name again is
    /// the update path. The index line is replaced in place when one already
    /// points at this file, so hand-written grouping and ordering survive.
    pub fn write_entry(
        &self,
        scope: MemoryScope,
        entry: &MemoryEntry,
    ) -> Result<WrittenEntry, EntryError> {
        // Unlike the other writers, this one refuses rather than silently
        // no-ops: a model told "saved" for a write that never happened would
        // keep referring back to a memory that does not exist.
        if self.is_ephemeral() && scope == MemoryScope::Workspace {
            return Err(EntryError::Ephemeral);
        }

        let dir = self.memories_dir(scope);
        std::fs::create_dir_all(&dir)?;

        let path = dir.join(entry.file_name());
        let created = !path.exists();
        let modified = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        std::fs::write(&path, entry.render(&modified))?;

        let index_path = dir.join(MEMORIES_INDEX_FILE);
        let existing = std::fs::read_to_string(&index_path).unwrap_or_default();
        let updated = upsert_index_line(&existing, &entry.file_name(), &entry.index_line());
        std::fs::write(&index_path, updated)?;

        tracing::debug!(
            path = %path.display(),
            scope = ?scope,
            created,
            "wrote memory entry"
        );

        Ok(WrittenEntry {
            path,
            index_path,
            scope,
            created,
        })
    }
}

/// An entry that was removed, and what had to be cleaned up with it.
#[derive(Debug, Clone)]
pub struct DeletedEntry {
    /// The entry file, now gone. Kept so the caller can drop its chunks from
    /// the search index — a memory the model can still find after deleting it
    /// is worse than one it never deleted.
    pub path: PathBuf,
    /// The index the pointer line was removed from.
    pub index_path: PathBuf,
    /// Scope the entry lived in.
    pub scope: MemoryScope,
    /// The hook from its index line, when it had one, so the caller can say
    /// *what* it deleted rather than only that something is gone.
    pub description: Option<String>,
}

impl MemoryStorage {
    /// Path an entry of this name would occupy in a scope. The name is
    /// slugified the same way [`MemoryEntry::new`] slugifies it, so a delete
    /// and a write agree on what one name means.
    pub fn entry_path(&self, scope: MemoryScope, name: &str) -> PathBuf {
        self.memories_dir(scope)
            .join(format!("{}.md", slugify(name.trim(), MAX_NAME_CHARS)))
    }

    /// Scopes that currently hold an entry of this name, workspace first.
    ///
    /// Workspace first because it is the more specific store, and because a
    /// caller that resolves an unqualified name wants the nearer one; a caller
    /// that must not guess uses the length of this instead.
    pub fn entry_scopes(&self, name: &str) -> Vec<MemoryScope> {
        [MemoryScope::Workspace, MemoryScope::Global]
            .into_iter()
            .filter(|&scope| self.entry_path(scope, name).is_file())
            .collect()
    }

    /// Remove one entry and its pointer line. `Ok(None)` when no such entry
    /// exists in that scope — an absent memory is not an error, it is the
    /// state the caller asked for.
    pub fn delete_entry(
        &self,
        scope: MemoryScope,
        name: &str,
    ) -> Result<Option<DeletedEntry>, EntryError> {
        let path = self.entry_path(scope, name);
        if !path.is_file() {
            return Ok(None);
        }
        let Some(file_name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            return Ok(None);
        };

        let index_path = self.memories_index_file(scope);
        let existing = std::fs::read_to_string(&index_path).unwrap_or_default();
        let description = existing
            .lines()
            .filter_map(parse_pointer_line)
            .find(|p| p.target == file_name)
            .map(|p| p.description.to_string())
            .filter(|d| !d.is_empty());

        // File first: a pointer to a file that is still there is a recoverable
        // inconsistency, while a file with no pointer is invisible to the index
        // in the prefix and would have to be found by search to be removed.
        std::fs::remove_file(&path)?;
        if index_path.exists() {
            std::fs::write(&index_path, remove_index_line(&existing, &file_name))?;
        }

        tracing::debug!(
            path = %path.display(),
            scope = ?scope,
            "deleted memory entry"
        );

        Ok(Some(DeletedEntry {
            path,
            index_path,
            scope,
            description,
        }))
    }
}

/// Replace the pointer line for `file_name` in an index, or add one.
///
/// Every other line is preserved byte-for-byte, so headings, grouping and
/// hand-written notes in a user's or Claude Code's `MEMORY.md` survive a write.
/// A new line is inserted directly after the last existing pointer line rather
/// than at end of file, so entries stay together when the index has a trailing
/// section.
pub fn upsert_index_line(existing: &str, file_name: &str, new_line: &str) -> String {
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    let mut last_pointer: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        match pointer_target(line) {
            Some(target) if target == file_name => {
                let mut out = lines.clone();
                out[i] = new_line.to_string();
                return join_lines(&out);
            }
            Some(_) => last_pointer = Some(i),
            None => {}
        }
    }

    match last_pointer {
        Some(i) => lines.insert(i + 1, new_line.to_string()),
        None => {
            // No pointers yet: keep any existing preamble, separated by a
            // blank line so Markdown starts the list cleanly.
            if lines.iter().any(|l| !l.trim().is_empty()) {
                while lines.last().is_some_and(|l| l.trim().is_empty()) {
                    lines.pop();
                }
                lines.push(String::new());
            }
            lines.push(new_line.to_string());
        }
    }
    join_lines(&lines)
}

/// Drop the pointer line for `file_name` from an index.
///
/// The counterpart to [`upsert_index_line`] and bound by the same rule: every
/// line that is not that one pointer survives byte-for-byte, so headings,
/// grouping and hand-written notes are not collateral damage of a delete.
/// Removing the last pointer from an index that held nothing else empties the
/// file rather than leaving a lone newline behind.
pub fn remove_index_line(existing: &str, file_name: &str) -> String {
    let kept: Vec<&str> = existing
        .lines()
        .filter(|line| pointer_target(line) != Some(file_name))
        .collect();
    if kept.iter().all(|l| l.trim().is_empty()) {
        return String::new();
    }
    let mut out = kept.join("\n");
    out.push('\n');
    out
}

/// The pointer lines of an index file, trimmed, in file order.
///
/// Headings, prose and blank lines are dropped. An index is allowed to carry
/// them — [`upsert_index_line`] preserves whatever a user or Claude Code wrote
/// around the pointers — but a caller that puts the index in front of the model
/// wants its size to track the number of entries, not the length of a preamble
/// somebody pasted into `MEMORY.md`.
pub fn index_pointer_lines(index: &str) -> Vec<&str> {
    index
        .lines()
        .filter(|line| pointer_target(line).is_some())
        .map(str::trim)
        .collect()
}

/// The three parts of a `- [Title](target) — hook` pointer line.
///
/// Reading a line back is what lets an import keep the title and hook a human
/// already wrote instead of regenerating both from the slug — Claude Code's
/// index carries real titles (`Роль оркестратора`) over kebab-case file names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerLine<'a> {
    /// Link text.
    pub title: &'a str,
    /// Link target: the entry's file name.
    pub target: &'a str,
    /// Text after the em dash; empty when the line carries none.
    pub description: &'a str,
}

/// Parse a `- [Title](target) — hook` pointer line, or `None` for any other line.
///
/// The em dash and hook are optional. A pointer without one is still a pointer,
/// and treating it as prose would drop the entry out of every index operation.
pub fn parse_pointer_line(line: &str) -> Option<PointerLine<'_>> {
    let rest = line.trim().strip_prefix("- [")?;
    let close = rest.find("](")?;
    let after = &rest[close + 2..];
    let end = after.find(')')?;
    let description = after[end + 1..]
        .trim_start()
        .strip_prefix('\u{2014}')
        .unwrap_or("")
        .trim();
    Some(PointerLine {
        title: &rest[..close],
        target: &after[..end],
        description,
    })
}

/// Render a pointer line, omitting the em dash when there is no hook to hang
/// off it.
pub fn pointer_line(title: &str, file_name: &str, description: &str) -> String {
    if description.is_empty() {
        format!("- [{title}]({file_name})")
    } else {
        format!("- [{title}]({file_name}) \u{2014} {description}")
    }
}

/// Extract the link target of a `- [Title](target) — hook` pointer line.
fn pointer_target(line: &str) -> Option<&str> {
    parse_pointer_line(line).map(|p| p.target)
}

fn join_lines(lines: &[String]) -> String {
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Title-cased fallback for the index link text: `prefers-tabs` → `prefers tabs`.
///
/// Matches Claude Code's `displayTitle`; callers that want a real title pass one.
fn default_title(name: &str) -> String {
    name.replace('-', " ")
}

/// Collapse all whitespace runs to single spaces and trim.
///
/// Frontmatter and index values are single-line by construction; a pasted
/// newline would otherwise corrupt both files.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_len(field: &'static str, value: &str, max: usize) -> Result<(), EntryError> {
    let len = value.chars().count();
    if len > max {
        return Err(EntryError::TooLong { field, len, max });
    }
    Ok(())
}

/// Render a string as a YAML scalar, quoting only when plain style would
/// change the parsed value.
///
/// Claude Code writes `description: "Тело коммита — 3–5 строк…"` for values
/// that need it and bare text otherwise; matching that keeps the files
/// byte-comparable with what the user already has.
fn yaml_scalar(s: &str) -> String {
    let needs_quotes = s.is_empty()
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.contains(": ")
        || s.ends_with(':')
        || s.contains(" #")
        || s.contains('"')
        || s.contains('\\')
        || s.contains('\n')
        || s.starts_with([
            '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%',
            '@', '`',
        ])
        || matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
        );

    if !needs_quotes {
        return s.to_string();
    }
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn storage(tmp: &TempDir) -> MemoryStorage {
        let global = tmp.path().join("memory");
        let workspace = global.join("proj-abc12345");
        MemoryStorage::with_paths(global, workspace)
    }

    fn entry(name: &str) -> MemoryEntry {
        MemoryEntry::new(
            name,
            None,
            "A one line hook.",
            EntryType::Project,
            "The body.",
        )
        .unwrap()
    }

    // ── validation ────────────────────────────────────────────────────────

    #[test]
    fn name_is_slugified() {
        let e = entry("Prefers Tabs!");
        assert_eq!(e.name, "prefers-tabs");
        assert_eq!(e.file_name(), "prefers-tabs.md");
    }

    #[test]
    fn name_variants_collapse_to_one_entry() {
        assert_eq!(entry("Prefers Tabs").name, entry("prefers-tabs").name);
    }

    #[test]
    fn name_without_alphanumerics_is_rejected() {
        let err = MemoryEntry::new("!!!", None, "hook", EntryType::User, "body").unwrap_err();
        assert!(matches!(err, EntryError::UnusableName(_)), "got {err:?}");
    }

    #[test]
    fn empty_fields_are_rejected() {
        assert!(matches!(
            MemoryEntry::new("", None, "hook", EntryType::User, "body").unwrap_err(),
            EntryError::Empty("name")
        ));
        assert!(matches!(
            MemoryEntry::new("n", None, "  ", EntryType::User, "body").unwrap_err(),
            EntryError::Empty("description")
        ));
        assert!(matches!(
            MemoryEntry::new("n", None, "hook", EntryType::User, "\n\n").unwrap_err(),
            EntryError::Empty("content")
        ));
    }

    /// The body cap is the model-facing bound on a write; it must reject rather
    /// than truncate, so a half-stored fact never looks stored.
    #[test]
    fn oversized_body_is_rejected_not_truncated() {
        let body = "x".repeat(MAX_BODY_CHARS + 1);
        let err = MemoryEntry::new("n", None, "hook", EntryType::User, &body).unwrap_err();
        match err {
            EntryError::TooLong { field, len, max } => {
                assert_eq!(field, "content");
                assert_eq!(len, MAX_BODY_CHARS + 1);
                assert_eq!(max, MAX_BODY_CHARS);
            }
            other => panic!("expected TooLong, got {other:?}"),
        }
    }

    #[test]
    fn caps_count_characters_not_bytes() {
        // Cyrillic is 2 bytes per char; a body at the cap must still be accepted.
        let body = "я".repeat(MAX_BODY_CHARS);
        assert!(MemoryEntry::new("n", None, "hook", EntryType::User, &body).is_ok());
    }

    #[test]
    fn oversized_name_and_description_are_rejected() {
        let long = "a".repeat(MAX_NAME_CHARS + 1);
        assert!(matches!(
            MemoryEntry::new(&long, None, "hook", EntryType::User, "body").unwrap_err(),
            EntryError::TooLong { field: "name", .. }
        ));
        let long = "a".repeat(MAX_DESCRIPTION_CHARS + 1);
        assert!(matches!(
            MemoryEntry::new("n", None, &long, EntryType::User, "body").unwrap_err(),
            EntryError::TooLong {
                field: "description",
                ..
            }
        ));
    }

    #[test]
    fn multiline_description_is_collapsed() {
        let e = MemoryEntry::new("n", None, "one\n  two\n", EntryType::User, "b").unwrap();
        assert_eq!(e.description, "one two");
    }

    #[test]
    fn title_defaults_to_dehyphenated_name() {
        assert_eq!(entry("commit-message-style").title, "commit message style");
    }

    #[test]
    fn explicit_title_wins() {
        let e = MemoryEntry::new(
            "orchestrator-role",
            Some("Роль оркестратора"),
            "hook",
            EntryType::User,
            "b",
        )
        .unwrap();
        assert_eq!(e.title, "Роль оркестратора");
    }

    // ── type → scope ──────────────────────────────────────────────────────

    #[test]
    fn type_round_trips_through_parse() {
        for t in EntryType::ALL {
            assert_eq!(EntryType::parse(t.as_str()), Some(t));
        }
        assert_eq!(EntryType::parse("PROJECT"), Some(EntryType::Project));
        assert_eq!(EntryType::parse("nonsense"), None);
    }

    #[test]
    fn default_scope_follows_type() {
        assert_eq!(EntryType::User.default_scope(), MemoryScope::Global);
        assert_eq!(EntryType::Feedback.default_scope(), MemoryScope::Global);
        assert_eq!(EntryType::Project.default_scope(), MemoryScope::Workspace);
        assert_eq!(EntryType::Reference.default_scope(), MemoryScope::Workspace);
    }

    // ── rendering ─────────────────────────────────────────────────────────

    #[test]
    fn render_emits_claude_code_frontmatter() {
        let e = entry("prefers-tabs");
        let rendered = e.render("2026-08-10T15:54:27Z");
        assert!(
            rendered.starts_with("---\nname: prefers-tabs\n"),
            "{rendered}"
        );
        assert!(rendered.contains("\ndescription: A one line hook.\n"));
        assert!(rendered.contains("\nmetadata:\n  type: project\n"));
        assert!(rendered.contains("  modified: 2026-08-10T15:54:27Z\n"));
        assert!(rendered.ends_with("---\n\nThe body.\n"));
    }

    /// A description containing `: ` would silently become a nested mapping if
    /// emitted bare, so it must be quoted.
    #[test]
    fn colon_bearing_description_is_quoted() {
        let e = MemoryEntry::new("n", None, "rule: always lint", EntryType::User, "b").unwrap();
        assert!(
            e.render("t").contains("description: \"rule: always lint\""),
            "{}",
            e.render("t")
        );
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        let e = MemoryEntry::new("n", None, r#"say "hi" \ bye"#, EntryType::User, "b").unwrap();
        assert!(
            e.render("t")
                .contains(r#"description: "say \"hi\" \\ bye""#),
            "{}",
            e.render("t")
        );
    }

    /// Plain values stay plain — the point is byte-comparability with the files
    /// Claude Code already wrote.
    #[test]
    fn plain_description_stays_unquoted() {
        assert_eq!(yaml_scalar("a normal hook"), "a normal hook");
        assert_eq!(yaml_scalar("Тело коммита"), "Тело коммита");
    }

    #[test]
    fn yaml_lookalike_scalars_are_quoted() {
        assert_eq!(yaml_scalar("true"), "\"true\"");
        assert_eq!(yaml_scalar("no"), "\"no\"");
        assert_eq!(yaml_scalar("- leading dash"), "\"- leading dash\"");
    }

    #[test]
    fn index_line_uses_em_dash() {
        let e = entry("prefers-tabs");
        assert_eq!(
            e.index_line(),
            "- [prefers tabs](prefers-tabs.md) \u{2014} A one line hook."
        );
    }

    // ── index upsert ──────────────────────────────────────────────────────

    #[test]
    fn upsert_into_empty_index_creates_the_list() {
        let out = upsert_index_line("", "a.md", "- [a](a.md) — hook");
        assert_eq!(out, "- [a](a.md) — hook\n");
    }

    #[test]
    fn upsert_replaces_the_matching_pointer_in_place() {
        let existing = "- [a](a.md) — old\n- [b](b.md) — b hook\n";
        let out = upsert_index_line(existing, "a.md", "- [A](a.md) — new");
        assert_eq!(out, "- [A](a.md) — new\n- [b](b.md) — b hook\n");
    }

    #[test]
    fn upsert_appends_after_the_last_pointer() {
        let existing = "# Index\n\n- [a](a.md) — a\n- [b](b.md) — b\n\nSome trailing prose.\n";
        let out = upsert_index_line(existing, "c.md", "- [c](c.md) — c");
        assert_eq!(
            out,
            "# Index\n\n- [a](a.md) — a\n- [b](b.md) — b\n- [c](c.md) — c\n\nSome trailing prose.\n"
        );
    }

    /// Claude Code's own `MEMORY.md` is a bare bullet list with no heading; a
    /// write must not add one or reorder it.
    #[test]
    fn upsert_preserves_a_headerless_claude_index() {
        let existing = "- [Роль оркестратора](orchestrator-role.md) — код пишут субагенты\n";
        let out = upsert_index_line(
            existing,
            "readme-style.md",
            "- [Стиль](readme-style.md) — без фаз",
        );
        assert_eq!(
            out,
            "- [Роль оркестратора](orchestrator-role.md) — код пишут субагенты\n\
             - [Стиль](readme-style.md) — без фаз\n"
        );
    }

    #[test]
    fn upsert_keeps_a_preamble_when_there_are_no_pointers_yet() {
        let out = upsert_index_line("# Memory\n\n", "a.md", "- [a](a.md) — hook");
        assert_eq!(out, "# Memory\n\n- [a](a.md) — hook\n");
    }

    #[test]
    fn upsert_ignores_non_pointer_links() {
        let existing = "See [the docs](https://example.com) first.\n";
        let out = upsert_index_line(existing, "a.md", "- [a](a.md) — hook");
        assert_eq!(
            out,
            "See [the docs](https://example.com) first.\n\n- [a](a.md) — hook\n"
        );
    }

    // ── index removal ─────────────────────────────────────────────────────

    #[test]
    fn remove_drops_only_the_matching_pointer() {
        let existing = "- [a](a.md) — a\n- [b](b.md) — b\n- [c](c.md) — c\n";
        assert_eq!(
            remove_index_line(existing, "b.md"),
            "- [a](a.md) — a\n- [c](c.md) — c\n"
        );
    }

    /// The mirror of `upsert_index_line`'s contract: a delete must not be an
    /// excuse to rewrite the file. Headings, grouping and prose stay.
    #[test]
    fn remove_preserves_prose_and_grouping() {
        let existing = "# Index\n\nSome preamble.\n\n## Build\n- [a](a.md) — a\n- [b](b.md) — b\n\n\
                        See [the docs](https://example.com).\n";
        assert_eq!(
            remove_index_line(existing, "a.md"),
            "# Index\n\nSome preamble.\n\n## Build\n- [b](b.md) — b\n\n\
             See [the docs](https://example.com).\n"
        );
    }

    #[test]
    fn removing_an_absent_pointer_changes_nothing() {
        let existing = "# Index\n\n- [a](a.md) — a\n";
        assert_eq!(remove_index_line(existing, "zzz.md"), existing);
    }

    /// Deleting the last entry must not leave a file holding one newline, which
    /// would render as an empty bullet list.
    #[test]
    fn removing_the_last_pointer_empties_the_index() {
        assert_eq!(remove_index_line("- [a](a.md) — a\n", "a.md"), "");
    }

    // ── pointer parsing ───────────────────────────────────────────────────

    #[test]
    fn a_pointer_line_parses_into_its_three_parts() {
        let p = parse_pointer_line("- [Роль оркестратора](orchestrator-role.md) — код пишут")
            .expect("parses");
        assert_eq!(p.title, "Роль оркестратора");
        assert_eq!(p.target, "orchestrator-role.md");
        assert_eq!(p.description, "код пишут");
    }

    /// A pointer with no hook is still a pointer; treating it as prose would
    /// hide the entry from the index block and from a delete.
    #[test]
    fn a_pointer_without_a_hook_still_parses() {
        let p = parse_pointer_line("- [a](a.md)").expect("parses");
        assert_eq!(p.target, "a.md");
        assert!(p.description.is_empty());
        assert_eq!(pointer_line("a", "a.md", ""), "- [a](a.md)");
    }

    #[test]
    fn prose_and_plain_links_are_not_pointers() {
        assert!(parse_pointer_line("See [the docs](https://example.com).").is_none());
        assert!(parse_pointer_line("## Group").is_none());
        assert!(parse_pointer_line("").is_none());
    }

    // ── deleting ──────────────────────────────────────────────────────────

    #[test]
    fn delete_removes_the_file_and_its_pointer() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        s.write_entry(MemoryScope::Workspace, &entry("keep-me"))
            .unwrap();
        let doomed = s
            .write_entry(MemoryScope::Workspace, &entry("drop-me"))
            .unwrap();

        let deleted = s
            .delete_entry(MemoryScope::Workspace, "drop-me")
            .unwrap()
            .expect("entry existed");
        assert_eq!(deleted.path, doomed.path);
        assert_eq!(deleted.description.as_deref(), Some("A one line hook."));
        assert!(!doomed.path.exists());

        let index = std::fs::read_to_string(&doomed.index_path).unwrap();
        assert!(!index.contains("(drop-me.md)"), "{index}");
        assert!(index.contains("(keep-me.md)"), "the other entry stays");
    }

    /// The name is folded the same way a write folds it, so `/memory` and the
    /// model can both delete `"Prefers Tabs"` by the name they wrote it under.
    #[test]
    fn delete_slugifies_the_name_like_a_write() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        s.write_entry(MemoryScope::Global, &entry("Prefers Tabs"))
            .unwrap();
        assert!(
            s.delete_entry(MemoryScope::Global, "Prefers Tabs!")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn deleting_an_absent_entry_is_not_an_error() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        assert!(
            s.delete_entry(MemoryScope::Workspace, "never-existed")
                .unwrap()
                .is_none()
        );
    }

    /// Scopes are separate stores. A delete in one must not reach into the
    /// other, and `entry_scopes` is what tells an unqualified caller so.
    #[test]
    fn delete_is_scoped_and_ambiguity_is_visible() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        s.write_entry(MemoryScope::Global, &entry("a-fact"))
            .unwrap();
        s.write_entry(MemoryScope::Workspace, &entry("a-fact"))
            .unwrap();
        assert_eq!(
            s.entry_scopes("a-fact"),
            vec![MemoryScope::Workspace, MemoryScope::Global],
        );

        s.delete_entry(MemoryScope::Workspace, "a-fact").unwrap();
        assert_eq!(s.entry_scopes("a-fact"), vec![MemoryScope::Global]);
        assert!(s.entry_path(MemoryScope::Global, "a-fact").exists());
    }

    // ── index reading ─────────────────────────────────────────────────────

    #[test]
    fn pointer_lines_keep_file_order_and_drop_everything_else() {
        let index = "# Memory\n\nSome preamble.\n\n\
                     - [a](a.md) — first\n\
                     ## Group\n\
                     - [b](b.md) — second\n\n\
                     See [the docs](https://example.com).\n";
        assert_eq!(
            index_pointer_lines(index),
            vec!["- [a](a.md) — first", "- [b](b.md) — second"],
        );
    }

    #[test]
    fn pointer_lines_of_an_empty_index_are_empty() {
        assert!(index_pointer_lines("").is_empty());
        assert!(index_pointer_lines("# Memory\n\n_(nothing yet)_\n").is_empty());
    }

    /// An indented pointer (a nested list in a hand-grouped index) is still a
    /// pointer, and comes back without its indentation so a renderer can put
    /// it under its own heading.
    #[test]
    fn pointer_lines_are_trimmed() {
        assert_eq!(
            index_pointer_lines("  - [a](a.md) — hook  \n"),
            vec!["- [a](a.md) — hook"],
        );
    }

    // ── writing ───────────────────────────────────────────────────────────

    #[test]
    fn write_entry_creates_file_and_index() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let written = s
            .write_entry(MemoryScope::Workspace, &entry("a-fact"))
            .unwrap();

        assert!(written.created);
        assert!(written.path.ends_with("memories/a-fact.md"));
        assert!(written.index_path.ends_with("memories/MEMORY.md"));

        let index = std::fs::read_to_string(&written.index_path).unwrap();
        assert!(index.contains("(a-fact.md)"), "{index}");
        let body = std::fs::read_to_string(&written.path).unwrap();
        assert!(body.starts_with("---\nname: a-fact\n"), "{body}");
    }

    /// Rewriting a name is the update path: one file, one index line, no dupes.
    #[test]
    fn rewriting_a_name_updates_in_place() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        s.write_entry(MemoryScope::Workspace, &entry("a-fact"))
            .unwrap();

        let revised = MemoryEntry::new(
            "a-fact",
            None,
            "Revised hook.",
            EntryType::Project,
            "New body.",
        )
        .unwrap();
        let written = s.write_entry(MemoryScope::Workspace, &revised).unwrap();
        assert!(!written.created, "second write must report an update");

        let index = std::fs::read_to_string(&written.index_path).unwrap();
        assert_eq!(index.matches("(a-fact.md)").count(), 1, "{index}");
        assert!(index.contains("Revised hook."), "{index}");
        let body = std::fs::read_to_string(&written.path).unwrap();
        assert!(body.contains("New body."));
        assert!(!body.contains("The body."));
    }

    #[test]
    fn scopes_write_to_separate_directories() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let g = s
            .write_entry(MemoryScope::Global, &entry("a-fact"))
            .unwrap();
        let w = s
            .write_entry(MemoryScope::Workspace, &entry("a-fact"))
            .unwrap();
        assert_ne!(g.path, w.path);
        assert!(g.path.starts_with(s.global_dir()));
        assert!(w.path.starts_with(s.workspace_dir()));
    }

    /// A silent skip would tell the model "saved" for a write that never
    /// happened, and it would keep citing the memory for the rest of the session.
    #[test]
    fn ephemeral_workspace_write_errors_instead_of_no_op() {
        let tmp = TempDir::new().unwrap();
        // A literal `/tmp` path rather than `TempDir`: `is_ephemeral_cwd` also
        // consults `TMPDIR`, which the test runner may point elsewhere.
        let s = MemoryStorage::new(
            Path::new("/tmp/grok-entry-ephemeral-test"),
            Some(&tmp.path().join("memory")),
        );
        assert!(s.is_ephemeral(), "a /tmp cwd must be ephemeral");
        assert!(matches!(
            s.write_entry(MemoryScope::Workspace, &entry("a-fact")),
            Err(EntryError::Ephemeral)
        ));
        // Global scope still works from a temp cwd.
        assert!(s.write_entry(MemoryScope::Global, &entry("a-fact")).is_ok());
    }

    // ── discovery ─────────────────────────────────────────────────────────

    #[test]
    fn list_memories_puts_the_index_first() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        s.write_entry(MemoryScope::Global, &entry("zebra")).unwrap();
        s.write_entry(MemoryScope::Global, &entry("alpha")).unwrap();

        let files = s.list_memories(MemoryScope::Global);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["MEMORY.md", "alpha.md", "zebra.md"]);
    }

    #[test]
    fn list_memories_is_empty_when_the_directory_is_absent() {
        let tmp = TempDir::new().unwrap();
        assert!(storage(&tmp).list_memories(MemoryScope::Global).is_empty());
    }

    /// The reason entries live one level down instead of taking over
    /// `MEMORY.md`: `dream` rewrites the scope-root file from scratch on every
    /// consolidation, which would erase an index kept there.
    #[test]
    fn dream_rewriting_memory_md_leaves_entries_untouched() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        s.ensure_initialized().unwrap();
        let written = s
            .write_entry(MemoryScope::Workspace, &entry("a-fact"))
            .unwrap();

        // Exactly what dream does on a successful consolidation.
        s.write_long_term(
            MemoryScope::Workspace,
            "# Project Memory\n\n## Fresh\n\nRewritten.",
        )
        .unwrap();

        assert!(written.path.exists(), "entry file survived");
        assert!(written.index_path.exists(), "entry index survived");
        let root = std::fs::read_to_string(s.workspace_memory_file()).unwrap();
        assert!(
            root.contains("Rewritten."),
            "dream still owns the root file"
        );
        assert!(!root.contains("a-fact"), "the two files stay separate");
    }

    /// Entries must reach the startup reindex and `/memory browse`, and must be
    /// classified by scope rather than as session residue.
    #[test]
    fn entries_are_listed_and_classified_by_scope() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let g = s
            .write_entry(MemoryScope::Global, &entry("global-fact"))
            .unwrap();
        let w = s
            .write_entry(MemoryScope::Workspace, &entry("ws-fact"))
            .unwrap();

        let files = s.list_memory_files().unwrap();
        assert!(files.contains(&g.path), "global entry listed: {files:?}");
        assert!(files.contains(&w.path), "workspace entry listed: {files:?}");

        assert_eq!(s.classify_source(&g.path), "global");
        assert_eq!(s.classify_source(&w.path), "workspace");
        assert_eq!(s.classify_source(&w.index_path), "workspace");
    }

    #[test]
    fn is_memories_path_recognizes_both_scopes() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        assert!(s.is_memories_path(&s.memories_dir(MemoryScope::Global).join("a.md")));
        assert!(s.is_memories_path(&s.memories_dir(MemoryScope::Workspace).join("a.md")));
        assert!(!s.is_memories_path(&s.global_dir().join("MEMORY.md")));
        assert!(!s.is_memories_path(&s.workspace_dir().join("sessions").join("a.md")));
    }
}
