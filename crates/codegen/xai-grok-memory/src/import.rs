//! One-way import of a Claude Code memory directory into grok's own store.
//!
//! Claude Code keeps per-project memory at
//! `~/.claude/projects/{sanitized-cwd}/memory/`, in exactly the layout
//! [`crate::entry`] writes: one `.md` per fact with `name` / `description` /
//! `metadata.type` frontmatter, plus a `MEMORY.md` of `- [Title](name.md) — hook`
//! pointer lines. Nothing here converts anything — an entry file is copied
//! byte-for-byte and its pointer line is rebuilt from the source index — so the
//! only real work is deciding *where* each entry lands and *when* not to write.
//!
//! ## Rules
//!
//! - **Never writes the source.** Everything under the Claude Code directory is
//!   opened read-only. That directory belongs to another program, and a user
//!   who runs this must not find their Claude Code memory rearranged.
//! - **Never overwrites.** An entry whose name already exists in the
//!   destination scope is skipped and reported. Import is not authoritative
//!   over what the user or the model wrote here, and a rename would leave two
//!   near-identical entries the model cannot tell apart in the index.
//! - **Routes by type, not by origin.** Claude Code memory is per-project, but
//!   grok has global and workspace scopes and already maps `metadata.type` onto
//!   them ([`EntryType::default_scope`]). A `user` or `feedback` memory is about
//!   the person and follows them; `project` and `reference` stay here. An entry
//!   with no usable type is treated as `project`, which is both Claude Code's
//!   own default and the contained choice.
//!
//! Together the first two rules make the import idempotent without a ledger
//! file: a second run finds every name present and writes nothing. The cost is
//! that an entry edited in Claude Code after the import will not come back
//! across — deliberate, because this is a migration onto one store rather than
//! a sync between two. `memory_delete` plus a re-run is the way to take a newer
//! copy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::entry::{
    EntryType, MAX_NAME_CHARS, MEMORIES_INDEX_FILE, pointer_line, upsert_index_line,
};
use crate::storage::{MemoryScope, MemoryStorage, slugify};

/// Claude Code's project-directory encoding: every character that is not
/// `[A-Za-z0-9]` becomes `-`, with no run collapsing and no case folding, so
/// `/home/u/grok-build` is `-home-u-grok-build`.
///
/// Reproducing it exactly is the whole lookup: collapse the runs and the
/// directory is simply not found.
pub fn sanitize_project_path(dir: &Path) -> String {
    dir.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Where Claude Code keeps the memory for one project.
///
/// `home` is a parameter rather than read from the environment so the resolver
/// is a pure function a test can point at a fixture.
pub fn claude_memory_dir_for(home: &Path, project_dir: &Path) -> PathBuf {
    home.join(".claude")
        .join("projects")
        .join(sanitize_project_path(project_dir))
        .join("memory")
}

/// Why one source file did not become an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The destination scope already holds an entry of that name.
    AlreadyPresent(MemoryScope),
    /// Nothing in the name survives slugification.
    UnusableName,
    /// The source file could not be read as UTF-8 text.
    Unreadable(String),
    /// Workspace-scoped, in a temp-dir cwd where workspace memory is not kept.
    Ephemeral,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyPresent(scope) => {
                write!(f, "already in {} memory", scope_label(*scope))
            }
            Self::UnusableName => write!(f, "no usable name"),
            Self::Unreadable(e) => write!(f, "unreadable: {e}"),
            Self::Ephemeral => write!(f, "workspace memory is not kept for a temporary directory"),
        }
    }
}

/// One entry that crossed over.
#[derive(Debug, Clone)]
pub struct ImportedEntry {
    /// Slug it was stored under.
    pub name: String,
    /// Where it came from.
    pub source: PathBuf,
    /// Where it now lives.
    pub path: PathBuf,
    /// The index that now points at it.
    pub index_path: PathBuf,
    /// Scope it landed in.
    pub scope: MemoryScope,
}

