use super::{QueueMutation, ServerRowCapabilities};

/// The whole policy: only `parent_agent_message` is protected on the session's own queue; everything is on a mirror.
#[test]
fn capabilities_truth_table() {
    let cases = [
        (
            "parent_agent_message",
            QueueMutation::PerRowKind,
            ServerRowCapabilities::PROTECTED,
        ),
        (
            "prompt",
            QueueMutation::PerRowKind,
            ServerRowCapabilities::EDITABLE,
        ),
        (
            "future",
            QueueMutation::PerRowKind,
            ServerRowCapabilities::EDITABLE,
        ),
        (
            "prompt",
            QueueMutation::ReadOnly,
            ServerRowCapabilities::PROTECTED,
        ),
        (
            "future",
            QueueMutation::ReadOnly,
            ServerRowCapabilities::PROTECTED,
        ),
    ];
    for (kind, mutation, expected) in cases {
        assert_eq!(
            expected,
            ServerRowCapabilities::for_pane(kind, mutation),
            "{kind} under {mutation:?}"
        );
    }
    assert_eq!(
        ServerRowCapabilities::EDITABLE,
        ServerRowCapabilities::for_local(QueueMutation::PerRowKind)
    );
    assert_eq!(
        ServerRowCapabilities::PROTECTED,
        ServerRowCapabilities::for_local(QueueMutation::ReadOnly)
    );
}

/// The agent's own `editable` answer decides a row on the session's own queue,
/// the kind rule only covers an agent too old to send one, and a mirror stays
/// read-only whatever the agent says.
#[test]
fn a_row_the_agent_labels_decides_its_own_mutability() {
    let row = |kind: &str, editable: Option<bool>| crate::app::prompt_queue::QueueEntryWire {
        kind: kind.to_owned(),
        editable,
        ..serde_json::from_value(serde_json::json!({
            "id": "p1", "text": "t", "kind": kind, "version": 0
        }))
        .expect("minimal wire row")
    };
    let cases = [
        (row("prompt", Some(false)), QueueMutation::PerRowKind, false),
        (
            row("parent_agent_message", Some(true)),
            QueueMutation::PerRowKind,
            true,
        ),
        (
            row("parent_agent_message", None),
            QueueMutation::PerRowKind,
            false,
        ),
        (row("prompt", None), QueueMutation::PerRowKind, true),
        (row("prompt", Some(true)), QueueMutation::ReadOnly, false),
    ];
    for (entry, mutation, editable) in cases {
        assert_eq!(
            editable,
            ServerRowCapabilities::for_wire(&entry, mutation).can_edit(),
            "{} editable={:?} under {mutation:?}",
            entry.kind,
            entry.editable
        );
    }
}
