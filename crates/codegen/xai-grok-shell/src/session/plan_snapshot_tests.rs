//! Regression tests for the saved plan on the wire.
//!
//! Two kinds live here. The read tests pin what the agent reports for a plan
//! file in each of the states a session can leave it in. The wire tests pin the
//! compatibility contract in both directions: a payload from a build that
//! predates a field still deserializes, and a payload from a newer one does not
//! break this build.

use super::*;

#[tokio::test]
async fn a_written_plan_reads_back_verbatim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plan = dir.path().join("plan.md");
    std::fs::write(&plan, "# Plan\n\n1. Land the wire method\n").expect("seed plan");

    let snapshot = PlanSnapshot::read(&plan, false).await;

    assert_eq!(
        snapshot.content.as_deref(),
        Some("# Plan\n\n1. Land the wire method\n")
    );
    assert_eq!(
        snapshot.path.as_deref(),
        Some(plan.display().to_string()).as_deref()
    );
    assert!(!snapshot.awaiting_approval);
}

#[tokio::test]
async fn a_missing_plan_is_reported_as_no_plan_rather_than_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plan = dir.path().join("plan.md");

    let snapshot = PlanSnapshot::read(&plan, false).await;

    assert_eq!(snapshot.content, None);
    // The path is still named: a client showing "no plan yet" can say which
    // file would hold one.
    assert_eq!(
        snapshot.path.as_deref(),
        Some(plan.display().to_string()).as_deref()
    );
}

/// A parked `exit_plan_mode` over a blank plan is the case the pager draws an
/// empty-plan placeholder for, so the two facts must be separable on the wire.
#[tokio::test]
async fn a_blank_plan_under_a_parked_approval_reports_both() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plan = dir.path().join("plan.md");
    std::fs::write(&plan, "   \n\n\t\n").expect("seed blank plan");

    let snapshot = PlanSnapshot::read(&plan, true).await;

    assert_eq!(snapshot.content, None, "whitespace is not a plan");
    assert!(snapshot.awaiting_approval);
}

#[test]
fn the_response_carries_the_session_it_answers_for() {
    let response = SessionPlanResponse {
        session_id: "sess-1".to_owned(),
        plan: PlanSnapshot {
            content: Some("body".to_owned()),
            path: Some("/home/u/.grok/sessions/p/sess-1/plan.md".to_owned()),
            awaiting_approval: true,
        },
    };

    let json = serde_json::to_string(&response).expect("serialize");
    assert!(json.contains("\"sessionId\":\"sess-1\""), "{json}");
    // Flattened, so a client reads the plan fields off the response directly
    // rather than through a nested object.
    assert!(json.contains("\"awaitingApproval\":true"), "{json}");
    assert!(!json.contains("\"plan\":"), "{json}");
}

/// An agent that predates a field sends a payload without it. Deserializing
/// must yield the field's zero, not fail.
#[test]
fn an_older_agents_payload_still_deserializes() {
    let snapshot: PlanSnapshot =
        serde_json::from_str(r#"{"content":"body"}"#).expect("older payload");

    assert_eq!(snapshot.content.as_deref(), Some("body"));
    assert_eq!(snapshot.path, None);
    assert!(!snapshot.awaiting_approval);
}

/// A newer agent sends fields this build has never heard of. Ignoring them is
/// the contract; erroring would make every added field a breaking change.
#[test]
fn a_newer_agents_payload_does_not_break_this_build() {
    let snapshot: PlanSnapshot = serde_json::from_str(
        r#"{"content":"body","awaitingApproval":true,"revisionCount":4,"authoredBy":"model"}"#,
    )
    .expect("newer payload");

    assert_eq!(snapshot.content.as_deref(), Some("body"));
    assert!(snapshot.awaiting_approval);
}

/// `None` content is omitted rather than sent as `null`, so a client reading
/// the field's presence sees the same thing as one reading its value.
#[test]
fn an_absent_plan_omits_the_content_field() {
    let json = serde_json::to_string(&PlanSnapshot::default()).expect("serialize");

    assert!(!json.contains("content"), "{json}");
    assert!(!json.contains("path"), "{json}");
    assert!(json.contains("\"awaitingApproval\":false"), "{json}");
}
