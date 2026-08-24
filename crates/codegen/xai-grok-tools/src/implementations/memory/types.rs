//! Input/output types for memory tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Input for the `memory_search` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MemorySearchInput {
    /// The search query string. Use specific technical terms rather than
    /// conversational language. Good: "authentication middleware patterns".
    /// Bad: "that thing we discussed about auth".
    pub query: String,
    /// Maximum number of results to return.
    ///
    /// When omitted the backend-configured value is used (typically 6 from
    /// `[memory.search].max_results`), so leaving this unset is preferred
    /// for normal queries.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Minimum relevance score threshold.
    ///
    /// When omitted the backend-configured value is used (typically 0.0 from
    /// `[memory.search].min_score`).
    #[serde(default)]
    pub min_score: Option<f64>,
}

/// Output schema for `memory_search` (used for JSON Schema generation only).
#[derive(Debug, JsonSchema)]
pub struct MemorySearchOutput {
    /// Formatted search results as markdown text.
    pub results: String,
}

/// Input for the `memory_get` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MemoryGetInput {
    /// Path to the memory file to read.
    pub path: String,
    /// 1-based start line, matching the line numbers in the tool's output
    /// (default: beginning of file). 0 is accepted and treated as 1.
    #[serde(default)]
    pub from: Option<usize>,
    /// Maximum number of lines to return (default: all).
    #[serde(default)]
    pub lines: Option<usize>,
}

/// Output schema for `memory_get` (used for JSON Schema generation only).
#[derive(Debug, JsonSchema)]
pub struct MemoryGetOutput {
    /// File content (optionally line-limited).
    pub content: String,
}

/// Input for the `memory_write` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MemoryWriteInput {
    /// Short kebab-case slug naming this one fact, e.g. `prefers-tabs` or
    /// `auth-token-refresh`. It becomes the file name and is the update key:
    /// writing the same name again replaces that memory rather than adding a
    /// second one. Spaces and capitals are folded into the slug.
    pub name: String,
    /// One line describing what this memory holds, written so a future session
    /// can judge from the line alone whether to open the file. This is what
    /// appears next to the memory in the index.
    pub description: String,
    /// What kind of fact this is. Also picks the default scope: `user` and
    /// `feedback` are saved globally, `project` and `reference` to this project.
    #[serde(rename = "type")]
    pub entry_type: crate::types::memory_backend::MemoryEntryType,
    /// The memory itself, as markdown. Keep it to the one fact named above;
    /// link related memories with `[[other-memory-name]]`.
    pub content: String,
    /// Human-readable title shown in the index. Defaults to the name with
    /// dashes replaced by spaces.
    #[serde(default)]
    pub title: Option<String>,
    /// Override where this is saved. Omit to follow the default for `type`.
    #[serde(default)]
    pub scope: Option<crate::types::memory_backend::MemoryWriteScope>,
}

/// Output schema for `memory_write` (used for JSON Schema generation only).
#[derive(Debug, JsonSchema)]
pub struct MemoryWriteOutput {
    /// Confirmation of what was stored and where.
    pub result: String,
}

/// Input for the `memory_delete` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MemoryDeleteInput {
    /// Name of the memory to remove — the `name.md` on its line in the memory
    /// index, without the extension. Spaces and capitals are folded the same
    /// way `memory_write` folds them.
    pub name: String,
    /// Which store to remove it from. Omit unless the same name exists in
    /// both, in which case you will be told to pick one.
    #[serde(default)]
    pub scope: Option<crate::types::memory_backend::MemoryWriteScope>,
}

/// Output schema for `memory_delete` (used for JSON Schema generation only).
#[derive(Debug, JsonSchema)]
pub struct MemoryDeleteOutput {
    /// Confirmation of what was removed, or why nothing was.
    pub result: String,
}
