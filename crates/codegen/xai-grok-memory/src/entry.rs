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
        format!(
            "- [{}]({}) \u{2014} {}",
            self.title,
            self.file_name(),
            self.description
        )
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

/// Extract the link target of a `- [Title](target) — hook` pointer line.
fn pointer_target(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("- [")?;
    let close = rest.find("](")?;
    let after = &rest[close + 2..];
    let end = after.find(')')?;
    Some(&after[..end])
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