/// What an import did, in enough detail to tell the user why an entry is
/// missing.
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// Directory that was read.
    pub source: PathBuf,
    /// `false` when there was no such directory — the ordinary case for a
    /// project that was never opened in Claude Code, and not a failure.
    pub source_present: bool,
    /// Entries written, in source order.
    pub imported: Vec<ImportedEntry>,
    /// Source file name and why it was left alone.
    pub skipped: Vec<(String, SkipReason)>,
}

impl ImportReport {
    /// Every file the import touched: each new entry plus each index it
    /// changed, deduplicated, for the caller to feed to the search index.
    pub fn written_paths(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for entry in &self.imported {
            for path in [&entry.path, &entry.index_path] {
                if !out.contains(path) {
                    out.push(path.clone());
                }
            }
        }
        out
    }

    /// A user-facing summary. Names the skipped entries, because "skipped 4" on
    /// its own reads as data loss.
    pub fn summary(&self) -> String {
        if !self.source_present {
            return format!(
                "No Claude Code memory for this project at {}.",
                self.source.display()
            );
        }
        if self.imported.is_empty() && self.skipped.is_empty() {
            return format!("No memories to import from {}.", self.source.display());
        }

        let mut out = format!(
            "Imported {} of {} memories from {}.",
            self.imported.len(),
            self.imported.len() + self.skipped.len(),
            self.source.display()
        );
        for scope in [MemoryScope::Workspace, MemoryScope::Global] {
            let names: Vec<&str> = self
                .imported
                .iter()
                .filter(|e| e.scope == scope)
                .map(|e| e.name.as_str())
                .collect();
            if !names.is_empty() {
                out.push_str(&format!("\n  {}: {}", scope_label(scope), names.join(", ")));
            }
        }
        for (name, reason) in &self.skipped {
            out.push_str(&format!("\n  skipped {name} — {reason}"));
        }
        if self.skipped.iter().any(is_collision) {
            out.push_str(
                "\nAn entry already here is never overwritten. To take the Claude Code \
                 version of one, delete it first and import again.",
            );
        }
        out
    }
}

fn is_collision((_, reason): &(String, SkipReason)) -> bool {
    matches!(reason, SkipReason::AlreadyPresent(_))
}

fn scope_label(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Global => "global",
        MemoryScope::Workspace => "workspace",
    }
}

