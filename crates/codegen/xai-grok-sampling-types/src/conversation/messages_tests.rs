//! Tests for the Messages API conversion.

use super::test_support::*;
use super::*;

fn messages_test_request(reasoning_effort: Option<crate::ReasoningEffort>) -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::user("Hello")],
        model: Some("test-model".to_string()),
        reasoning_effort,
        ..Default::default()
    }
}

#[test]
fn json_schema_and_reasoning_effort_are_orthogonal_in_output_config() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "x": { "type": "string" } },
        "required": ["x"]
    });
    let mut req = ConversationRequest::from_items(vec![ConversationItem::user("go")])
        .with_json_schema(schema);
    req.reasoning_effort = Some(crate::ReasoningEffort::High);

    let msgs = build_messages_request(&req, None);
    let oc = msgs.output_config.expect("output_config present");
    assert_eq!(oc.effort.as_deref(), Some("high"));
    assert!(oc.format.is_some());
    assert!(
        msgs.thinking.is_some(),
        "thinking set when effort is present"
    );
}

#[test]
fn test_messages_request_wire_format_for_supported_variants() {
    for (variant, expected) in [
        (crate::ReasoningEffort::Low, "low"),
        (crate::ReasoningEffort::Medium, "medium"),
        (crate::ReasoningEffort::High, "high"),
        (crate::ReasoningEffort::Xhigh, "xhigh"),
        (crate::ReasoningEffort::Max, "max"),
    ] {
        let req = messages_test_request(Some(variant));
        let msgs = build_messages_request(&req, None);
        let json = serde_json::to_value(&msgs).unwrap();
        assert_eq!(
            json.pointer("/output_config/effort")
                .and_then(|v| v.as_str()),
            Some(expected),
            "{variant:?} should map to output_config.effort={expected:?}; got: {json:#}",
        );
        assert_eq!(
            json.pointer("/thinking/type").and_then(|v| v.as_str()),
            Some("adaptive"),
            "{variant:?} should auto-pair thinking.type=adaptive; got: {json:#}",
        );
    }
}

#[test]
fn test_messages_request_omits_output_config_when_no_supported_effort() {
    let none_or_unsupported = [
        None,
        Some(crate::ReasoningEffort::None),
        Some(crate::ReasoningEffort::Minimal),
    ];
    for input in none_or_unsupported {
        let req = messages_test_request(input);
        let msgs = build_messages_request(&req, None);
        assert!(
            msgs.output_config.is_none(),
            "input {input:?} must not produce output_config",
        );
        assert!(
            msgs.thinking.is_none(),
            "input {input:?} must not auto-pair thinking",
        );
    }
}

#[test]
fn test_messages_request_thinking_carries_summarized_display() {
    let req = ConversationRequest {
        reasoning_effort: Some(crate::ReasoningEffort::High),
        ..ConversationRequest::from_items(vec![ConversationItem::user("hi")])
            .with_model("messages-compatible-model")
    };
    let msg = build_messages_request(&req, None);
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        json.pointer("/thinking/type").and_then(|v| v.as_str()),
        Some("adaptive"),
        "thinking.type should be 'adaptive'; got: {json:#}",
    );
    assert_eq!(
        json.pointer("/thinking/display").and_then(|v| v.as_str()),
        Some("summarized"),
        "thinking.display must be 'summarized' so 4.7+ surfaces thinking content; got: {json:#}",
    );
}

#[test]
fn test_messages_request_omits_thinking_when_effort_unset() {
    let req = ConversationRequest::from_items(vec![ConversationItem::user("hi")])
        .with_model("messages-compatible-model");
    let msg = build_messages_request(&req, None);
    let json = serde_json::to_value(&msg).unwrap();
    assert!(
        json.get("thinking").is_none()
            || json
                .pointer("/thinking")
                .map(|v| v.is_null())
                .unwrap_or(false),
        "thinking must be absent when reasoning_effort is unset; got: {json:#}",
    );
    assert!(
        json.get("output_config").is_none()
            || json
                .pointer("/output_config")
                .map(|v| v.is_null())
                .unwrap_or(false),
        "output_config must be absent when reasoning_effort is unset; got: {json:#}",
    );
}

