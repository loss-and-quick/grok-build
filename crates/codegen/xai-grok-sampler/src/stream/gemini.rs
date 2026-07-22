//! Layer-2 stream transform for the Google Gemini API.
//!
//! Consumes a raw [`GeminiStreamChunk`] stream (each item one decoded
//! `streamGenerateContent` SSE event) and produces [`SamplingEvent`]s. Pure:
//! no I/O, no shell coupling. Yields exactly one terminal event
//! ([`SamplingEvent::Completed`] or [`SamplingEvent::Failed`]) per request.

use std::time::{Duration, Instant};

use futures_util::stream::{BoxStream, Stream};

use xai_grok_sampling_types::gemini::GeminiStreamChunk;
use xai_grok_sampling_types::{
    AssistantItem, ConversationItem, ConversationResponse, ResponseModelMetadata, SamplingError,
    StopReason, TokenUsage, ToolCall,
};

use crate::events::{SamplingChannel, SamplingErrorInfo, SamplingEvent};
use crate::metrics::InferenceLatencyStats;
use crate::types::RequestId;

/// Map a Gemini `finishReason` to a unified [`StopReason`].
fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
            StopReason::ContentFilter
        }
        other => {
            tracing::warn!(
                finish_reason = %other,
                "unrecognized Gemini finishReason; treating as stop"
            );
            StopReason::Stop
        }
    }
}

/// Transform a raw Gemini `streamGenerateContent` stream into
/// [`SamplingEvent`]s.
pub fn stream_gemini<'a>(
    raw_stream: BoxStream<'a, Result<GeminiStreamChunk, SamplingError>>,
    model_metadata: Option<ResponseModelMetadata>,
    request_id: RequestId,
    idle_timeout: Duration,
) -> impl Stream<Item = SamplingEvent> + Send + 'a {
    async_stream::stream! {
        let stream_start = Instant::now();
        let mut chunk_timestamps: Vec<Instant> = Vec::new();

        yield SamplingEvent::StreamStarted {
            request_id: request_id.clone(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        if let Some(metadata) = model_metadata {
            yield SamplingEvent::ModelMetadata {
                request_id: request_id.clone(),
                metadata,
            };
        }

        let mut assistant_text = String::new();
        let mut assistant_tool_calls: Vec<ToolCall> = Vec::new();
        let mut final_stop_reason: Option<StopReason> = None;
        let mut final_model: Option<String> = None;
        let mut prompt_tokens: u32 = 0;
        let mut completion_tokens: u32 = 0;
        let mut total_tokens: u32 = 0;
        let mut cached_tokens: u32 = 0;

        let mut chunk_index: u64 = 0;
        let mut message_chunk_count: u64 = 0;
        let mut next_tool_index: u32 = 0;
        let mut first_token_emitted = false;

        let mut stream = raw_stream;
        loop {
            let chunk = match tokio::time::timeout(idle_timeout, futures_util::StreamExt::next(&mut stream)).await {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(err))) => {
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&err),
                    };
                    return;
                }
                Ok(None) => break,
                Err(_elapsed) => {
                    let err = SamplingError::IdleTimeout {
                        elapsed_secs: idle_timeout.as_secs(),
                    };
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&err),
                    };
                    return;
                }
            };

            chunk_timestamps.push(Instant::now());

            if let Some(mv) = chunk.model_version {
                final_model = Some(mv);
            }
            if let Some(usage) = chunk.usage_metadata {
                // Gemini reports cumulative counts per chunk; keep the latest.
                prompt_tokens = usage.prompt_token_count;
                completion_tokens = usage.candidates_token_count;
                total_tokens = usage.total_token_count;
                cached_tokens = usage.cached_content_token_count;
            }

            for candidate in chunk.candidates {
                if let Some(content) = candidate.content {
                    for part in content.parts {
                        if let Some(text) = part.text
                            && !text.is_empty()
                        {
                            if !first_token_emitted {
                                first_token_emitted = true;
                                yield SamplingEvent::FirstToken {
                                    request_id: request_id.clone(),
                                };
                            }
                            assistant_text.push_str(&text);
                            yield SamplingEvent::ChannelToken {
                                request_id: request_id.clone(),
                                channel: SamplingChannel::Text,
                                text,
                                chunk_index,
                            };
                            chunk_index += 1;
                            message_chunk_count += 1;
                        }
                        if let Some(fc) = part.function_call {
                            let arguments = fc
                                .args
                                .as_ref()
                                .map(|a| a.to_string())
                                .unwrap_or_else(|| "{}".to_owned());
                            let id = fc.id.clone().unwrap_or_else(|| {
                                format!("gemini-call-{next_tool_index}")
                            });
                            // Gemini sends a complete functionCall (not deltas),
                            // so emit it as a single full-argument delta.
                            yield SamplingEvent::ToolCallDelta {
                                request_id: request_id.clone(),
                                tool_index: next_tool_index,
                                id: Some(id.clone()),
                                name: Some(fc.name.clone()),
                                arguments_delta: Some(arguments.clone()),
                            };
                            assistant_tool_calls.push(ToolCall {
                                id: std::sync::Arc::from(id.as_str()),
                                name: fc.name,
                                arguments: std::sync::Arc::from(arguments.as_str()),
                            });
                            next_tool_index += 1;
                        }
                    }
                }
                if let Some(reason) = candidate.finish_reason {
                    final_stop_reason = Some(map_finish_reason(&reason));
                }
            }
        }

        if final_stop_reason == Some(StopReason::Length) && assistant_tool_calls.is_empty() {
            yield SamplingEvent::Failed {
                request_id: request_id.clone(),
                error: SamplingErrorInfo::from(&SamplingError::MaxTokensTruncation),
            };
            return;
        }

        let usage = if prompt_tokens > 0 || completion_tokens > 0 || total_tokens > 0 {
            Some(TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: if total_tokens > 0 {
                    total_tokens
                } else {
                    prompt_tokens.saturating_add(completion_tokens)
                },
                reasoning_tokens: 0,
                cached_prompt_tokens: cached_tokens,
            })
        } else {
            None
        };

        let stop_reason = if !assistant_tool_calls.is_empty() {
            Some(StopReason::ToolCalls)
        } else {
            final_stop_reason
        };

        let assistant_item = ConversationItem::Assistant(AssistantItem {
            content: std::sync::Arc::<str>::from(assistant_text),
            tool_calls: assistant_tool_calls,
            model_id: final_model,
            model_fingerprint: None,
            reasoning_effort: None,
        });

        let stream_end = Instant::now();
        let metrics =
            InferenceLatencyStats::from_timestamps(stream_start, &chunk_timestamps, stream_end);

        let response = ConversationResponse {
            items: vec![assistant_item],
            stop_reason,
            usage,
            cost_usd_ticks: None,
            message_chunks_emitted: message_chunk_count,
            doom_loop_signals: Vec::new(),
            stop_message: None,
        };

        yield SamplingEvent::Completed {
            request_id: request_id.clone(),
            response: Box::new(response),
            metrics,
        };
    }
}

#[cfg(test)]
#[path = "gemini_tests.rs"]
mod tests;