/// Copy every entry in a Claude Code memory directory that grok does not
/// already have.
///
/// Reads `source` and writes only under `storage`'s `memories/` directories.
/// A missing `source` is reported, not an error: asking to import when there is
/// nothing to import is a reasonable thing to do once.
pub fn import_claude_memories(storage: &MemoryStorage, source: &Path) -> ImportReport {
    let mut report = ImportReport {
        source: source.to_path_buf(),
        source_present: source.is_dir(),
        imported: Vec::new(),
        skipped: Vec::new(),
    };
    if !report.source_present {
        return report;
    }

    // The source index is the only place the human-written titles live; the
    // file names are kebab slugs. Read it once and key it by file name.
    let source_index =
        std::fs::read_to_string(source.join(MEMORIES_INDEX_FILE)).unwrap_or_default();
    let titles: HashMap<&str, (&str, &str)> = source_index
        .lines()
        .filter_map(crate::entry::parse_pointer_line)
        .map(|p| (p.target, (p.title, p.description)))
        .collect();

    for source_path in entry_files(source) {
        let file_name = source_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = match std::fs::read_to_string(&source_path) {
            Ok(t) => t,
            Err(e) => {
                report
                    .skipped
                    .push((file_name, SkipReason::Unreadable(e.to_string())));
                continue;
            }
        };
        let front = parse_frontmatter(&text);

        let stem = source_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let raw_name = front.get("name").filter(|n| !n.is_empty()).unwrap_or(&stem);
        let name = slugify(raw_name.trim(), MAX_NAME_CHARS);
        if name.is_empty() {
            report.skipped.push((file_name, SkipReason::UnusableName));
            continue;
        }

        let entry_type = front
            .get("type")
            .and_then(|t| EntryType::parse(t))
            .unwrap_or(EntryType::Project);
        let scope = entry_type.default_scope();
        if scope == MemoryScope::Workspace && storage.is_ephemeral() {
            report.skipped.push((file_name, SkipReason::Ephemeral));
            continue;
        }

        let dest = storage.entry_path(scope, &name);
        if dest.exists() {
            report
                .skipped
                .push((file_name, SkipReason::AlreadyPresent(scope)));
            continue;
        }

        // Title and hook come from the source index when it has a pointer to
        // this file, so a curated title survives; frontmatter is the fallback.
        let dest_file_name = format!("{name}.md");
        let (title, description) = match titles.get(file_name.as_str()) {
            Some((title, description)) if !title.is_empty() => {
                (title.to_string(), description.to_string())
            }
            _ => (
                name.replace('-', " "),
                front.get("description").cloned().unwrap_or_default(),
            ),
        };

        let dir = storage.memories_dir(scope);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            report
                .skipped
                .push((file_name, SkipReason::Unreadable(e.to_string())));
            continue;
        }
        // Byte-for-byte: the frontmatter Claude Code writes is a superset of
        // grok's (`node_type`, `originSessionId`), nothing here parses it back,
        // and re-rendering would silently drop those and impose the model-facing
        // body cap on memory the user already has.
        if let Err(e) = std::fs::write(&dest, &text) {
            report
                .skipped
                .push((file_name, SkipReason::Unreadable(e.to_string())));
            continue;
        }

        let index_path = dir.join(MEMORIES_INDEX_FILE);
        let existing = std::fs::read_to_string(&index_path).unwrap_or_default();
        let line = pointer_line(&title, &dest_file_name, &description);
        let updated = upsert_index_line(&existing, &dest_file_name, &line);
        if let Err(e) = std::fs::write(&index_path, updated) {
            tracing::warn!(
                path = %index_path.display(),
                error = %e,
                "memory import: entry written but the index could not be updated"
            );
        }

        report.imported.push(ImportedEntry {
            name,
            source: source_path,
            path: dest,
            index_path,
            scope,
        });
    }

    tracing::info!(
        source = %source.display(),
        imported = report.imported.len(),
        skipped = report.skipped.len(),
        "memory import: finished"
    );
    report
}

/// The `.md` files in a memory directory, sorted, excluding the index and
/// dotfiles.
fn entry_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().and_then(|e| e.to_str()) == Some("md")
                && p.file_name().is_some_and(|n| {
                    n != MEMORIES_INDEX_FILE && !n.to_string_lossy().starts_with('.')
                })
        })
        .collect();
    files.sort();
    files
}

/// Flatten a leading YAML frontmatter block to `key -> value`, ignoring
/// nesting.
///
/// Deliberately lenient rather than a YAML parser: the only keys read back are
/// `name`, `description` and `type`, all scalars, and their nesting differs
/// between what grok writes (`metadata:` then two-space `type:`) and what
/// Claude Code writes (`metadata: ` with a trailing space, plus `node_type` and
/// `originSessionId`). A strict parser would refuse files that are perfectly
/// usable; flattening reads both. No key here appears at two depths.
fn parse_frontmatter(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return out;
    };
    for line in rest.lines() {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let value = unquote(value.trim());
        if !value.is_empty() {
            out.insert(key.to_string(), value);
        }
    }
    out
}