#[test]
fn test_messages_request_previous_tip_skips_a_trailing_user_run() {
    let mut items = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Fix the bug"),
    ];
    items.extend(agent_turn(0));
    items.extend(agent_turn(1));
    // The shape after a parallel batch: tool results, then followups.
    items.push(ConversationItem::user("[Image content]"));
    items.push(ConversationItem::user("<system-reminder>"));

    let json = serde_json::to_value(build_messages_request(
        &ConversationRequest::from_items(items).with_model("messages-compatible-model"),
        None,
    ))
    .unwrap();
    let messages = json["messages"].as_array().unwrap();

    let marked: Vec<usize> = (0..messages.len())
        .filter(|&i| marker_on_last_block(&messages[i]).is_some())
        .collect();
    let last_assistant = messages
        .iter()
        .rposition(|m| m["role"] == "assistant")
        .unwrap();
    assert_eq!(marked.len(), 2, "tip and previous tip only: {json:#}");
    assert_eq!(marked[1], messages.len() - 1, "tip: {json:#}");
    assert!(
        marked[0] < last_assistant,
        "the previous tip must sit before the last assistant turn, not inside \
             the trailing user run; got {marked:?} in {json:#}",
    );
}

#[test]
fn test_messages_request_cache_breakpoint_marks_an_image_tip() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::User(UserItem {
            content: vec![
                ContentPart::Text {
                    text: "what is in this screenshot".into(),
                },
                ContentPart::Image {
                    url: "data:image/png;base64,iVBOR".into(),
                },
            ],
            ..Default::default()
        }),
    ])
    .with_model("messages-compatible-model");

    let json = serde_json::to_value(build_messages_request(&req, None)).unwrap();
    let blocks = json["messages"][0]["content"].as_array().unwrap();

    assert_eq!(blocks.last().unwrap()["type"].as_str(), Some("image"));
    assert_eq!(
        marker_on_last_block(&json["messages"][0]),
        Some("ephemeral"),
        "{json:#}",
    );
    assert!(blocks[0].get("cache_control").is_none(), "{json:#}");
}

#[test]
fn test_messages_request_cache_breakpoint_skips_thinking() {
    // Signed by this backend (empty `id`, real `encrypted_content`): the one
    // shape that still emits a thinking block, since an unsigned item is
    // dropped rather than sent with `signature: ""`.
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Fix the bug"),
        reasoning_sibling("", "weighing options", Some("EqoBCkYIBRgCKkA")),
        ConversationItem::assistant("Fixed it."),
    ])
    .with_model("messages-compatible-model");

    let json = serde_json::to_value(build_messages_request(&req, None)).unwrap();
    let blocks = json["messages"][1]["content"].as_array().unwrap();

    let thinking = blocks
        .iter()
        .find(|b| b["type"] == "thinking")
        .expect("reasoning should emit a thinking block");
    assert!(thinking.get("cache_control").is_none(), "{json:#}");
    assert_eq!(
        marker_on_last_block(&json["messages"][1]),
        Some("ephemeral"),
        "{json:#}",
    );
}

#[test]
fn test_btw_stripped_reasoning_produces_no_thinking_blocks() {
    // Simulate a conversation where the model responded with thinking.
    let with_reasoning = ConversationItem::Assistant(AssistantItem {
        content: "Here is the answer.".into(),
        tool_calls: vec![],
        model_id: Some("messages-compatible-model".into()),
        model_fingerprint: None,
        reasoning_effort: None,
    });

    // Reasoning now lives as a sibling `ConversationItem::Reasoning`,
    // so "stripping reasoning" means filtering those siblings out — see
    // `strip_reasoning_blocks` in xai-chat-state. Here the assistant
    // never had a sibling Reasoning, so the strip is a no-op.
    let stripped = with_reasoning;

    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("hello"),
        stripped,
        ConversationItem::user("btw what is X?"),
    ]);

    let msg = build_messages_request(&req, None);
    let json = serde_json::to_value(&msg).unwrap();

    // No thinking blocks should appear in any message.
    let messages = json.get("messages").unwrap().as_array().unwrap();
    for (i, m) in messages.iter().enumerate() {
        if let Some(content) = m.get("content").and_then(|c| c.as_array()) {
            for block in content {
                assert_ne!(
                    block.get("type").and_then(|t| t.as_str()),
                    Some("thinking"),
                    "message[{i}] must not contain thinking blocks after stripping reasoning",
                );
            }
        }
    }

    // Top-level thinking must also be absent.
    assert!(
        json.get("thinking").is_none()
            || json
                .pointer("/thinking")
                .map(|v| v.is_null())
                .unwrap_or(false),
        "top-level thinking must be absent; got: {json:#}",
    );
}

