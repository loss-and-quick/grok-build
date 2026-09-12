//! Durable watermark + delivered-work record for exactly-once response-boundary delivery.
//!
//! When a response boundary commits, queued `/loop` fires and `queue=true` parent messages are
//! drained from the queue into interjections for the next request (see
//! [`crate::session::acp_session_impl::prompt_queue::SessionActorPromoterExt::drain_response_boundary_work`]).
//! If the process crashes *after* the commit but *before* the interjection lands in the
//! conversation, that work would be lost: on resume the queue is rebuilt from the conversation,
//! which does not yet contain the not-yet-injected work.
//!
//! [`PromptDeliveryState`] is the durable record. It is written **before** the in-memory
//! `response_seq` is advanced (write-ahead):
//!
//! - [`PromptDeliveryState::watermark`] is the highest committed response boundary for the
//!   running prompt, so the counter stays continuous across a crash instead of resetting to 0.
//! - [`PromptDeliveryState::pending`] is the work drained but not yet injected.
//!
//! A crash can therefore neither lose committed work (the record survives on disk) nor
//! double-deliver it (the watermark makes the in-memory counter authoritative on resume).
//! [`reconcile_delivered_work`] is a pure predicate that, given the persisted entries and a
//! "is this work already in the conversation" test, splits them into delivered (present in the
//! conversation, so skip) and pending (must be re-injected).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sampling::{ConversationItem, ContentPart};

/// Side-file name in the session dir holding the [`PromptDeliveryState`].
pub(crate) const PROMPT_DELIVERY_FILE: &str = "prompt_delivery_state.json";

/// Highest committed response boundary for a prompt: `(prompt_id, response_seq)`.
///
/// Persisted so a resumed prompt continues its per-response orchestration boundary instead of
/// restarting at `0`, which would re-bind already-delivered work to a fresh boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PromptDeliveryWatermark {
    pub prompt_id: String,
    pub response_seq: u64,
}

/// One unit of work drained at a response boundary but not yet injected into the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PromptDeliveryEntry {
    /// The running prompt this work is bound to (the prompt whose boundary it commits against).
    pub prompt_id: String,
    /// The response boundary this work was committed at.
    pub response_seq: u64,
    /// Unique id of the drained `/loop` fire or queued parent message.
    pub work_prompt_id: String,
    /// Serialised tag of the drained work's [`crate::session::PromptOrigin`].
    pub origin: String,
    /// Rendered prompt text, re-injected verbatim as an interjection on resume.
    pub text: String,
}

/// The durable record: the watermark plus the not-yet-delivered work entries.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PromptDeliveryState {
    pub watermark: Option<PromptDeliveryWatermark>,
    /// Work drained at committed boundaries but not yet injected into the conversation.
    /// Small and short-lived: entries are removed once their work is delivered.
    pub pending: Vec<PromptDeliveryEntry>,
}

impl PromptDeliveryState {
    /// Write-ahead record of a committed boundary: bump the watermark to at least `response_seq`
    /// for `prompt_id`, then append the drained `entries`. Call **before** advancing the
    /// in-memory `response_seq` so a crash cannot advance the counter without the record.
    pub(crate) fn commit(
        &mut self,
        prompt_id: &str,
        response_seq: u64,
        entries: Vec<PromptDeliveryEntry>,
    ) {
        let existing = self.watermark.clone();
        let bump = !matches!(
            existing,
            Some(w) if w.prompt_id == prompt_id && w.response_seq >= response_seq
        );
        if bump {
            self.watermark = Some(PromptDeliveryWatermark {
                prompt_id: prompt_id.to_string(),
                response_seq,
            });
        }
        self.pending.extend(entries);
    }

    /// Remove the entries whose work was already delivered (present in the conversation).
    /// Returns the entries that were delivered so the caller can drop them from the record.
    pub(crate) fn retain_pending(&mut self, is_delivered: impl Fn(&PromptDeliveryEntry) -> bool) {
        self.pending.retain(|e| !is_delivered(e));
    }

    /// Split pending into `(delivered, pending)` using `is_delivered`. Pure; side-effect free.
    pub(crate) fn reconcile(
        &self,
        is_delivered: impl Fn(&PromptDeliveryEntry) -> bool,
    ) -> (Vec<PromptDeliveryEntry>, Vec<PromptDeliveryEntry>) {
        let mut delivered = Vec::new();
        let mut pending = Vec::new();
        for entry in &self.pending {
            if is_delivered(entry) {
                delivered.push(entry.clone());
            } else {
                pending.push(entry.clone());
            }
        }
        (delivered, pending)
    }
}

/// Whether the conversation already contains `needle` (used on resume to skip re-injecting work
/// that was delivered before a crash). Interjections land as user messages, so only user content
/// is scanned; a substring match tolerates the whitespace the render layer may normalize.
pub(crate) fn conversation_contains_text(
    conversation: &[ConversationItem],
    needle: &str,
) -> bool {
    if needle.is_empty() {
        return false;
    }
    conversation.iter().any(|item| match item {
        ConversationItem::User(user) => user
            .content
            .iter()
            .any(|part| match part {
                ContentPart::Text { text } => text.as_ref().contains(needle),
                _ => false,
            }),
        _ => false,
    })
}

/// Absolute path of the [`PromptDeliveryState`] side-file for a session dir.
pub(crate) fn prompt_delivery_state_path(session_dir: &Path) -> PathBuf {
    session_dir.join(PROMPT_DELIVERY_FILE)
}

