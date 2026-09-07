//! Compaction is served, not refused, on a Gemini provider.
//!
//! `generate_session_compact` dispatched the backend itself and answered a Gemini one with
//! "compact failed: Gemini backend is not yet supported". The plumbing it needed
//! (`conversation_stream_gemini` and the request builder behind it) has existed since; the
//! refusal outlived its reason. These tests hold the branch open against a Gemini endpoint.
//!
//! They also pin what the translation *cannot* carry, because that is the whole content of the
//! old refusal: `GeminiRequest` has no `toolConfig` and no slot for xAI's hosted tools, so the
//! compaction tool choice and the hosted-tool specs are dropped on the way to the wire. Both are
//! dropped on at least one already-served backend too, which is why they do not justify a refusal
//! — but a future reader who widens the translation should see these assertions fail.

use super::*;
use crate::sampling::{Client, ConversationItem, SamplerConfig, ToolCall};
use xai_grok_test_support::MockInferenceServer;

fn gemini_config(base_url: &str) -> SamplerConfig {
    SamplerConfig {
        api_key: Some("test-api-key".to_string()),
        base_url: base_url.to_string(),
        model: "test-model".to_string(),
        max_completion_tokens: Some(1000),
        temperature: Some(0.7),
        api_backend: ApiBackend::Gemini,
        context_window: 256_000,
        ..Default::default()
    }
}

/// History with a tool call and its result, so the turn shapes Gemini translates specially
/// (`functionCall` / `functionResponse`) ride along rather than a two-item side-call prompt.
fn compaction_history() -> Vec<ConversationItem> {
    vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("<user_query>\nfix the retry budget\n</user_query>"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call-1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"path":"retry.rs"}"#.into(),
        }]),
        ConversationItem::tool_result("call-1", "const MAX_RETRIES: u32 = 3;"),
        ConversationItem::assistant("Capped at three attempts."),
        ConversationItem::user("Summarize the conversation so far."),
    ]
}

fn compaction_tools() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "read_file".to_string(),
        description: Some("Reads a file".to_string()),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }]
}

/// The one request the compaction call made, or a failure naming what it hit instead.
fn gemini_request_body(server: &MockInferenceServer) -> serde_json::Value {
    let requests = server.requests();
    let logged = requests
        .iter()
        .rev()
        .find(|request| request.path.contains(":streamGenerateContent"))
        .expect("compaction must reach the Gemini endpoint, not be refused before it");
    logged
        .body
        .clone()
        .expect("the Gemini request body is JSON")
}

#[tokio::test]
async fn gemini_compaction_streams_a_summary() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("<summary>capped at three attempts</summary>");

    let config = gemini_config(&server.url());
    let client = Client::new(config.clone()).unwrap();

    let output = generate_session_compact(
        compaction_history(),
        0,
        compaction_tools(),
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_secs(30),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("a Gemini provider must serve compaction, not refuse it: {e:?}"));

    assert_eq!(
        output.content,
        "<summary>capped at three attempts</summary>"
    );
    // The raw provider string, not a normalized one: `CompactOutput` documents it as such.
    assert_eq!(output.stop_reason.as_deref(), Some("STOP"));
    assert!(!output.truncated);
    assert!(output.delta_count > 0, "text parts must count as deltas");
}

#[tokio::test]
async fn gemini_compaction_request_carries_the_history_and_the_tools() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("<summary>ok</summary>");

    let config = gemini_config(&server.url());
    let client = Client::new(config.clone()).unwrap();

    generate_session_compact(
        compaction_history(),
        0,
        compaction_tools(),
        // A hosted tool the caller attaches for prefix-cache alignment on the backends that carry them.
        vec![HostedTool::WebSearch { options: None }],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_secs(30),
        0,
        crate::util::config::CompactionToolChoice::None,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("a Gemini provider must serve compaction, not refuse it: {e:?}"));

    let body = gemini_request_body(&server);

    assert!(
        body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "the system prompt must survive translation: {body:#}"
    );
    let contents = body["contents"].as_array().expect("contents is an array");
    assert!(
        contents
            .iter()
            .any(|turn| turn["parts"].as_array().is_some_and(|parts| parts
                .iter()
                .any(|p| p["functionCall"]["name"] == "read_file"))),
        "the tool call must survive translation: {body:#}"
    );
    assert!(
        contents
            .iter()
            .any(|turn| turn["parts"].as_array().is_some_and(|parts| parts
                .iter()
                .any(|p| p["functionResponse"]["name"] == "read_file"))),
        "the tool result must survive translation: {body:#}"
    );
    assert!(
        contents.last().is_some_and(|turn| turn["parts"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Summarize the conversation so far."))),
        "the summarization prompt must be the final turn: {body:#}"
    );
    assert_eq!(
        body["generationConfig"]["temperature"].as_f64(),
        Some(1.0),
        "{body:#}"
    );
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"], "read_file",
        "the tool definitions ride along to keep the cached prefix aligned: {body:#}"
    );

    // The two drops the refusal used to stand in for. Neither is expressible in `GeminiRequest`.
    assert!(
        body.get("toolConfig").is_none(),
        "a tool choice Gemini cannot carry must not appear to be honored: {body:#}"
    );
    assert!(
        !body.to_string().contains("web_search"),
        "hosted tools have no Gemini representation and are dropped: {body:#}"
    );
}
