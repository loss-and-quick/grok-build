//! The memory index, folded into the `<user_info>` prefix.
//!
//! First-turn injection ([`turn`](super::turn)) is
//! *retrieval*: one search, with the first turn's query, whose top hits become
//! `<memory_context>`. A memory that does not score for that one query is
//! invisible for the whole session, and the model cannot search for something
//! it does not know exists. This module injects *awareness* instead — one
//! pointer line per saved entry, taken from each scope's `memories/MEMORY.md`
//! — so the model can decide for itself which body is worth a `memory_get`.
//! The two are separately switchable ([`MemoryIndexInjectionConfig`]) because
//! they answer different questions.
//!
//! It lands in the `<user_info>` prefix for the reason
//! [`session_start_context`](super::session_start_context) gives: that
//! message is the one piece of session-scoped context the pipeline already
//! re-establishes verbatim after a compaction and on a model switch, so the
//! index does not evaporate at the moment the conversation loses everything
//! else.
//!
//! Cache stability comes from *where* it lands rather than from a snapshot.
//! The prefix is built once per session segment — at
//! [`SessionActor::ensure_prefix_ready`], at compaction, at a model switch —
//! and every request in that segment replays the same bytes. So an entry
//! `memory_write` adds mid-session does not rewrite the prefix and does not
//! bust the KV cache; it is already visible to the model through the write's
//! own tool result and through `memory_search`, and it joins the index at the
//! next segment boundary. Rebuilds re-read the files because the surrounding
//! prefix (git status, the date) is regenerated there anyway.
//!
//! [`MemoryIndexInjectionConfig`]: crate::config::MemoryIndexInjectionConfig

use super::*;

/// Hard cap (in characters) on the pointer lines of the rendered index.
///
/// An index grows without limit; the prefix it rides in is paid for on every
/// request of the session. Matching
/// [`SESSION_START_CONTEXT_MAX_CHARS`](super::session_start_context::SESSION_START_CONTEXT_MAX_CHARS)
/// buys roughly 40 entries at a typical line length, which is well past what a
/// hand-curated store holds.
///
/// Past the cap whole lines are dropped — never a clipped line, which would
/// leave a half-written description reading as a fact — and the block says how
/// many it dropped. A silently short index would look complete and teach the
/// model that what it cannot see does not exist.
pub(crate) const MEMORY_INDEX_MAX_CHARS: usize = 4_000;

/// One scope's contribution to the index block.
pub(crate) struct IndexScope<'a> {
    /// How the scope is named to the model.
    pub label: &'a str,
    /// Directory the scope's entry files live in, so a pointer line's relative
    /// file name can be turned into a `memory_get` path.
    pub dir: String,
    /// The scope's pointer lines, in index-file order.
    pub lines: Vec<&'a str>,
}

