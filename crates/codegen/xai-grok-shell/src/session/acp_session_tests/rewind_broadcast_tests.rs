//! A rewind is a change to shared, persisted state, so the clients that did not ask for it are told.
//!
//! The marker was written to `updates.jsonl` through `persist_xai_update_only`, which by its own contract does not
//! reach the gateway. Every other attached client therefore went on rendering the turns the rewind discarded until it
//! reloaded the session, and its replay cursor pointed into a branch the log no longer had.

use super::support::create_test_actor;

use crate::extensions::notification::SessionUpdate as XaiSessionUpdate;
use crate::sampling::ConversationItem;
use crate::session::persistence::PersistenceMsg;
use crate::session::{RewindMode, RewindRequest};
use agent_client_protocol as acp;

/// The first `x.ai/session_notification` on the gateway carrying a `rewind_marker`, with its target index.
fn drain_rewind_marker(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<xai_acp_lib::AcpClientMessage>,
) -> Option<(String, usize)> {
    let mut found = None;
    while let Ok(msg) = rx.try_recv() {
        if let xai_acp_lib::AcpClientMessage::ExtNotification(args) = msg {
            let method = args.request.method.to_string();
            let params: serde_json::Value =
                serde_json::from_str(args.request.params.get()).unwrap_or(serde_json::Value::Null);
            if found.is_none() && params["update"]["sessionUpdate"] == "rewind_marker" {
                let target = params["update"]["target_prompt_index"]
                    .as_u64()
                    .expect("a marker names the prompt it cuts to")
                    as usize;
                found = Some((method, target));
            }
            let _ = args.response_tx.send(Ok(()));
        }
    }
    found
}

/// The marker still lands in `updates.jsonl`: replay reads it to rebuild a branched timeline, so sending it live must
/// be an addition rather than a move.
fn persisted_rewind_target(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>,
) -> Option<usize> {
    while let Ok(msg) = rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::Xai(n)) = msg
            && let XaiSessionUpdate::RewindMarker {
                target_prompt_index,
                ..
            } = n.update
        {
            return Some(target_prompt_index);
        }
    }
    None
}

#[tokio::test(flavor = "current_thread")]
async fn a_rewind_is_announced_to_the_session_and_still_written_down() {
    let local = tokio::task::LocalSet::new();
    local.run_until(run()).await;
}

async fn run() {
    let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    actor.session_info.id = acp::SessionId::new(format!("rw-broadcast-{unique}"));
    let session_dir = crate::session::persistence::session_dir(&actor.session_info);
    std::fs::create_dir_all(&session_dir).unwrap();

    let mut snap = actor
        .chat_state_handle
        .snapshot()
        .await
        .expect("snapshot available");
    snap.conversation = vec![
        ConversationItem::system("SYS"),
        ConversationItem::user("P0"),
        ConversationItem::assistant("R0"),
        ConversationItem::user("P1"),
        ConversationItem::assistant("R1"),
    ];
    snap.prompt_index = 2;
    snap.prompt_texts = vec!["P0".to_string(), "P1".to_string()];
    actor.chat_state_handle.restore_snapshot(snap);

    // The setup itself emits nothing worth keeping; only the rewind's own frames are under test.
    while gateway_rx.try_recv().is_ok() {}
    while persistence_rx.try_recv().is_ok() {}

    let resp = actor
        .handle_rewind(RewindRequest {
            target_prompt_index: 1,
            force: true,
            mode: RewindMode::ConversationOnly,
        })
        .await
        .expect("handle_rewind ok");
    assert!(resp.success, "rewind should succeed: {resp:?}");

    let (method, target) =
        drain_rewind_marker(&mut gateway_rx).expect("the rewind must reach the other clients");
    assert_eq!(
        method, "x.ai/session_notification",
        "the marker rides the ordinary session carrier, so it fans out to this session's subscribers"
    );
    assert_eq!(target, 1);
    assert_eq!(
        persisted_rewind_target(&mut persistence_rx),
        Some(1),
        "sending the marker must not stop it being written: replay needs it to rebuild the branch"
    );

    let _ = std::fs::remove_dir_all(&session_dir);
}