/// Load the durable record. Best-effort: a missing or unreadable file yields the empty state,
/// which is the safe default (no committed work to recover).
pub(crate) fn load_prompt_delivery_state(session_dir: &Path) -> PromptDeliveryState {
    let path = prompt_delivery_state_path(session_dir);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the durable record. Best-effort; a write failure is logged, never fatal.
pub(crate) fn save_prompt_delivery_state(session_dir: &Path, state: &PromptDeliveryState) {
    if !session_dir.is_dir() {
        return;
    }
    let path = prompt_delivery_state_path(session_dir);
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!(?e, path = %path.display(), "failed to persist prompt delivery state");
            }
        }
        Err(e) => {
            tracing::warn!(?e, "failed to serialize prompt delivery state");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(prompt_id: &str, seq: u64, work: &str, text: &str) -> PromptDeliveryEntry {
        PromptDeliveryEntry {
            prompt_id: prompt_id.to_string(),
            response_seq: seq,
            work_prompt_id: work.to_string(),
            origin: "scheduler_fire".to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn commit_bumps_watermark_and_appends_pending() {
        let mut state = PromptDeliveryState::default();
        assert_eq!(state.watermark, None);
        state.commit("P", 1, vec![entry("P", 1, "L1", "loop one")]);
        assert_eq!(
            state.watermark,
            Some(PromptDeliveryWatermark {
                prompt_id: "P".to_string(),
                response_seq: 1
            })
        );
        assert_eq!(state.pending.len(), 1);

        // A later boundary for the same prompt bumps only forward.
        state.commit("P", 3, vec![entry("P", 3, "L2", "loop two")]);
        assert_eq!(state.watermark.unwrap().response_seq, 3);
        assert_eq!(state.pending.len(), 2);
    }

    #[test]
    fn commit_does_not_lower_watermark() {
        let mut state = PromptDeliveryState::default();
        state.commit("P", 5, vec![entry("P", 5, "L1", "x")]);
        // Committing an older seq for the same prompt must not rewind the watermark.
        state.commit("P", 2, vec![entry("P", 2, "L2", "y")]);
        assert_eq!(state.watermark.unwrap().response_seq, 5);
    }

    #[test]
    fn commit_for_new_prompt_replaces_watermark_prompt_id() {
        let mut state = PromptDeliveryState::default();
        state.commit("P", 2, vec![entry("P", 2, "L1", "x")]);
        // A different running prompt starts its own boundary.
        state.commit("Q", 1, vec![entry("Q", 1, "M1", "y")]);
        let wm = state.watermark.unwrap();
        assert_eq!(wm.prompt_id, "Q");
        assert_eq!(wm.response_seq, 1);
    }

    #[test]
    fn retain_removes_only_delivered_entries() {
        let mut state = PromptDeliveryState::default();
        state.commit(
            "P",
            1,
            vec![entry("P", 1, "L1", "loop one"), entry("P", 1, "M1", "parent")],
        );
        // "loop one" was injected (delivered); "parent" was not yet injected (pending).
        state.retain_pending(|e| e.text == "loop one");
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].work_prompt_id, "M1");
    }

    #[test]
    fn reconcile_splits_delivered_and_pending() {
        let state = PromptDeliveryState {
            watermark: Some(PromptDeliveryWatermark {
                prompt_id: "P".to_string(),
                response_seq: 2,
            }),
            pending: vec![
                entry("P", 2, "L1", "delivered"),
                entry("P", 2, "M1", "pending"),
            ],
        };
        let (delivered, pending) = state.reconcile(|e| e.text == "delivered");
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].work_prompt_id, "L1");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].work_prompt_id, "M1");
        // reconcile is pure: the source state is untouched.
        assert_eq!(state.pending.len(), 2);
    }

    #[test]
    fn round_trip_survives_persist_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = PromptDeliveryState::default();
        state.commit(
            "P",
            4,
            vec![entry("P", 4, "L1", "loop one"), entry("P", 4, "M1", "parent")],
        );
        save_prompt_delivery_state(dir.path(), &state);

        let loaded = load_prompt_delivery_state(dir.path());
        assert_eq!(loaded, state);
        assert_eq!(loaded.watermark.unwrap().response_seq, 4);
        assert_eq!(loaded.pending.len(), 2);
    }

    #[test]
    fn load_missing_file_is_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_prompt_delivery_state(dir.path());
        assert_eq!(loaded, PromptDeliveryState::default());
    }

    #[test]
    fn load_corrupt_file_is_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            prompt_delivery_state_path(dir.path()),
            "{ not valid json",
        )
        .unwrap();
        let loaded = load_prompt_delivery_state(dir.path());
        assert_eq!(loaded, PromptDeliveryState::default());
    }

    #[test]
    fn save_is_noop_without_session_dir() {
        // Writing outside an existing dir must not panic and must be a harmless best-effort.
        save_prompt_delivery_state(Path::new("/nonexistent/path/here"), &PromptDeliveryState::default());
    }

    #[test]
    fn conversation_contains_text_matches_user_only() {
        let with_text = vec![ConversationItem::user("hello boundary")];
        let without = vec![ConversationItem::system("hello boundary")];
        assert!(conversation_contains_text(&with_text, "boundary"));
        assert!(!conversation_contains_text(&without, "boundary"));
        // Empty needle never matches (a drained item with no text is not "delivered" by text).
        assert!(!conversation_contains_text(&with_text, ""));
    }
}
