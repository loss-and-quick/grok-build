//! `memory_delete` tool — remove one saved fact.

use std::sync::Arc;

use super::types::MemoryDeleteInput;
use crate::types::memory_backend::{MemoryBackend, MemoryDeleteOutcome, MemoryDeleteRequest};
use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};

#[derive(Debug, Default)]
pub struct MemoryDeleteImpl;

impl crate::types::tool_metadata::ToolMetadata for MemoryDeleteImpl {
    fn kind(&self) -> ToolKind {
        ToolKind::MemoryDelete
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Remove one memory from the store, by the `name` on its line in the memory index. \
         The entry file is deleted, its line leaves the index, and it stops coming back \
         from `memory_search`.\n\n\
         Use this when:\n\
         - The user asks you to forget something\n\
         - A memory turned out to be wrong, and the correction does not belong in it\n\
         - A memory has expired: the convention changed, the project moved on\n\n\
         Prefer `memory_write` with the same name when the fact still holds but needs \
         fixing — that rewrites the entry in place and keeps its history of being \
         useful. Delete is for memories that should not exist, not for memories that \
         are out of date. If a name exists in both stores you will be asked which one \
         you meant; pass `scope` to say."
    }
}

impl xai_tool_runtime::Tool for MemoryDeleteImpl {
    type Args = MemoryDeleteInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("memory_delete").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "memory_delete",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    /// Mutating, and destructively so, but for the same reason as
    /// `memory_write` it carries no path: the target is derived from a
    /// slugified name inside grok's own memory directory, so there is nothing
    /// here for a workspace permission rule to gate.
    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(xai_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: MemoryDeleteInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let Some(memory) = resources
            .lock()
            .await
            .get::<Arc<dyn MemoryBackend>>()
            .cloned()
        else {
            return Ok(ToolOutput::Text(
                "Memory is not enabled. Use --experimental-memory to enable.".into(),
            ));
        };
        tracing::info!(
            target: crate::types::memory_backend::MEMORY_LOG_TARGET,
            "MEMORY_DELETE: invoked"
        );

        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Ok(ToolOutput::Text(
                "Memory not deleted: name is empty.".into(),
            ));
        }

        let request = MemoryDeleteRequest {
            name: name.clone(),
            scope: input.scope,
        };

        // As in `memory_write`, a refusal comes back as tool output rather than
        // a `ToolError`: every outcome here is something the model can act on
        // in its next call, and it has to read which one it got.
        let outcome = match memory.delete(request).await {
            Ok(o) => o,
            Err(e) => {
                tracing::info!(
                    target: crate::types::memory_backend::MEMORY_LOG_TARGET,
                    error = %e,
                    "MEMORY_DELETE: failed"
                );
                return Ok(ToolOutput::Text(format!("Memory not deleted: {e}").into()));
            }
        };

        let text = match outcome {
            MemoryDeleteOutcome::Deleted {
                path,
                scope,
                description,
                removed_chunks,
            } => {
                tracing::info!(
                    target: crate::types::memory_backend::MEMORY_LOG_TARGET,
                    removed_chunks,
                    "MEMORY_DELETE: complete"
                );
                // Echo the hook back: this is the only record of what the
                // memory said that survives the call, and the user reading the
                // transcript is the one who can tell if it was the wrong one.
                let hook = description
                    .map(|d| format!(" \u{2014} {d}"))
                    .unwrap_or_default();
                format!(
                    "Deleted {scope} memory \"{name}\"{hook}\nRemoved {path} and its index \
                     line. It no longer appears in memory_search."
                )
            }
            MemoryDeleteOutcome::NotFound => {
                format!(
                    "No memory named \"{name}\". Check the name against the memory index, \
                     or search for it with memory_search."
                )
            }
            MemoryDeleteOutcome::Ambiguous => {
                format!(
                    "Both stores hold a memory named \"{name}\", and they are different \
                     entries. Call again with scope set to \"global\" or \"project\" to say \
                     which one to remove."
                )
            }
        };
        Ok(ToolOutput::Text(text.into()))
    }
}
