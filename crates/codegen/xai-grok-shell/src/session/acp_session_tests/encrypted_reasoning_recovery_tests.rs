//! Recovery from a provider that refuses to verify replayed encrypted
//! reasoning content.

use super::support::*;
use super::*;
use std::sync::Arc;
use xai_grok_sampling_types::rs;

/// The 400 a provider answers when the history replays reasoning minted
/// somewhere else. The wording is the provider's own prose — the item id is a
/// placeholder, not a real one.
fn encrypted_content_error() -> xai_grok_sampler::SamplingErrorInfo {
    xai_grok_sampler::SamplingErrorInfo {
        kind: xai_grok_sampler::SamplingErrorKind::Api,
        message: "API error (status 400): invalid_request_error: The encrypted content for item \
                  rs_00placeholder could not be verified. Reason: Encrypted content could not be \
                  decrypted or parsed."
            .to_string(),
        status_code: Some(400),
        is_retryable: false,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
        model_metadata: None,
        empty_response_context: None,
        doom_loop_triggers: None,
        doom_loop_aborted_at_chunk: None,
        credential: xai_grok_sampling_types::SentCredential::Unknown,
    }
}

fn reasoning(id: &str, encrypted: Option<&str>) -> ConversationItem {
    ConversationItem::Reasoning(rs::ReasoningItem {
        id: id.to_string(),
        summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
            text: "a thought".to_string(),
        })],
        content: None,
        encrypted_content: encrypted.map(str::to_owned),
        status: None,
    })
}

async fn actor_with_conversation(items: Vec<ConversationItem>) -> Arc<SessionActor> {
    let (gateway_tx, _grx) =
        tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
    let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    actor.chat_state_handle.replace_conversation(items);
    Arc::new(actor)
}

/// The turn is recoverable: the rejected reasoning leaves the history and the
/// loop resubmits. It used to end here with "start a new session", throwing a
/// whole conversation away over one unverifiable block — and only when the
/// detector matched, which for this wording it did not.
#[tokio::test(flavor = "current_thread")]
async fn encrypted_content_rejection_strips_the_reasoning_and_resubmits() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = actor_with_conversation(vec![
                ConversationItem::user("explain the borrow checker"),
                reasoning("rs_00placeholder", Some("blob-from-another-issuer")),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
                ConversationItem::user("and lifetimes?"),
            ])
            .await;

            let recovery = actor
                .handle_sampling_failure(encrypted_content_error(), 0)
                .await;

            assert!(
                matches!(
                    recovery,
                    Ok(SamplerFailureRecovery::ResubmitWithoutReasoning)
                ),
                "the turn must be resubmitted, not failed"
            );
            let items = actor.chat_state_handle.get_conversation().await;
            assert!(
                !items
                    .iter()
                    .any(|i| matches!(i, ConversationItem::Reasoning(_))),
                "the unverifiable reasoning must be gone: {items:?}"
            );
            assert_eq!(
                items.len(),
                3,
                "user turns and assistant answers are untouched: {items:?}"
            );
        })
        .await;
}

/// The recovery only fires while it has something to remove. With no
/// encrypted reasoning left, a resubmit would send the same bytes and earn
/// the same 400, so the turn ends with the terminal message instead.
#[tokio::test(flavor = "current_thread")]
async fn encrypted_content_rejection_is_terminal_with_nothing_left_to_drop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = actor_with_conversation(vec![
                ConversationItem::user("explain the borrow checker"),
                reasoning("rs_00local", None),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ])
            .await;

            let recovery = actor
                .handle_sampling_failure(encrypted_content_error(), 0)
                .await;

            assert!(
                recovery.is_err(),
                "with no blob to drop the turn must not loop"
            );
            let items = actor.chat_state_handle.get_conversation().await;
            assert_eq!(items.len(), 3, "an unrepairable turn changes nothing");
        })
        .await;
}
