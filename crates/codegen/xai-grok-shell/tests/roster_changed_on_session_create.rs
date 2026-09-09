//! A session that has just been created announces itself on `x.ai/sessions/changed`.
//!
//! The upsert was pushed from `spawn_session_on_thread` *before* `insert_resident`, and
//! `resident_roster_entry` is built from the hosted handle — so it read `None` and `emit_roster_changed` returned
//! having sent nothing. Every dashboard but the one that asked for the session learned about it only from the next
//! `x.ai/sessions/list` poll, or from the session's first turn-boundary activity delta: a session opened in another
//! client did not appear at all, and a just-opened dormant row kept saying `dormant`.
//!
//! This drives a real `MvpAgent` over ACP, so the assertion is on what a client actually receives rather than on the
//! helper in isolation.

#[allow(dead_code)]
mod acp_harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use acp_harness::{allow_once, connect_and_auth, new_session, run_agent_test};
use agent_client_protocol as acp;

/// Records the ext notifications the agent pushes, which is where roster deltas arrive.
#[derive(Clone, Default)]
struct RecordingClient {
    ext: Rc<RefCell<Vec<(String, String)>>>,
}

#[async_trait::async_trait(?Send)]
impl acp::Client for RecordingClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Ok(acp::RequestPermissionResponse::new(allow_once(&args)))
    }

    async fn session_notification(&self, _args: acp::SessionNotification) -> acp::Result<()> {
        Ok(())
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> acp::Result<()> {
        self.ext
            .borrow_mut()
            .push((args.method.to_string(), args.params.get().to_string()));
        Ok(())
    }
}

/// Roster deltas are fire-and-forget through the gateway, so they land after `session/new` has answered.
async fn wait_for_roster_upsert(client: &RecordingClient, session_id: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(params) = client
            .ext
            .borrow()
            .iter()
            .find(|(method, params)| {
                method.trim_start_matches('_') == "x.ai/sessions/changed"
                    && params.contains(session_id)
            })
            .map(|(_, params)| params.clone())
        {
            return params;
        }
        assert!(
            Instant::now() < deadline,
            "no x.ai/sessions/changed upsert named {session_id}; saw {:?}",
            client
                .ext
                .borrow()
                .iter()
                .map(|(m, _)| m.clone())
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[test]
fn creating_a_session_announces_it_to_every_client() {
    run_agent_test(|cwd, _mock| async move {
        let client = RecordingClient::default();
        let (conn, _init) = connect_and_auth(client.clone(), "roster-broadcast-pin").await;
        let session = new_session(&conn, &cwd).await;

        let params = wait_for_roster_upsert(&client, session.0.as_ref()).await;
        let changed: serde_json::Value = serde_json::from_str(&params).expect("valid JSON params");
        let upserted = changed["upserted"]
            .as_array()
            .expect("a create is an upsert, not a removal");
        let entry = upserted
            .iter()
            .find(|e| e["sessionId"] == serde_json::json!(session.0.as_ref()))
            .expect("the new session is the one named");
        assert_eq!(
            entry["resident"],
            serde_json::json!(true),
            "the delta must carry the hosted entry, which is what reading it after `insert_resident` buys"
        );
        assert!(
            changed["removed"]
                .as_array()
                .is_none_or(|removed| removed.is_empty()),
            "a create removes nothing"
        );
    });
}