#[test]
fn test_btw_mid_turn_truncation_removes_trailing_tool_use() {
    // Simulate a conversation that was snapshotted mid-turn: the last
    // assistant made a tool call that hasn't been answered yet.
    let mut items = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Fix the bug"),
        ConversationItem::assistant("I'll look at the code."),
        // Completed tool call pair:
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "src/main.rs"}"#.into(),
        }]),
        ConversationItem::tool_result("call_1", "fn main() {}"),
        ConversationItem::assistant("I see the issue. Let me fix it."),
        // Mid-turn: tool call with NO tool_result yet
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_2".into(),
            name: "search_replace".to_string(),
            arguments: "{}".into(),
        }]),
    ];

    // Apply the same truncation pattern as handle_side_question.
    while let Some(last) = items.last() {
        match last {
            ConversationItem::Assistant(a) if !a.tool_calls.is_empty() => {
                items.pop();
            }
            ConversationItem::ToolResult(_) => {
                items.pop();
            }
            _ => break,
        }
    }

    // Add the btw user question.
    items.push(ConversationItem::user("btw what is X?"));

    let msg = build_messages_request(&ConversationRequest::from_items(items.clone()), None);
    let json = serde_json::to_value(&msg).unwrap();
    let messages = json.get("messages").unwrap().as_array().unwrap();

    // The last message before the btw question should be a plain
    // assistant text (not a tool_use), so the request is valid.
    // Messages: user("Fix the bug"), asst("I'll look"), asst(tool_use call_1),
    //           user(tool_result call_1), asst("I see the issue"),
    //           user("btw what is X?")
    // The orphaned call_2 assistant must be gone.
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .unwrap();
    if let Some(content) = last_assistant.get("content").and_then(|c| c.as_array()) {
        for block in content {
            assert_ne!(
                block.get("type").and_then(|t| t.as_str()),
                Some("tool_use"),
                "last assistant must not have unanswered tool_use blocks",
            );
        }
    }

    // Verify the original complete pair (call_1) survived.
    // system + user + asst_text + asst(call_1) + tool_result(call_1) + asst_text + user(btw) = 7
    assert_eq!(items.len(), 7);
}

#[test]
fn test_btw_cross_api_messages_no_regressions() {
    let items = btw_prepare_items(btw_mid_turn_conversation());
    let req = ConversationRequest::from_items(items);
    let msg = build_messages_request(&req, None);
    let json = serde_json::to_value(&msg).unwrap();

    let messages = json.get("messages").unwrap().as_array().unwrap();

    // No thinking blocks anywhere.
    for (i, m) in messages.iter().enumerate() {
        if let Some(content) = m.get("content").and_then(|c| c.as_array()) {
            for block in content {
                assert_ne!(
                    block.get("type").and_then(|t| t.as_str()),
                    Some("thinking"),
                    "messages[{i}] must not contain thinking blocks",
                );
            }
        }
    }

    // Last assistant message must not have unanswered tool_use.
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .expect("should have an assistant message");
    if let Some(content) = last_assistant.get("content").and_then(|c| c.as_array()) {
        for block in content {
            assert_ne!(
                block.get("type").and_then(|t| t.as_str()),
                Some("tool_use"),
                "last assistant in btw request must not have unanswered tool_use",
            );
        }
    }

    // Top-level thinking must be absent (no reasoning_effort set).
    assert!(
        json.get("thinking").is_none() || json.pointer("/thinking").is_some_and(|v| v.is_null()),
        "top-level thinking must be absent; got: {json:#}",
    );

    // Temperature must be absent (not hardcoded).
    assert!(
        json.get("temperature").is_none()
            || json.pointer("/temperature").is_some_and(|v| v.is_null()),
        "temperature must be absent so proxy defaults can apply; got: {json:#}",
    );

    // The completed tool pair (call_1) must survive.
    let has_tool_use_call_1 = messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|b| {
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                        && b.get("id").and_then(|id| id.as_str()) == Some("call_1")
                })
            })
    });
    assert!(
        has_tool_use_call_1,
        "completed tool_use call_1 must survive"
    );

    let has_tool_result_call_1 = messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|b| {
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        && b.get("tool_use_id").and_then(|id| id.as_str()) == Some("call_1")
                })
            })
    });
    assert!(
        has_tool_result_call_1,
        "completed tool_result for call_1 must survive"
    );
}

