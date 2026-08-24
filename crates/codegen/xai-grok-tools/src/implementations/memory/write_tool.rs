//! `memory_write` tool — persist one fact at the model's own request.

use std::sync::Arc;

use super::types::MemoryWriteInput;
use crate::types::memory_backend::{MemoryBackend, MemoryWriteRequest};
use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};

#[derive(Debug, Default)]
pub struct MemoryWriteImpl;

impl crate::types::tool_metadata::ToolMetadata for MemoryWriteImpl {
    fn kind(&self) -> ToolKind {
        ToolKind::MemoryWrite
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Save one durable fact to memory so future sessions can find it. Each call writes \
         one file holding one fact, and adds it to the memory index.\n\n\
         Use this when:\n\
         - The user asks you to remember something\n\
         - The user corrects you in a way that should not need repeating\n\
         - You learn a durable convention, preference, or piece of project knowledge\n\n\
         Do not use it for anything the next session could just read out of the code, for \
         the state of work in progress, or for facts that expire on their own.\n\n\
         Writing an existing `name` replaces that memory — that is how you correct or \
         extend one. Search first with `memory_search` if you are unsure whether a memory \
         on the topic already exists. Reference related memories from the body with \
         `[[other-memory-name]]`."
    }
}

impl xai_tool_runtime::Tool for MemoryWriteImpl {
    type Args = MemoryWriteInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("memory_write").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "memory_write",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    /// Mutating, so `ToolScope::Write` — the computer hub must route it to the
    /// leader agent only. It takes no path: the target is derived from the
    /// slugified `name` inside grok's own memory directory, so unlike the file
    /// tools there is nothing here for a workspace permission rule to gate.
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
        input: MemoryWriteInput,
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
            "MEMORY_WRITE: invoked"
        );

        let request = MemoryWriteRequest {
            name: input.name,
            title: input.title,
            description: input.description,
            entry_type: input.entry_type,
            scope: input.scope,
            content: input.content,
        };

        // Validation failures come back as tool output, not `ToolError`: the
        // model can fix a too-long body or an unusable name on the next call,
        // and it needs to read why to do that.
        let outcome = match memory.write(request).await {
            Ok(o) => o,
            Err(e) => {
                tracing::info!(
                    target: crate::types::memory_backend::MEMORY_LOG_TARGET,
                    error = %e,
                    "MEMORY_WRITE: rejected"
                );
                return Ok(ToolOutput::Text(format!("Memory not written: {e}").into()));
            }
        };

        let verb = if outcome.created { "Saved" } else { "Updated" };
        tracing::info!(
            target: crate::types::memory_backend::MEMORY_LOG_TARGET,
            created = outcome.created,
            "MEMORY_WRITE: complete"
        );
        Ok(ToolOutput::Text(
            format!(
                "{verb} {} memory at {}\nIndexed in {}\nSearchable now via memory_search.",
                outcome.scope, outcome.path, outcome.index_path,
            )
            .into(),
        ))
    }
}
