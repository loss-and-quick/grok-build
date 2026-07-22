//! The subagent-orchestration seam between the plugin host and the core.
//!
//! The host serves the `agent_*` plugin RPCs but knows nothing about sessions
//! or coordinators: the shell injects an [`AgentOrchestrator`] (via
//! [`crate::PluginHost::set_agent_orchestrator`]) that routes every operation
//! through the session's existing subagent coordinator channel — plugin-spawned
//! subagents are real children of the session, visible in the TUI like any
//! Task-tool spawn. This mirrors the [`crate::SpawnHardener`] idiom: an
//! injected callback keeps this crate free of any shell dependency.

use std::future::Future;
use std::pin::Pin;

// Re-exported so orchestrator implementations (the shell) can name the wire
// status without a direct protocol dependency.
pub use xai_grok_plugin_protocol::AgentStatusDto;

/// Boxed future for the async orchestrator methods (object safety).
pub type OrchestratorFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A spawn request forwarded from `agent_spawn`, already defaulted by the host
/// (`plugin` names the requesting plugin, for descriptions/attribution).
#[derive(Debug, Clone)]
pub struct AgentSpawnSpec {
    pub plugin: String,
    /// `None` → the orchestrator's default agent type.
    pub agent_type: Option<String>,
    pub prompt: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<String>,
}

/// A successfully submitted spawn: its id plus the terminal-result channel.
/// The sender side is dropped without a value only when the session is torn
/// down before the subagent finishes (the host reports that as a failure).
pub struct SpawnedSubagent {
    pub id: String,
    pub result_rx: tokio::sync::oneshot::Receiver<AgentOutcome>,
}

/// A subagent's terminal result, mapped onto the wire vocabulary.
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    /// Never `Running` — this is a terminal summary.
    pub status: AgentStatusDto,
    pub output: String,
    pub error: Option<String>,
    pub tokens_used: u64,
    pub duration_ms: u64,
    pub tool_calls: u32,
    pub turns: u32,
}

impl AgentOutcome {
    /// A synthetic failure for infrastructure errors (coordinator gone,
    /// timeout-cancel that never produced a result).
    pub fn infra_failure(status: AgentStatusDto, error: impl Into<String>) -> Self {
        Self {
            status,
            output: String::new(),
            error: Some(error.into()),
            tokens_used: 0,
            duration_ms: 0,
            tool_calls: 0,
            turns: 0,
        }
    }
}

/// A live-progress snapshot for a still-running subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProgress {
    /// `initializing` or `running`.
    pub phase: &'static str,
    pub turns: u32,
    pub tool_calls: u32,
    pub tokens_used: u64,
    pub elapsed_ms: u64,
}

/// One spawnable agent type's metadata, as the orchestrator reports it. The
/// host maps this to the wire `AgentDescriptorDto`; keeping it a plain core
/// type (like [`SpawnedSubagent`]) keeps this crate free of shell types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDescriptor {
    pub name: String,
    pub description: String,
    /// Explicit model id; `None` when the agent inherits the session's model.
    pub model: Option<String>,
}

/// Cancellation outcome, mirroring the coordinator's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestratorCancel {
    Cancelled,
    AlreadyFinished,
    NotFound,
}

/// Injected seam the `agent_*` capability handlers call. Implemented by the
/// shell over the session's subagent coordinator channel; every method is
/// callable from the host's plain-`tokio::spawn` request tasks (`Send`).
pub trait AgentOrchestrator: Send + Sync {
    /// Submit a spawn. Errors are submission failures only (coordinator
    /// unavailable); spec-level failures (unknown type, bad model) surface as
    /// the subagent's terminal result.
    fn spawn(&self, spec: AgentSpawnSpec) -> Result<SpawnedSubagent, String>;

    /// A progress snapshot for a running subagent; `None` when the id is
    /// unknown or already terminal (the outcome channel covers terminals).
    fn progress<'a>(&'a self, id: &'a str) -> OrchestratorFuture<'a, Option<AgentProgress>>;

    /// Cancel a subagent by id.
    fn cancel<'a>(&'a self, id: &'a str) -> OrchestratorFuture<'a, OrchestratorCancel>;

    /// Spawnable agent types for this session (sorted, toggle-filtered), each
    /// with its name, description, and explicit model override.
    fn list_agent_types<'a>(&'a self) -> OrchestratorFuture<'a, Vec<AgentDescriptor>>;
}
