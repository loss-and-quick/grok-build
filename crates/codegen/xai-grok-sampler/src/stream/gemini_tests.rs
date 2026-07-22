//! Unit tests for the Gemini L2 stream transform.

use super::*;
use futures_util::StreamExt;
use futures_util::stream;
use std::pin::pin;
use xai_grok_sampling_types::gemini::{
    GeminiCandidate, GeminiContent, GeminiFunctionCall, GeminiPart, GeminiStreamChunk,
    GeminiUsageMetadata,
};

fn rid() -> RequestId {
    RequestId::from("gemini-test")
}

fn text_chunk(text: &str) -> GeminiStreamChunk {
    GeminiStreamChunk {
        candidates: vec![GeminiCandidate {
            content: Some(GeminiContent {
                role: Some("model".to_owned()),
                parts: vec![GeminiPart {
                    text: Some(text.to_owned()),
                    ..Default::default()
                }],
            }),
            finish_reason: None,
        }],
        ..Default::default()
    }
}

fn terminal_chunk(finish: &str, prompt: u32, candidates: u32) -> GeminiStreamChunk {
    GeminiStreamChunk {
        candidates: vec![GeminiCandidate {
            content: None,
            finish_reason: Some(finish.to_owned()),
        }],
        usage_metadata: Some(GeminiUsageMetadata {
            prompt_token_count: prompt,
            candidates_token_count: candidates,
            total_token_count: prompt + candidates,
            cached_content_token_count: 0,
        }),
        model_version: Some("gemini-x".to_owned()),
    }
}

async fn collect(s: impl Stream<Item = SamplingEvent>) -> Vec<SamplingEvent> {
    let mut out = Vec::new();
    let mut s = pin!(s);
    while let Some(ev) = s.next().await {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn empty_stream_yields_started_then_completed() {
    let raw = stream::iter(Vec::<Result<GeminiStreamChunk, SamplingError>>::new()).boxed();
    let events = collect(stream_gemini(raw, None, rid(), Duration::from_secs(60))).await;
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], SamplingEvent::StreamStarted { .. }));
    assert!(matches!(events[1], SamplingEvent::Completed { .. }));
}

#[tokio::test]
async fn text_parts_assemble_with_usage() {
    let chunks: Vec<Result<GeminiStreamChunk, SamplingError>> = vec![
        Ok(text_chunk("Hello, ")),
        Ok(text_chunk("world!")),
        Ok(terminal_chunk("STOP", 12, 4)),
    ];
    let raw = stream::iter(chunks).boxed();
    let evs = collect(stream_gemini(raw, None, rid(), Duration::from_secs(60))).await;

    let text: String = evs
        .iter()
        .filter_map(|e| match e {
            SamplingEvent::ChannelToken {
                channel: SamplingChannel::Text,
                text,
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello, world!");

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let a = response.assistant().expect("assistant item present");
            assert_eq!(a.content.as_ref(), "Hello, world!");
            assert_eq!(a.model_id.as_deref(), Some("gemini-x"));
            assert_eq!(response.stop_reason, Some(StopReason::Stop));
            let u = response.usage.as_ref().expect("usage extracted");
            assert_eq!(u.prompt_tokens, 12);
            assert_eq!(u.completion_tokens, 4);
            assert_eq!(u.total_tokens, 16);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn function_call_becomes_tool_call_and_tool_calls_stop_reason() {
    let call_chunk = GeminiStreamChunk {
        candidates: vec![GeminiCandidate {
            content: Some(GeminiContent {
                role: Some("model".to_owned()),
                parts: vec![GeminiPart {
                    function_call: Some(GeminiFunctionCall {
                        name: "get_weather".to_owned(),
                        args: Some(serde_json::json!({ "city": "Paris" })),
                        id: Some("call_42".to_owned()),
                    }),
                    ..Default::default()
                }],
            }),
            finish_reason: Some("STOP".to_owned()),
        }],
        ..Default::default()
    };
    let raw = stream::iter(vec![Ok::<_, SamplingError>(call_chunk)]).boxed();
    let evs = collect(stream_gemini(raw, None, rid(), Duration::from_secs(60))).await;

    let deltas: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            SamplingEvent::ToolCallDelta {
                name,
                arguments_delta,
                ..
            } => Some((name.clone(), arguments_delta.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].0.as_deref(), Some("get_weather"));
    assert!(deltas[0].1.as_deref().unwrap().contains("Paris"));

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let a = response.assistant().expect("assistant item");
            assert_eq!(a.tool_calls.len(), 1);
            assert_eq!(a.tool_calls[0].name, "get_weather");
            assert_eq!(a.tool_calls[0].id.as_ref(), "call_42");
            // Tool calls override the wire finishReason.
            assert_eq!(response.stop_reason, Some(StopReason::ToolCalls));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn transport_error_yields_failed() {
    let chunks: Vec<Result<GeminiStreamChunk, SamplingError>> = vec![
        Ok(text_chunk("partial")),
        Err(SamplingError::EventStreamError("boom".to_owned())),
    ];
    let raw = stream::iter(chunks).boxed();
    let evs = collect(stream_gemini(raw, None, rid(), Duration::from_secs(60))).await;
    assert!(matches!(evs.last().unwrap(), SamplingEvent::Failed { .. }));
}