#[test]
fn test_tool_result_with_images_to_anthropic() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Read this"),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result_with_images(
            "call_1",
            "Read image file: photo.png",
            vec![ContentPart::Image {
                url: "data:image/png;base64,iVBOR".into(),
            }],
        ),
    ]);

    let messages_req = build_messages_request(&req, None);

    // Find the user message that contains the tool result
    // (the Messages API wraps tool results in user messages)
    let tool_result_msg = messages_req
        .messages
        .iter()
        .find(|m| {
            if let crate::messages::MessageContent::Blocks(blocks) = &m.content {
                blocks
                    .iter()
                    .any(|b| matches!(b, crate::messages::ContentBlock::ToolResult { .. }))
            } else {
                false
            }
        })
        .expect("Expected a message with ToolResult block");

    let crate::messages::MessageContent::Blocks(blocks) = &tool_result_msg.content else {
        panic!("Expected Blocks");
    };
    let tool_result_block = blocks
        .iter()
        .find_map(|b| {
            if let crate::messages::ContentBlock::ToolResult { content, .. } = b {
                Some(content)
            } else {
                None
            }
        })
        .unwrap();

    // Should be Blocks variant with text + image, not Text
    let crate::messages::ToolResultContent::Blocks(inner) = tool_result_block else {
        panic!("Expected ToolResultContent::Blocks, got Text");
    };
    assert_eq!(inner.len(), 2);
    assert!(
        matches!(&inner[0], crate::messages::ContentBlock::Text { text, .. } if text == "Read image file: photo.png")
    );
    assert!(
        matches!(&inner[1], crate::messages::ContentBlock::Image { source: crate::messages::ImageSource::Base64 { media_type, data }, .. } if media_type == "image/png" && data == "iVBOR")
    );
}

#[test]
fn upgrade_legacy_reasoning_singular_anthropic_no_id() {
    // Messages streaming sets id = "" (see stream/messages.rs:340).
    // The upgrader must still emit a sibling carrying text + signature.
    let raw = serde_json::json!({
        "type": "assistant",
        "content": "answer",
        "reasoning": {
            "text": "Let me think about this...",
            "encrypted": "signature-bytes-here",
            "id": ""
        },
        "model_id": "messages-compatible-model"
    });
    let mut seen = std::collections::HashSet::new();
    let siblings = upgrade_legacy_reasoning(&raw, &mut seen);
    assert_eq!(siblings.len(), 1);
    let ConversationItem::Reasoning(r) = &siblings[0] else {
        panic!("expected Reasoning sibling");
    };
    assert_eq!(r.id, "");
    assert_eq!(r.encrypted_content.as_deref(), Some("signature-bytes-here"));
}
/// A declared dialect decides the wire shape of `thinking`, and adaptive is
/// only one of three. Sending it to a model that predates it returns
/// `adaptive thinking is not supported on this model`, which is what the
/// unconditional mapping produced for every endpoint.
#[test]
fn test_messages_request_thinking_follows_the_declared_dialect() {
    let budget = crate::ThinkingDialect::Budget {
        budget_tokens: std::num::NonZeroU32::new(8_000).unwrap(),
    };
    for (dialect, want_type, want_budget) in [
        (crate::ThinkingDialect::Adaptive, Some("adaptive"), None),
        (budget, Some("enabled"), Some(8_000)),
        (crate::ThinkingDialect::Off, None, None),
    ] {
        let req = messages_test_request(Some(crate::ReasoningEffort::High));
        let json = serde_json::to_value(build_messages_request(&req, Some(dialect))).unwrap();
        assert_eq!(
            json.pointer("/thinking/type").and_then(|v| v.as_str()),
            want_type,
            "{dialect:?} should produce thinking.type={want_type:?}; got: {json:#}",
        );
        assert_eq!(
            json.pointer("/thinking/budget_tokens")
                .and_then(|v| v.as_u64()),
            want_budget,
            "{dialect:?} should produce budget_tokens={want_budget:?}; got: {json:#}",
        );
        // The effort is an independent knob and is unaffected either way.
        assert_eq!(
            json.pointer("/output_config/effort")
                .and_then(|v| v.as_str()),
            Some("high"),
            "{dialect:?} must not disturb the effort; got: {json:#}",
        );
    }
}