/// Undo the quoting [`crate::entry`]'s `yaml_scalar` (and Claude Code) apply to
/// values that would not survive plain style.
fn unquote(value: &str) -> String {
    // A lone `"` cannot be a quoted scalar: stripping the prefix leaves an
    // empty string, which then has no suffix to strip.
    let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return value.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut escaped = false;
    for c in inner.chars() {
        match (escaped, c) {
            (false, '\\') => escaped = true,
            _ => {
                out.push(c);
                escaped = false;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{MemoryEntry, index_pointer_lines};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn storage(tmp: &TempDir) -> MemoryStorage {
        let global = tmp.path().join("memory");
        let workspace = global.join("proj-abc12345");
        MemoryStorage::with_paths(global, workspace)
    }

    /// Writes a Claude-Code-shaped source directory. `entries` is
    /// `(file stem, type, description, body)`; the index gets a real title so
    /// the "titles survive" case is covered by default.
    fn claude_dir(tmp: &TempDir, entries: &[(&str, &str, &str, &str)]) -> PathBuf {
        let dir = tmp.path().join("claude-memory");
        std::fs::create_dir_all(&dir).unwrap();
        let mut index = String::new();
        for (stem, ty, description, body) in entries {
            std::fs::write(
                dir.join(format!("{stem}.md")),
                format!(
                    "---\nname: {stem}\ndescription: \"{description}\"\nmetadata: \n  \
                     node_type: memory\n  type: {ty}\n  originSessionId: abc-123\n  \
                     modified: 2026-08-03T16:35:12.045Z\n---\n\n{body}\n"
                ),
            )
            .unwrap();
            index.push_str(&format!(
                "- [Title of {stem}]({stem}.md) \u{2014} {description}\n"
            ));
        }
        std::fs::write(dir.join("MEMORY.md"), index).unwrap();
        dir
    }

    /// Path, bytes and mtime for every file under a directory. The mtime is in
    /// there so a rewrite with identical content still shows up.
    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
        let mut out = BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_file() {
                let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                out.insert(path.clone(), (std::fs::read(&path).unwrap(), modified));
            }
        }
        out
    }

    // ── the Claude Code lookup ────────────────────────────────────────────

    /// Claude Code does not collapse runs and does not fold case. Getting
    /// either wrong means the directory is simply never found.
    #[test]
    fn project_path_sanitization_matches_claude_code() {
        assert_eq!(
            sanitize_project_path(Path::new("/home/minicx/grok-build")),
            "-home-minicx-grok-build"
        );
        assert_eq!(
            sanitize_project_path(Path::new("/home/minicx/.config/nixos")),
            "-home-minicx--config-nixos",
            "runs are not collapsed"
        );
        assert_eq!(
            sanitize_project_path(Path::new("/home/u/OrcaSlicer")),
            "-home-u-OrcaSlicer",
            "case is preserved"
        );
    }

    #[test]
    fn claude_memory_dir_is_under_the_project_encoding() {
        assert_eq!(
            claude_memory_dir_for(Path::new("/home/u"), Path::new("/home/u/proj")),
            PathBuf::from("/home/u/.claude/projects/-home-u-proj/memory")
        );
    }

    // ── read-only at the source ───────────────────────────────────────────

    /// The source belongs to another program. Nothing in an import may write
    /// there — not the entries, not its index, not a ledger file.
    #[test]
    fn the_source_directory_is_never_written() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let source = claude_dir(
            &tmp,
            &[
                ("a-fact", "project", "a hook", "A body."),
                ("b-fact", "user", "b hook", "B body."),
            ],
        );
        let before = snapshot(&source);

        let first = import_claude_memories(&s, &source);
        assert_eq!(first.imported.len(), 2);
        assert_eq!(
            snapshot(&source),
            before,
            "import must not touch the source"
        );

        // The second run collides on every name; that path must not write either.
        let second = import_claude_memories(&s, &source);
        assert_eq!(second.imported.len(), 0);
        assert_eq!(
            snapshot(&source),
            before,
            "a re-run must not touch it either"
        );
    }

    // ── scope routing ─────────────────────────────────────────────────────

    /// Claude Code memory is per-project, but grok has two scopes and already
    /// maps `metadata.type` onto them. A `user` fact must not be pinned to the
    /// repo it happened to be recorded in.
    #[test]
    fn entries_are_routed_by_type_not_by_origin() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let source = claude_dir(
            &tmp,
            &[
                ("prefers-tabs", "user", "indent", "Tabs."),
                ("no-rebase", "feedback", "corrected", "Do not rebase."),
                ("build-cmd", "project", "how to build", "cargo build."),
                ("sqlite-fts", "reference", "fts5", "Contentless tables."),
            ],
        );
        let report = import_claude_memories(&s, &source);
        assert_eq!(report.imported.len(), 4, "{:?}", report.skipped);

        let scope_of = |name: &str| {
            report
                .imported
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.scope)
                .unwrap()
        };
        assert_eq!(scope_of("prefers-tabs"), MemoryScope::Global);
        assert_eq!(scope_of("no-rebase"), MemoryScope::Global);
        assert_eq!(scope_of("build-cmd"), MemoryScope::Workspace);
        assert_eq!(scope_of("sqlite-fts"), MemoryScope::Workspace);
    }

    /// An unreadable or missing type must land in the contained scope, not the
    /// one that pollutes every future session.
    #[test]
    fn an_unknown_type_defaults_to_the_workspace() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let source = tmp.path().join("claude-memory");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("bare.md"), "just a body, no frontmatter\n").unwrap();
        std::fs::write(
            source.join("odd.md"),
            "---\nname: odd\nmetadata:\n  type: wat\n---\n\nBody.\n",
        )
        .unwrap();

        let report = import_claude_memories(&s, &source);
        assert_eq!(report.imported.len(), 2, "{:?}", report.skipped);
        assert!(
            report
                .imported
                .iter()
                .all(|e| e.scope == MemoryScope::Workspace)
        );
    }

    // ── idempotence and collisions ────────────────────────────────────────

    /// Two rules, one behaviour: nothing here is ever overwritten, so a second
    /// run writes nothing and needs no ledger file to know that.
    #[test]
    fn a_second_run_imports_nothing_and_changes_nothing() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let source = claude_dir(&tmp, &[("a-fact", "project", "a hook", "A body.")]);

        let first = import_claude_memories(&s, &source);
        assert_eq!(first.imported.len(), 1);
        let dest_dir = s.memories_dir(MemoryScope::Workspace);
        let after_first = snapshot(&dest_dir);

        let second = import_claude_memories(&s, &source);
        assert!(second.imported.is_empty());
        assert_eq!(
            second.skipped,
            vec![(
                "a-fact.md".to_string(),
                SkipReason::AlreadyPresent(MemoryScope::Workspace)
            )]
        );
        assert_eq!(snapshot(&dest_dir), after_first, "a re-run is a no-op");
    }

    /// The entry the user wrote here wins, always. Silently replacing it would
    /// lose work that the import has no claim to be more current than.
    #[test]
    fn an_entry_written_here_is_never_overwritten() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let mine = MemoryEntry::new(
            "a-fact",
            Some("Mine"),
            "My own hook.",
            EntryType::Project,
            "My own body.",
        )
        .unwrap();
        let written = s.write_entry(MemoryScope::Workspace, &mine).unwrap();

        let source = claude_dir(&tmp, &[("a-fact", "project", "their hook", "Their body.")]);
        let report = import_claude_memories(&s, &source);

        assert!(report.imported.is_empty());
        assert_eq!(
            report.skipped,
            vec![(
                "a-fact.md".to_string(),
                SkipReason::AlreadyPresent(MemoryScope::Workspace)
            )]
        );
        let body = std::fs::read_to_string(&written.path).unwrap();
        assert!(body.contains("My own body."), "{body}");
        assert!(!body.contains("Their body."));
        assert!(
            report.summary().contains("delete it first"),
            "the summary says how to override"
        );
    }

    /// The same name in the other scope is a different entry, and must still
    /// import.
    #[test]
    fn a_name_taken_in_one_scope_does_not_block_the_other() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let mine = MemoryEntry::new("a-fact", None, "hook", EntryType::User, "body").unwrap();
        s.write_entry(MemoryScope::Global, &mine).unwrap();

        let source = claude_dir(&tmp, &[("a-fact", "project", "their hook", "Their body.")]);
        let report = import_claude_memories(&s, &source);
        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.imported[0].scope, MemoryScope::Workspace);
    }

    // ── what lands on disk ────────────────────────────────────────────────

    /// The format was chosen so that no conversion is needed. Copying the bytes
    /// keeps `node_type` and `originSessionId`, which grok does not write but
    /// Claude Code reads, and keeps a body longer than the model-facing cap.
    #[test]
    fn entry_files_are_copied_byte_for_byte() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let big = "x".repeat(crate::entry::MAX_BODY_CHARS * 4);
        let source = claude_dir(&tmp, &[("a-fact", "project", "a hook", &big)]);
        let source_bytes = std::fs::read(source.join("a-fact.md")).unwrap();

        let report = import_claude_memories(&s, &source);
        assert_eq!(report.imported.len(), 1, "{:?}", report.skipped);
        assert_eq!(
            std::fs::read(&report.imported[0].path).unwrap(),
            source_bytes
        );
        let text = String::from_utf8(source_bytes).unwrap();
        assert!(text.contains("node_type: memory"));
        assert!(text.contains("originSessionId: abc-123"));
    }

    /// Claude Code's index carries titles a person wrote, over kebab file
    /// names. Regenerating the line from the slug would throw them away.
    #[test]
    fn the_index_line_keeps_the_source_title_and_hook() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let source = tmp.path().join("claude-memory");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("orchestrator-role.md"),
            "---\nname: orchestrator-role\ndescription: fallback hook\nmetadata:\n  \
             type: project\n---\n\nBody.\n",
        )
        .unwrap();
        std::fs::write(
            source.join("MEMORY.md"),
            "- [Роль оркестратора](orchestrator-role.md) \u{2014} код пишут субагенты\n",
        )
        .unwrap();

        let report = import_claude_memories(&s, &source);
        assert_eq!(report.imported.len(), 1);
        let index = std::fs::read_to_string(&report.imported[0].index_path).unwrap();
        assert_eq!(
            index_pointer_lines(&index),
            vec!["- [Роль оркестратора](orchestrator-role.md) \u{2014} код пишут субагенты"],
        );
    }

    /// A source with no index still has to produce usable pointer lines, from
    /// the frontmatter.
    #[test]
    fn a_source_without_an_index_falls_back_to_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let source = tmp.path().join("claude-memory");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("a-fact.md"),
            "---\nname: a-fact\ndescription: \"the: hook\"\nmetadata:\n  type: project\n---\n\nB.\n",
        )
        .unwrap();

        let report = import_claude_memories(&s, &source);
        let index = std::fs::read_to_string(&report.imported[0].index_path).unwrap();
        assert_eq!(
            index_pointer_lines(&index),
            vec!["- [a fact](a-fact.md) \u{2014} the: hook"],
            "a quoted YAML scalar is unquoted for the index line"
        );
    }

    /// The imported entries must join the index the prefix reads, not sit in a
    /// separate list — and an index grok already had must survive the merge.
    #[test]
    fn imported_entries_join_the_existing_index() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let mine = MemoryEntry::new("mine", None, "my hook", EntryType::Project, "body").unwrap();
        s.write_entry(MemoryScope::Workspace, &mine).unwrap();

        let source = claude_dir(&tmp, &[("theirs", "project", "their hook", "Body.")]);
        import_claude_memories(&s, &source);

        let index = std::fs::read_to_string(s.memories_index_file(MemoryScope::Workspace)).unwrap();
        let lines = index_pointer_lines(&index);
        assert_eq!(lines.len(), 2, "{index}");
        assert!(index.contains("(mine.md)"), "{index}");
        assert!(index.contains("(theirs.md)"), "{index}");
    }

    /// `list_memory_files` is what the startup reindex walks; an imported entry
    /// that is not in it is not searchable at the next start either.
    #[test]
    fn imported_entries_are_listed_for_reindexing() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let source = claude_dir(
            &tmp,
            &[
                ("a-fact", "project", "a hook", "A body."),
                ("b-fact", "user", "b hook", "B body."),
            ],
        );
        let report = import_claude_memories(&s, &source);
        let listed = s.list_memory_files().unwrap();
        for entry in &report.imported {
            assert!(listed.contains(&entry.path), "{:?} not listed", entry.path);
        }
        for path in report.written_paths() {
            assert!(path.is_file(), "{path:?} must exist to be reindexed");
        }
    }

    // ── absent and odd sources ────────────────────────────────────────────

    #[test]
    fn a_missing_source_is_reported_not_an_error() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let report = import_claude_memories(&s, &tmp.path().join("nope"));
        assert!(!report.source_present);
        assert!(report.imported.is_empty());
        assert!(report.summary().contains("No Claude Code memory"));
    }

    /// The source index is not an entry, and dotfiles (the plugin left a
    /// `.import-state.json`, and editors leave their own) are not either.
    #[test]
    fn the_source_index_and_dotfiles_are_not_imported() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp);
        let source = claude_dir(&tmp, &[("a-fact", "project", "a hook", "A body.")]);
        std::fs::write(source.join(".hidden.md"), "---\nname: hidden\n---\n\nx\n").unwrap();

        let report = import_claude_memories(&s, &source);
        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.imported[0].name, "a-fact");
        assert!(!s.entry_path(MemoryScope::Workspace, "MEMORY").exists());
    }

    /// Workspace memory is not kept for a temp-dir cwd, so a `project` entry
    /// has nowhere to go and must say so rather than be silently dropped.
    #[test]
    fn an_ephemeral_workspace_skips_project_entries_but_keeps_user_ones() {
        let tmp = TempDir::new().unwrap();
        let s = MemoryStorage::new(
            Path::new("/tmp/grok-import-ephemeral-test"),
            Some(&tmp.path().join("memory")),
        );
        assert!(s.is_ephemeral());
        let source = claude_dir(
            &tmp,
            &[
                ("a-fact", "project", "a hook", "A body."),
                ("b-fact", "user", "b hook", "B body."),
            ],
        );
        let report = import_claude_memories(&s, &source);
        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.imported[0].name, "b-fact");
        assert_eq!(
            report.skipped,
            vec![("a-fact.md".to_string(), SkipReason::Ephemeral)]
        );
    }

    // ── frontmatter reading ───────────────────────────────────────────────

    /// Claude Code writes `metadata: ` with a trailing space and two extra
    /// keys; grok writes `metadata:` with neither. One reader has to take both.
    #[test]
    fn frontmatter_reads_both_dialects() {
        let claude = parse_frontmatter(
            "---\nname: a\ndescription: \"h\"\nmetadata: \n  node_type: memory\n  \
             type: feedback\n  originSessionId: x\n---\n\nbody\n",
        );
        assert_eq!(claude.get("name").map(String::as_str), Some("a"));
        assert_eq!(claude.get("description").map(String::as_str), Some("h"));
        assert_eq!(claude.get("type").map(String::as_str), Some("feedback"));

        let grok =
            parse_frontmatter("---\nname: a\ndescription: h\nmetadata:\n  type: user\n---\n");
        assert_eq!(grok.get("type").map(String::as_str), Some("user"));
    }

    #[test]
    fn frontmatter_of_a_bare_file_is_empty() {
        assert!(parse_frontmatter("no frontmatter here\n").is_empty());
    }

    #[test]
    fn quoted_scalars_are_unescaped() {
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(unquote("\"a: b\""), "a: b");
        assert_eq!(unquote(r#""say \"hi\" \\ bye""#), r#"say "hi" \ bye"#);
        assert_eq!(unquote("\""), "\"", "a lone quote is not a quoted scalar");
    }
}
