//! `/memory import` — bring a Claude Code memory directory into the store.
//!
//! The mechanics are in [`crate::session::memory::import`]; what lives here is the
//! session-level part it cannot do: finding the source from the session's cwd,
//! and making what was imported searchable in the session that imported it.
//!
//! ## Why a command and not a tool or a startup hook
//!
//! The out-of-tree memory plugin imported automatically at `session_start`,
//! once per project per process. That is defensible for a plugin the user
//! installed *for* that purpose; it is not defensible for behaviour built into
//! the core, where the same startup would silently copy another program's data
//! into the store that now rides in every prompt, with no prompt and no undo.
//!
//! Nor is it a model-facing tool. The model has no basis for deciding to pull
//! in the user's Claude Code memory, and a tool it should never call on its own
//! still costs schema tokens in every request. Import is a migration a person
//! performs once — and a migration whose whole value is the report it prints,
//! since the entries it refuses to overwrite are the ones the user has to
//! reconcile by hand.
//!
//! Note what is *already* free: `MemoryStorage::list_memory_files` walks each
//! scope's `memories/` directory, so a folder copied in by hand is picked up by
//! the next startup reindex with no import step at all. What this command adds
//! is locating the source, routing entries across grok's two scopes, refusing
//! to clobber, and indexing without a restart.

use super::*;

impl SessionActor {
    /// Run an import and return the text to show the user.
    ///
    /// `source` overrides the derived directory, for a memory recorded under a
    /// different project path (a repo since moved, or a worktree).
    pub(super) async fn run_memory_import(&self, source: Option<String>) -> String {
        let Some(storage) = self.memory.storage() else {
            return "Memory is not enabled for this session. Turn it on with `/memory on` \
                    first."
                .to_owned();
        };

        let source = match source {
            // A slash command's argument arrives unexpanded, and `~` is how a
            // user writes a path under their home.
            Some(path) => match path.strip_prefix("~/").zip(dirs::home_dir()) {
                Some((rest, home)) => home.join(rest),
                None => std::path::PathBuf::from(path),
            },
            None => {
                let Some(home) = dirs::home_dir() else {
                    return "Cannot locate your home directory, so there is nowhere to look \
                            for a Claude Code memory directory. Pass one: `/memory import \
                            <dir>`."
                        .to_owned();
                };
                crate::session::memory::import::claude_memory_dir_for(
                    &home,
                    std::path::Path::new(&self.session_info.cwd),
                )
            }
        };

        let report = crate::session::memory::import::import_claude_memories(&storage, &source);
        tracing::info!(
            target: xai_grok_telemetry::memory_log::TARGET,
            source = %source.display(),
            imported = report.imported.len(),
            skipped = report.skipped.len(),
            "MEMORY_IMPORT: finished"
        );

        // Index inline, for the reason `memory_write` does: the point of
        // importing now is that the memories are usable now, and leaving it to
        // the watcher means a `memory_search` this session may or may not see
        // them depending on which sync happens to fire first. The index block
        // in the prefix is a separate matter — it is built once per session
        // segment, so the imported entries appear there at the next compaction
        // or model switch, and until then the model reaches them by search.
        // One open index and one embedding pass for the whole batch, rather
        // than `reindex_and_embed` per file: an import touches every entry at
        // once, and reopening the database and rebuilding the embedding client
        // once per memory is the difference between a pause and a stall.
        let written = report.written_paths();
        if !written.is_empty()
            && let Some(mut index) = self.memory.open_index(&storage)
        {
            for path in &written {
                if let Err(e) = index.reindex_file(path, storage.classify_source(path)) {
                    tracing::warn!(
                        target: xai_grok_telemetry::memory_log::TARGET,
                        path = %path.display(),
                        error = %e,
                        "MEMORY_IMPORT: could not index an imported entry"
                    );
                }
            }
            if let Some(ref params) = self.memory.backend_params
                && let Some(provider) = params.make_embedding_provider().await
            {
                crate::session::memory::embed_missing_chunks(&index, &provider).await;
            }
        }

        let mut out = report.summary();
        if !report.imported.is_empty() {
            out.push_str(
                "\nSearchable now; they join the memory index in the prefix at the next \
                 compaction or model switch.",
            );
        }
        out
    }
}
