//! Extension handlers for `x.ai/compact_conversation`, `x.ai/memory/flush`, `x.ai/memory/note`, and `x.ai/memory/rewrite`.
//! `memory/rewrite` turns a raw memory note into structured markdown with a one-shot LLM call.
//! `memory/note` is the write that follows it: the rewrite already crossed, but saving the result did not, so only a client sharing the agent's disk could finish `/remember`.

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::session::{CompactConversationRequest, CompactConversationResponse, SessionCommand};

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        m if m.starts_with("x.ai/compact_conversation") => handle_compact(agent, args).await,
        "x.ai/memory/flush" => handle_flush(agent, args).await,
        "x.ai/memory/note" => handle_note(agent, args).await,
        "x.ai/memory/rewrite" => handle_rewrite(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_compact(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let req: CompactConversationRequest = parse_params(args)?;
    // send over the compact query here properly
    let sid: acp::SessionId = req.session_id.into();
    let session_handle = agent.resident_handle(&sid);
    let (tx, rx) = oneshot::channel();
    if let Some(session) = session_handle {
        let _ = session.cmd_tx.send(SessionCommand::CompactSession {
            user_context: req.user_context,
            respond_to: tx,
        });
    }
    // Pass the session error through; rewrapping buries the detail in a Debug dump.
    rx.await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))??;
    to_raw_response(&CompactConversationResponse {})
}

async fn handle_flush(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    struct MemoryFlushRequest {
        session_id: String,
    }

    let req: MemoryFlushRequest = parse_params(args)?;
    let not_found_err = format!("session not found: {}", req.session_id);
    let sid: acp::SessionId = req.session_id.into();
    let Some(session) = agent.resident_handle(&sid) else {
        return Err(acp::Error::invalid_params().data(not_found_err));
    };
    let (tx, rx) = oneshot::channel();
    let _ = session
        .cmd_tx
        .send(SessionCommand::FlushMemory { respond_to: tx });
    let flushed = rx
        .await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))?
        .map_err(|e| acp::Error::internal_error().data(format!("{:?}", e)))?;
    to_raw_response(&MemoryFlushResponse { flushed })
}

#[derive(Serialize)]
struct MemoryFlushResponse {
    flushed: bool,
}

/// Which memory file a note is appended to.
///
/// Named on the wire rather than hardcoded, because the storage layer has
/// always had both and a client that could only reach one would need a second
/// method the day it wanted the other.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum NoteScope {
    /// `~/.grok/memory/MEMORY.md`, shared across workspaces. The default,
    /// matching what `/remember` has always written.
    #[default]
    Global,
    /// The workspace's own `MEMORY.md`, under a directory keyed by the cwd.
    Workspace,
}

impl From<NoteScope> for xai_grok_memory::MemoryScope {
    fn from(scope: NoteScope) -> Self {
        match scope {
            NoteScope::Global => Self::Global,
            NoteScope::Workspace => Self::Workspace,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NoteRequest {
    /// Which session's workspace the note is filed under. Not a credential —
    /// see [`handle_note`] — but the only thing that names a destination.
    session_id: String,
    text: String,
    #[serde(default)]
    scope: NoteScope,
}

/// `x.ai/memory/note`: append a note to the user's memory file.
///
/// ## Who may call it
///
/// Anyone holding a live session, the same bar as `memory/flush` and
/// `memory/rewrite`. What the session id buys is not authentication but
/// addressing: the workspace the note is filed under comes from the session the
/// agent already has, never from the caller, so no client can aim a write at a
/// directory of its choosing. There is no path parameter for the same reason.
///
/// ## What it does to a file the user also edits
///
/// It appends and never rewrites. Concurrency therefore has two shapes and only
/// one of them is real. Two appends at once — two sessions, or a session and a
/// client — are ordered by the kernel and each lands whole, because the storage
/// layer writes one buffer to an `O_APPEND` handle. An append racing a hand
/// edit is the shape nothing here can fix: an editor saving a whole file writes
/// back a buffer it read earlier, so a note appended in between is lost with
/// it. That is a lost note, not a corrupted file, and it is the same exposure
/// the terminal has always had; making the write cross the wire neither adds
/// nor removes it.
async fn handle_note(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct NoteResponse {
        /// The file the note landed in, so a client can say where it went.
        path: String,
    }

    let req: NoteRequest = parse_params(args)?;
    let text = req.text.trim().to_owned();
    if text.is_empty() {
        return Err(acp::Error::invalid_params().data("memory note is empty"));
    }

    let sid: acp::SessionId = req.session_id.clone().into();
    let Some(session) = agent.resident_handle(&sid) else {
        return Err(
            acp::Error::invalid_params().data(format!("session not found: {}", req.session_id))
        );
    };
    let cwd = std::path::PathBuf::from(session.info.cwd.clone());
    let scope: xai_grok_memory::MemoryScope = req.scope.into();

    let path = tokio::task::spawn_blocking(move || {
        let storage = xai_grok_memory::MemoryStorage::new(&cwd, None);
        storage
            .append_to_memory(scope, &text)
            .map(|()| match scope {
                xai_grok_memory::MemoryScope::Global => storage.global_memory_file(),
                xai_grok_memory::MemoryScope::Workspace => storage.workspace_memory_file(),
            })
    })
    .await
    .map_err(|e| acp::Error::internal_error().data(format!("memory write panicked: {e}")))?
    .map_err(|e| acp::Error::internal_error().data(format!("memory write failed: {e}")))?;

    to_raw_response(&NoteResponse {
        path: path.display().to_string(),
    })
}

async fn handle_rewrite(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RewriteRequest {
        session_id: String,
        raw_text: String,
        context_summary: String,
    }

    let req: RewriteRequest = parse_params(args)?;
    let not_found_err = format!("session not found: {}", req.session_id);
    let sid: acp::SessionId = req.session_id.into();
    let Some(session) = agent.resident_handle(&sid) else {
        return Err(acp::Error::invalid_params().data(not_found_err));
    };
    let (tx, rx) = oneshot::channel();
    let _ = session.cmd_tx.send(SessionCommand::RewriteMemoryNote {
        raw_text: req.raw_text,
        context_summary: req.context_summary,
        respond_to: tx,
    });
    let rewritten = rx
        .await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))?
        .map_err(|e| acp::Error::internal_error().data(e))?;
    to_raw_response(&serde_json::json!({ "rewritten": rewritten }))
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