/// A declared dialect does not depend on an effort being requested: the
/// endpoints that still require `budget_tokens` are largely the ones that
/// reject the effort parameter outright, so coupling the two would make
/// that dialect unreachable exactly where it is needed.
#[test]
fn test_messages_request_declared_dialect_applies_without_an_effort() {
    let req = messages_test_request(None);
    let json = serde_json::to_value(build_messages_request(
        &req,
        Some(crate::ThinkingDialect::Budget {
            budget_tokens: std::num::NonZeroU32::new(4_096).unwrap(),
        }),
    ))
    .unwrap();
    assert_eq!(
        json.pointer("/thinking/type").and_then(|v| v.as_str()),
        Some("enabled"),
        "got: {json:#}",
    );
    assert!(
        json.get("output_config").is_none_or(|v| v.is_null()),
        "no effort was requested, so none may be sent; got: {json:#}",
    );
}

/// Collect the `type` of every content block across every message.
fn messages_block_types(json: &serde_json::Value) -> Vec<&str> {
    json.get("messages")
        .and_then(|m| m.as_array())
        .map(|msgs| {
            msgs.iter()
                .filter_map(|m| m.get("content").and_then(|c| c.as_array()))
                .flatten()
                .filter_map(|b| b.get("type").and_then(|t| t.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

/// A reasoning item whose `encrypted_content` came from another backend is
/// not a signature this endpoint minted — a non-empty `id` marks a
/// Responses-API payload (`rs_*`, or one of the parallel `tco_*`
/// backend-tool blobs). Replaying it produces
/// `Invalid \`signature\` in \`thinking\` block`, so no block may be
/// emitted at all.
#[test]
fn test_messages_request_drops_thinking_with_foreign_encrypted_content() {
    for id in ["rs_abc123", "tco_resp123_call-1"] {
        let req = ConversationRequest::from_items(vec![
            ConversationItem::user("hi"),
            reasoning_sibling(id, "considering the options", Some("foreign-blob")),
            ConversationItem::assistant("done"),
        ])
        .with_model("messages-compatible-model");
        let json = serde_json::to_value(build_messages_request(&req, None)).unwrap();
        let types = messages_block_types(&json);
        assert!(
            !types.contains(&"thinking"),
            "id {id:?} carries a foreign blob and must not produce a thinking block; \
             got: {json:#}",
        );
        assert!(
            !serde_json::to_string(&json)
                .unwrap()
                .contains("foreign-blob"),
            "the foreign blob must not reach the wire at all; got: {json:#}",
        );
        assert!(
            types.contains(&"text"),
            "the surrounding turn must still be sent; got: {json:#}",
        );
    }
}

/// Reasoning text synthesized off a chat-completions or fallback stream has
/// no `encrypted_content`, so there is no signature to send. The old guard
/// emitted the block anyway with `signature: ""`, which the Messages API
/// rejects the same way.
#[test]
fn test_messages_request_drops_thinking_without_encrypted_content() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("hi"),
        reasoning_sibling("", "thinking out loud", None),
        ConversationItem::assistant("done"),
    ])
    .with_model("messages-compatible-model");
    let json = serde_json::to_value(build_messages_request(&req, None)).unwrap();
    assert!(
        !messages_block_types(&json).contains(&"thinking"),
        "an unsigned reasoning item must not produce a thinking block; got: {json:#}",
    );
    assert!(
        !serde_json::to_string(&json)
            .unwrap()
            .contains("thinking out loud"),
        "the unsigned reasoning text must not reach the wire; got: {json:#}",
    );
}

/// The half this must keep: a signature minted by the Messages stream
/// consumer (empty `id`, because the protocol carries no item id) still
/// rides back out unchanged, so a same-model turn replays as before.
#[test]
fn test_messages_request_keeps_thinking_signed_by_this_backend() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("hi"),
        reasoning_sibling("", "weighing it up", Some("EqoBCkYIBRgCKkA")),
        ConversationItem::assistant("done"),
    ])
    .with_model("messages-compatible-model");
    let json = serde_json::to_value(build_messages_request(&req, None)).unwrap();
    let block = json
        .pointer("/messages/1/content/0")
        .unwrap_or_else(|| panic!("expected an assistant block; got: {json:#}"));
    assert_eq!(
        block.get("type").and_then(|t| t.as_str()),
        Some("thinking"),
        "got: {json:#}",
    );
    assert_eq!(
        block.get("signature").and_then(|s| s.as_str()),
        Some("EqoBCkYIBRgCKkA"),
        "the signature must ride back out byte-for-byte; got: {json:#}",
    );
    assert_eq!(
        block.get("thinking").and_then(|t| t.as_str()),
        Some("weighing it up"),
        "got: {json:#}",
    );
}


