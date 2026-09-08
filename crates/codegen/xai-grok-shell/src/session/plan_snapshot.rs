//! The session's saved plan, as a wire type.
//!
//! ## Why this is on the wire
//!
//! A plan is a file the agent owns: `~/.grok/sessions/<cwd>/<session id>/plan.md`,
//! written by the model under [`crate::session::plan_mode`]'s write gate. Only
//! one form of it ever crossed — the body attached to the `exit_plan_mode`
//! approval request ([`crate::session::acp_session_impl::tool_calls`]) — so a
//! client that was attached while the plan was being written could show it and
//! a client that attached afterwards could not. The pager was not affected
//! because it reads the file itself, which is exactly the asymmetry: a browser
//! has no such file.
//!
//! ## Why a method of its own
//!
//! Not `session/info`: that response is a snapshot of counters, asked for
//! repeatedly (the pager re-asks it after settings writebacks and on every
//! usage-panel open), and a plan body is an unbounded document. Bolting a
//! document onto a poll makes every poll pay for it.
//!
//! Not a notification: a plan is pulled, not watched. `/view-plan` asks "what
//! is the plan right now", and the one moment the agent genuinely has something
//! to push — the approval request — already pushes it.
//!
//! So it is a request: one round trip, when a client wants the document.
//!
//! ## Compatibility
//!
//! Every field carries `#[serde(default)]`, so an agent that predates a field
//! deserializes as its zero rather than failing, and a client that predates one
//! ignores it. A client talking to an agent too old to know the method gets
//! `method not found`, which is a client-side "no plan available", not an
//! error to show.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// What a client needs to draw the saved plan without touching a disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlanSnapshot {
    /// The plan body, or `None` when no plan has been written yet.
    ///
    /// A file that exists but holds only whitespace reads as `None`: an empty
    /// plan and a missing one are the same thing to a reader, and collapsing
    /// them here keeps every client from having to re-decide it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Absolute path of the plan file on the agent's machine.
    ///
    /// The one part of this a client cannot compute: it is `grok_home` joined
    /// with the URL-encoded cwd and the session id. Carried so a client can
    /// name the file it is showing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Whether an `exit_plan_mode` approval is parked on this plan.
    ///
    /// Lets a client that attached after the request draw the decision surface
    /// instead of an inert preview. `content: None` together with this set is
    /// the approved-but-empty case, which still needs a decision surface.
    pub awaiting_approval: bool,
}

impl PlanSnapshot {
    /// Read the plan file and pair it with whether an approval is parked on it.
    ///
    /// An unreadable file is `None`, not an error: "no plan yet" is the normal
    /// case and the only one a client can act on, so a missing file and an
    /// unreadable one are reported the same way rather than surfacing an
    /// `io::Error` a client cannot do anything with.
    pub async fn read(plan_file: &Path, awaiting_approval: bool) -> Self {
        let content = tokio::fs::read_to_string(plan_file)
            .await
            .ok()
            .filter(|body| !body.trim().is_empty());
        Self {
            content,
            path: Some(plan_file.display().to_string()),
            awaiting_approval,
        }
    }
}

/// Full wire response for `x.ai/session/plan`.
///
/// Wraps the snapshot with the session it belongs to, so a client that has more
/// than one session open can drop a reply that outlived the session it asked
/// about.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPlanResponse {
    pub session_id: String,
    #[serde(flatten)]
    pub plan: PlanSnapshot,
}

#[cfg(test)]
#[path = "plan_snapshot_tests.rs"]
mod tests;
