//! Regression tests for the memory extension's request shapes.
//!
//! The handler itself needs a resident session, which these do not build. What
//! they pin is the part a second client depends on: how `x.ai/memory/note`
//! reads its params, so a browser sending the documented payload gets the write
//! the terminal gets.

use super::*;

fn note_request(json: &str) -> Result<NoteRequest, serde_json::Error> {
    serde_json::from_str(json)
}

/// The scope a client omits is the one `/remember` has always written.
#[test]
fn a_note_without_a_scope_goes_to_global_memory() {
    let req = note_request(r#"{"sessionId":"s1","text":"deploys need the staging flag"}"#)
        .expect("parse");

    assert_eq!(req.session_id, "s1");
    assert_eq!(req.text, "deploys need the staging flag");
    assert!(matches!(req.scope, NoteScope::Global));
}

/// The other file is reachable by naming it, so wanting it later does not need
/// a second method.
#[test]
fn a_note_can_name_the_workspace_file() {
    let req = note_request(r#"{"sessionId":"s1","text":"n","scope":"workspace"}"#).expect("parse");

    assert!(matches!(req.scope, NoteScope::Workspace));
}

/// There is no path parameter, so a caller cannot aim a write at a directory of
/// its choosing: the workspace comes from the session the agent already has.
#[test]
fn a_note_cannot_name_a_file_of_its_own() {
    let req = note_request(
        r#"{"sessionId":"s1","text":"n","path":"/etc/passwd","cwd":"/somewhere/else"}"#,
    )
    .expect("parse");

    assert_eq!(req.session_id, "s1");
    assert_eq!(req.text, "n");
}

/// A scope this build does not know is refused rather than quietly filed
/// somewhere: a client asking for a destination that does not exist should hear
/// so.
#[test]
fn an_unknown_scope_is_refused() {
    assert!(note_request(r#"{"sessionId":"s1","text":"n","scope":"everywhere"}"#).is_err());
}

/// The session id is the addressing, so a request without one is not a request.
#[test]
fn a_note_without_a_session_is_refused() {
    assert!(note_request(r#"{"text":"n"}"#).is_err());
}

/// `MemoryScope` is the storage layer's enum and `NoteScope` is the wire's;
/// they must not drift apart silently.
#[test]
fn each_wire_scope_names_a_real_memory_file() {
    assert!(matches!(
        xai_grok_memory::MemoryScope::from(NoteScope::Global),
        xai_grok_memory::MemoryScope::Global
    ));
    assert!(matches!(
        xai_grok_memory::MemoryScope::from(NoteScope::Workspace),
        xai_grok_memory::MemoryScope::Workspace
    ));
}