/// Render the index block body (no wrapper tag — the caller adds its own), or
/// `None` when no scope holds an entry.
///
/// Scopes keep the order they are given, and the cap is one budget shared
/// across all of them: callers pass the workspace first so that when a large
/// global store crowds the budget, the lines lost are the less specific ones.
/// A scope with no entries is left out entirely rather than shown empty — on a
/// fresh install that means nothing is injected at all, which is the honest
/// answer: there is no index to consult and no reason to spend a token saying
/// so.
pub(crate) fn format_memory_index(scopes: &[IndexScope<'_>]) -> Option<String> {
    if scopes.iter().all(|s| s.lines.is_empty()) {
        return None;
    }
    let mut out = String::from(
        "Memories saved for you in earlier sessions, one line per entry. Open one in \
         full with `memory_get` on the scope's directory plus the file name in its \
         line.\n",
    );
    let mut budget = MEMORY_INDEX_MAX_CHARS;
    for scope in scopes {
        if scope.lines.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {} — {}\n", scope.label, scope.dir));
        let mut shown = 0usize;
        for line in &scope.lines {
            let cost = line.chars().count() + 1;
            if cost > budget {
                break;
            }
            budget -= cost;
            out.push_str(line);
            out.push('\n');
            shown += 1;
        }
        let omitted = scope.lines.len() - shown;
        if omitted > 0 {
            out.push_str(&format!(
                "({omitted} further entries are not listed here; `memory_search` still \
                 finds them.)\n"
            ));
        }
    }
    Some(out)
}

/// What the memory index came to for this session: the block to inject, or
/// why there is none.
///
/// The reason is carried rather than logged where it is found, because the
/// same render answers two different questions. `/context` asks what the
/// prefix holds, which is a read; only [`SessionActor::with_memory_index`]
/// injects. A telemetry event written from the shared render would say an
/// injection happened every time a user opened `/context`, and anything
/// counting that marker would be reading `/context` invocations as injections.
pub(super) enum MemoryIndexOutcome {
    /// Rendered and ready: the block, and the number of entries it points at.
    Ready { block: String, entries: usize },
    /// Entries exist, but the agent's catalog has no `memory_get` to open one.
    NoOpener { entries: usize },
    /// Nothing to show: injection is off, there is no storage, or the store is
    /// empty.
    Nothing,
}

impl SessionActor {
    /// Read both scopes' indexes and render the block, with the number of
    /// entries it points at and, when there is none, why.
    ///
    /// Writes no telemetry: see [`MemoryIndexOutcome`]. The count comes back
    /// alongside the text because `/context` reports it and re-deriving it
    /// from the rendered block would have to parse back out what this function
    /// already knows.
    ///
    /// Skipped when the session's tool catalog has no `memory_get`: an agent
    /// that cannot open an entry has no use for a list of entries, and the
    /// block would be dead weight in every one of its requests. This is the
    /// gate the out-of-tree memory plugin applies to its own injection, moved
    /// to the tool the injected text actually asks the model to call.
    ///
    /// The two index files are read synchronously on the actor's own task, and
    /// stay that way. Each session actor owns a dedicated thread and runtime
    /// ([`spawn_session_actor`](super::spawn)), so the only thing a read can
    /// delay is the session that asked for it; the files are a few kilobytes
    /// of pointer lines, usually warm because the same reads happen at every
    /// prefix build and on every `memory_write`; and the added caller is
    /// `/context`, which is human-paced and runs once per invocation. Handing
    /// them to `spawn_blocking` would park this `!Send` actor at the same
    /// point anyway, buying a hop for a read measured in microseconds.
    pub(super) async fn memory_index_outcome(&self) -> MemoryIndexOutcome {
        if !self.memory.index_injection_config.enabled {
            return MemoryIndexOutcome::Nothing;
        }
        let Some(storage) = self.memory.storage() else {
            return MemoryIndexOutcome::Nothing;
        };
        let read =
            |scope| std::fs::read_to_string(storage.memories_index_file(scope)).unwrap_or_default();
        let (workspace, global) = (
            read(crate::session::memory::MemoryScope::Workspace),
            read(crate::session::memory::MemoryScope::Global),
        );
        use xai_grok_memory::entry::index_pointer_lines;
        let scopes = [
            IndexScope {
                label: "This workspace",
                dir: storage
                    .memories_dir(crate::session::memory::MemoryScope::Workspace)
                    .display()
                    .to_string(),
                lines: index_pointer_lines(&workspace),
            },
            IndexScope {
                label: "Global",
                dir: storage
                    .memories_dir(crate::session::memory::MemoryScope::Global)
                    .display()
                    .to_string(),
                lines: index_pointer_lines(&global),
            },
        ];
        let entries: usize = scopes.iter().map(|s| s.lines.len()).sum();
        if entries == 0 {
            return MemoryIndexOutcome::Nothing;
        }
        let tool_names = self.registered_tool_names().await;
        if !tool_names
            .iter()
            .any(|n| n == xai_grok_tools::implementations::memory::MEMORY_GET_TOOL_NAME)
        {
            return MemoryIndexOutcome::NoOpener { entries };
        }
        match format_memory_index(&scopes) {
            Some(block) => MemoryIndexOutcome::Ready { block, entries },
            None => MemoryIndexOutcome::Nothing,
        }
    }

    /// The rendered index block for a read-only report of what the prefix
    /// holds, such as `/context`. Injects nothing and records nothing.
    pub(super) async fn memory_index_block(&self) -> Option<(String, usize)> {
        match self.memory_index_outcome().await {
            MemoryIndexOutcome::Ready { block, entries } => Some((block, entries)),
            MemoryIndexOutcome::NoOpener { .. } | MemoryIndexOutcome::Nothing => None,
        }
    }

    /// Append the memory index to a freshly built `<user_info>` prefix,
    /// wrapped exactly like [`SessionActor::push_system_reminder`].
    ///
    /// Called at the three prefix-building sites, and only those, for the same
    /// reason [`SessionActor::with_session_start_context`] is: appending inside
    /// `build_user_message_prefix` would race the background build armed at
    /// `Initialize`, and appending at more than one consumption point would
    /// duplicate the block.
    ///
    /// This is also the only place the injection is logged, because it is the
    /// only place an injection happens.
    pub(super) async fn with_memory_index(&self, prefix: String) -> String {
        let (body, entries) = match self.memory_index_outcome().await {
            MemoryIndexOutcome::Ready { block, entries } => (block, entries),
            MemoryIndexOutcome::NoOpener { entries } => {
                tracing::info!(
                    target: xai_grok_telemetry::memory_log::TARGET,
                    entries,
                    "MEMORY_INDEX_INJECT: skipped -- agent has no memory_get to open an entry with"
                );
                return prefix;
            }
            MemoryIndexOutcome::Nothing => return prefix,
        };
        tracing::info!(
            target: xai_grok_telemetry::memory_log::TARGET,
            entries,
            block_chars = body.chars().count(),
            "MEMORY_INDEX_INJECT: folded the memory index into the session prefix"
        );
        let tag = self.reminder_wrapper_tag();
        let body = body.replace(&format!("</{tag}>"), &format!("<\\/{tag}>"));
        format!("{prefix}\n\n<{tag}>\n{body}</{tag}>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope<'a>(label: &'a str, lines: Vec<&'a str>) -> IndexScope<'a> {
        IndexScope {
            label,
            dir: format!("/memory/{label}"),
            lines,
        }
    }

    #[test]
    fn a_store_with_no_entries_renders_nothing() {
        assert!(format_memory_index(&[]).is_none());
        assert!(format_memory_index(&[scope("Global", vec![])]).is_none());
    }

    #[test]
    fn an_absent_scope_is_left_out_rather_than_shown_empty() {
        let block = format_memory_index(&[
            scope("This workspace", vec![]),
            scope("Global", vec!["- [a](a.md) — hook"]),
        ])
        .expect("one populated scope renders");
        assert!(!block.contains("This workspace"));
        assert!(block.contains("## Global — /memory/Global"));
        assert!(block.contains("- [a](a.md) — hook"));
    }

    #[test]
    fn both_scopes_are_labelled_with_the_directory_to_read_from() {
        let block = format_memory_index(&[
            scope("This workspace", vec!["- [w](w.md) — ws"]),
            scope("Global", vec!["- [g](g.md) — gl"]),
        ])
        .expect("two populated scopes render");
        let ws = block.find("## This workspace").expect("workspace section");
        let gl = block.find("## Global").expect("global section");
        assert!(ws < gl, "the more specific scope comes first");
        assert!(block.contains("memory_get"));
    }

    /// The cap must drop whole lines and admit it: a clipped final line would
    /// read as a fact with a truncated description, and a silent drop would
    /// tell the model the store holds only what it can see.
    #[test]
    fn an_oversized_index_is_capped_and_says_so() {
        let line = format!("- [{}](x.md) — {}", "t".repeat(40), "d".repeat(150));
        let lines: Vec<&str> = std::iter::repeat_n(line.as_str(), 200).collect();
        let block = format_memory_index(&[scope("Global", lines)]).expect("renders");
        assert!(
            block.chars().count() < MEMORY_INDEX_MAX_CHARS + 500,
            "block stayed near the budget"
        );
        for rendered in block.lines().filter(|l| l.starts_with("- [")) {
            assert_eq!(rendered, line, "every listed line is a whole line");
        }
        let shown = block.lines().filter(|l| l.starts_with("- [")).count();
        assert!(
            block.contains(&format!("({} further entries", 200 - shown)),
            "the block reports what it dropped: {block}"
        );
    }

    /// The budget is shared, and the workspace is passed first, so a large
    /// global store cannot squeeze out the scope closest to the work.
    #[test]
    fn the_workspace_scope_gets_the_budget_first() {
        let big = format!("- [{}](x.md) — {}", "t".repeat(40), "d".repeat(150));
        let global: Vec<&str> = std::iter::repeat_n(big.as_str(), 200).collect();
        let block = format_memory_index(&[
            scope("This workspace", vec!["- [w](w.md) — ws"]),
            scope("Global", global),
        ])
        .expect("renders");
        assert!(block.contains("- [w](w.md) — ws"));
        assert!(block.contains("further entries are not listed"));
    }
}
