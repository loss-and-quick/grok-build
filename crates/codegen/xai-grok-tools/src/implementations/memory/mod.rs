//! Memory tools for cross-session knowledge retrieval.
//!
//! - `memory_search` — search indexed memory for relevant chunks
//! - `memory_get` — read a specific memory file by path
//! - `memory_write` — save one fact at the model's own request

pub mod get_tool;
pub mod search_tool;
pub mod types;
pub mod write_tool;

pub use get_tool::MemoryGetImpl;
pub use search_tool::MemorySearchImpl;
pub use write_tool::MemoryWriteImpl;

/// Registered name of the `memory_search` tool.
///
/// Single source of truth shared between the tool definition and any
/// gating callers (e.g. shell-side slash-command availability checks).
pub const MEMORY_SEARCH_TOOL_NAME: &str = "memory_search";

/// Registered name of the `memory_get` tool.
pub const MEMORY_GET_TOOL_NAME: &str = "memory_get";

/// Registered name of the `memory_write` tool.
pub const MEMORY_WRITE_TOOL_NAME: &str = "memory_write";

#[cfg(test)]
mod tests {
    use super::*;

    /// The constants are the wire identifier embedded in
    /// `AvailableCommandsUpdate._meta.tools` and matched by the shell's
    /// memory-gate predicate. A typo in either site silently disables
    /// `/flush` and `/dream`. Pin both halves.
    #[test]
    fn memory_tool_constants_match_registered_ids() {
        assert_eq!(MEMORY_SEARCH_TOOL_NAME, "memory_search");
        assert_eq!(MEMORY_GET_TOOL_NAME, "memory_get");
        assert_eq!(MEMORY_WRITE_TOOL_NAME, "memory_write");
        assert_eq!(
            xai_tool_runtime::Tool::id(&MemorySearchImpl).to_string(),
            MEMORY_SEARCH_TOOL_NAME
        );
        assert_eq!(
            xai_tool_runtime::Tool::id(&MemoryGetImpl).to_string(),
            MEMORY_GET_TOOL_NAME
        );
        assert_eq!(
            xai_tool_runtime::Tool::id(&MemoryWriteImpl).to_string(),
            MEMORY_WRITE_TOOL_NAME
        );
    }

    /// `memory_write` mutates; the other two must stay read-only. A regression
    /// either way changes which capability modes and subagents can call them.
    #[test]
    fn only_the_write_tool_is_mutating() {
        use crate::types::tool::ToolKind;
        assert!(ToolKind::MemorySearch.is_read_only());
        assert!(ToolKind::MemoryGet.is_read_only());
        assert!(!ToolKind::MemoryWrite.is_read_only());
    }
}
