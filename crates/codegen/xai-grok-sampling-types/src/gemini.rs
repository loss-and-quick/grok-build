//! Google Gemini (`generateContent`) wire types and request builder.
//!
//! Mirrors [`crate::messages`]: the wire structs live here so config/sampler
//! code can build a request and decode a `streamGenerateContent` chunk without
//! reaching into the sampler. The streaming L2 transform that turns these
//! chunks into `SamplingEvent`s lives in the sampler crate.

use serde::{Deserialize, Serialize};

use crate::conversation::{ContentPart, ConversationItem, ConversationRequest};

// ============================================================================
// Request
// ============================================================================

/// A `generateContent` / `streamGenerateContent` request body.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiRequest {
    pub contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GeminiGenerationConfig>,
}

/// One turn of content. `role` is `"user"` or `"model"` for conversation
/// turns and absent on `system_instruction`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub parts: Vec<GeminiPart>,
}

/// A single part of a content turn. Exactly one payload field is set.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_response: Option<GeminiFunctionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<GeminiInlineData>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiFunctionCall {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiFunctionResponse {
    pub name: String,
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiInlineData {
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiTool {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiFunctionDeclaration {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<GeminiThinkingConfig>,
}

/// Gemini's reasoning knob. Unlike the other three formats it takes a token
/// budget rather than a discrete level, so [`thinking_budget_for_effort`]
/// translates.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiThinkingConfig {
    /// Thinking token allowance. `0` disables thinking; `-1` hands the ceiling
    /// to the server ("dynamic"); any other value is a request the server may
    /// clamp to the model's own range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
    /// Whether thought summaries come back as `thought`-flagged parts. Left
    /// unset (i.e. off) — the L2 stream transform has no branch for those
    /// parts, so asking for them would splice reasoning into assistant text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_thoughts: Option<bool>,
}

/// Map a discrete reasoning effort onto a Gemini `thinkingBudget`.
///
/// The other three wire formats take the level as-is; Gemini takes a token
/// count, so the ladder below is a client-side convention, not a protocol
/// requirement. Two values are documented sentinels and carry exact meaning:
/// `0` disables thinking and `-1` requests a server-chosen ("dynamic")
/// budget. The rest double at each step across the range that thinking-capable
/// models accept, and the server clamps a value its model cannot honor.
///
/// `Max` maps to the dynamic sentinel rather than a large literal: no fixed
/// number is "the maximum" across models, and `-1` is the only spelling that
/// means "as much as this model allows".
pub fn thinking_budget_for_effort(effort: crate::ReasoningEffort) -> i32 {
    use crate::ReasoningEffort as E;
    match effort {
        E::None => 0,
        E::Minimal => 1024,
        E::Low => 4096,
        E::Medium => 8192,
        E::High => 16384,
        E::Xhigh => 24576,
        E::Max => -1,
    }
}

// ============================================================================
// Response (streamed chunk)
// ============================================================================

/// One `streamGenerateContent` SSE chunk (also the non-streaming response
/// shape). Only the fields the sampler consumes are modeled; unknown fields
/// are ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiStreamChunk {
    #[serde(default)]
    pub candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    pub usage_metadata: Option<GeminiUsageMetadata>,
    #[serde(default)]
    pub model_version: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiCandidate {
    #[serde(default)]
    pub content: Option<GeminiContent>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUsageMetadata {
    #[serde(default)]
    pub prompt_token_count: u32,
    #[serde(default)]
    pub candidates_token_count: u32,
    #[serde(default)]
    pub total_token_count: u32,
    #[serde(default)]
    pub cached_content_token_count: u32,
}

// ============================================================================
// Request builder
// ============================================================================

/// Convert a unified [`ConversationRequest`] into a Gemini request body.
///
/// * System items collapse into `system_instruction`.
/// * User content becomes a `"user"` turn; assistant text + tool calls become
///   a `"model"` turn (each tool call a `functionCall` part).
/// * Tool results become a `"user"` turn carrying a `functionResponse` part.
/// * `tools` map to a single `functionDeclarations` group; sampling knobs map
///   to `generationConfig`.
pub fn build_gemini_request(req: &ConversationRequest) -> GeminiRequest {
    let mut contents: Vec<GeminiContent> = Vec::new();
    let mut system_text = String::new();
    // Maps a tool-call id to the function name it invoked. Gemini pairs a
    // `functionResponse` to its `functionCall` by name, not id, but the unified
    // `ToolResultItem` only carries the call id. We recover the name from the
    // preceding assistant `functionCall` parts, which the builder sees first
    // because conversation items are processed in order.
    let mut call_id_to_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    let push_turn = |contents: &mut Vec<GeminiContent>, role: &str, parts: Vec<GeminiPart>| {
        if !parts.is_empty() {
            contents.push(GeminiContent {
                role: Some(role.to_owned()),
                parts,
            });
        }
    };

    for item in &req.items {
        match item {
            ConversationItem::System(s) => {
                if !system_text.is_empty() {
                    system_text.push('\n');
                }
                system_text.push_str(s.content.as_ref());
            }
            ConversationItem::User(u) => {
                push_turn(&mut contents, "user", content_parts_to_gemini(&u.content));
            }
            ConversationItem::Assistant(a) => {
                let mut parts: Vec<GeminiPart> = Vec::new();
                if !a.content.is_empty() {
                    parts.push(GeminiPart {
                        text: Some(a.content.as_ref().to_owned()),
                        ..Default::default()
                    });
                }
                for tc in &a.tool_calls {
                    call_id_to_name.insert(tc.id.as_ref().to_owned(), tc.name.clone());
                    parts.push(GeminiPart {
                        function_call: Some(GeminiFunctionCall {
                            name: tc.name.clone(),
                            args: serde_json::from_str(&tc.arguments).ok(),
                            id: Some(tc.id.as_ref().to_owned()),
                        }),
                        ..Default::default()
                    });
                }
                push_turn(&mut contents, "model", parts);
            }
            ConversationItem::ToolResult(t) => {
                // Gemini carries tool output as a `functionResponse` part in a
                // user turn and pairs it to its `functionCall` by function name.
                // Recover the name from the preceding call; fall back to the id
                // when no matching call was seen (degraded, but preserves the
                // prior behavior rather than dropping the response).
                let name = call_id_to_name
                    .get(&t.tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| t.tool_call_id.clone());
                let response = serde_json::json!({ "content": t.content.as_ref() });
                push_turn(
                    &mut contents,
                    "user",
                    vec![GeminiPart {
                        function_response: Some(GeminiFunctionResponse { name, response }),
                        ..Default::default()
                    }],
                );
            }
            // Gemini has no wire slot for backend-tool or reasoning siblings.
            ConversationItem::BackendToolCall(_) | ConversationItem::Reasoning(_) => {}
        }
    }

    let system_instruction = (!system_text.is_empty()).then(|| GeminiContent {
        role: None,
        parts: vec![GeminiPart {
            text: Some(system_text),
            ..Default::default()
        }],
    });

    let tools = (!req.tools.is_empty()).then(|| {
        vec![GeminiTool {
            function_declarations: req
                .tools
                .iter()
                .map(|t| GeminiFunctionDeclaration {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: Some(t.parameters.clone()),
                })
                .collect(),
        }]
    });

    // Gemini's reasoning knob is a token budget, so an effort the caller chose
    // becomes `thinkingConfig.thinkingBudget`. Absent an effort the key is
    // omitted entirely and the model keeps its own default.
    let thinking_config = req.reasoning_effort.map(|effort| GeminiThinkingConfig {
        thinking_budget: Some(thinking_budget_for_effort(effort)),
        include_thoughts: None,
    });

    let generation_config = (req.temperature.is_some()
        || req.top_p.is_some()
        || req.max_output_tokens.is_some()
        || thinking_config.is_some())
    .then_some(GeminiGenerationConfig {
        temperature: req.temperature,
        top_p: req.top_p,
        max_output_tokens: req.max_output_tokens,
        thinking_config,
    });

    GeminiRequest {
        contents,
        system_instruction,
        tools,
        generation_config,
    }
}

/// Convert unified content parts into Gemini parts. Base64 `data:` images
/// become `inlineData`; other parts become text.
fn content_parts_to_gemini(parts: &[ContentPart]) -> Vec<GeminiPart> {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => GeminiPart {
                text: Some(text.as_ref().to_owned()),
                ..Default::default()
            },
            ContentPart::Image { url } => {
                if let Some(rest) = url.strip_prefix("data:")
                    && let Some((mime_type, data)) = rest.split_once(";base64,")
                {
                    GeminiPart {
                        inline_data: Some(GeminiInlineData {
                            mime_type: mime_type.to_owned(),
                            data: data.to_owned(),
                        }),
                        ..Default::default()
                    }
                } else {
                    GeminiPart {
                        text: Some(format!("[image: {url}]")),
                        ..Default::default()
                    }
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ConversationItem, ToolSpec};

    #[test]
    fn builds_contents_system_and_generation_config() {
        let req = ConversationRequest {
            items: vec![
                ConversationItem::system("be brief".to_owned()),
                ConversationItem::user("hello".to_owned()),
            ],
            temperature: Some(0.5),
            max_output_tokens: Some(256),
            ..Default::default()
        };
        let g = build_gemini_request(&req);
        assert_eq!(
            g.system_instruction.as_ref().unwrap().parts[0]
                .text
                .as_deref(),
            Some("be brief")
        );
        assert_eq!(g.contents.len(), 1);
        assert_eq!(g.contents[0].role.as_deref(), Some("user"));
        assert_eq!(g.contents[0].parts[0].text.as_deref(), Some("hello"));
        let gc = g.generation_config.unwrap();
        assert_eq!(gc.temperature, Some(0.5));
        assert_eq!(gc.max_output_tokens, Some(256));
        assert!(g.tools.is_none());
    }

    #[test]
    fn maps_tools_and_tool_call_round_trip() {
        let req = ConversationRequest {
            items: vec![ConversationItem::user("go".to_owned())],
            tools: vec![ToolSpec {
                name: "read_file".to_owned(),
                description: Some("read a file".to_owned()),
                parameters: serde_json::json!({"type": "object"}),
            }],
            ..Default::default()
        };
        let g = build_gemini_request(&req);
        let decls = &g.tools.as_ref().unwrap()[0].function_declarations;
        assert_eq!(decls[0].name, "read_file");
        assert_eq!(decls[0].description.as_deref(), Some("read a file"));

        // Serializes with camelCase wire keys.
        let json = serde_json::to_value(&g).unwrap();
        assert!(json["tools"][0]["functionDeclarations"][0]["name"] == "read_file");
    }

    #[test]
    fn assistant_tool_call_becomes_model_function_call_part() {
        use crate::conversation::{AssistantItem, ToolCall};
        let req = ConversationRequest {
            items: vec![ConversationItem::Assistant(AssistantItem {
                content: std::sync::Arc::from(""),
                tool_calls: vec![ToolCall {
                    id: std::sync::Arc::from("call_1"),
                    name: "search".to_owned(),
                    arguments: std::sync::Arc::from(r#"{"q":"x"}"#),
                }],
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            })],
            ..Default::default()
        };
        let g = build_gemini_request(&req);
        assert_eq!(g.contents[0].role.as_deref(), Some("model"));
        let fc = g.contents[0].parts[0].function_call.as_ref().unwrap();
        assert_eq!(fc.name, "search");
        assert_eq!(fc.args.as_ref().unwrap()["q"], "x");
    }

    #[test]
    fn tool_result_function_response_name_is_the_function_not_the_call_id() {
        use crate::conversation::{AssistantItem, ToolCall, ToolResultItem};
        let req = ConversationRequest {
            items: vec![
                ConversationItem::user("read it".to_owned()),
                ConversationItem::Assistant(AssistantItem {
                    content: std::sync::Arc::from(""),
                    tool_calls: vec![ToolCall {
                        id: std::sync::Arc::from("call_abc"),
                        name: "read_file".to_owned(),
                        arguments: std::sync::Arc::from("{}"),
                    }],
                    model_id: None,
                    model_fingerprint: None,
                    reasoning_effort: None,
                }),
                ConversationItem::ToolResult(ToolResultItem {
                    tool_call_id: "call_abc".to_owned(),
                    content: std::sync::Arc::from("file body"),
                    images: Vec::new(),
                }),
            ],
            ..Default::default()
        };
        let g = build_gemini_request(&req);
        // contents = [user, model, user(functionResponse)]
        assert_eq!(g.contents.len(), 3);
        let fr = g.contents[2].parts[0].function_response.as_ref().unwrap();
        assert_eq!(
            fr.name, "read_file",
            "functionResponse must pair by function name, not the call id"
        );
        assert_ne!(fr.name, "call_abc");
    }

    #[test]
    fn tool_result_without_preceding_call_falls_back_to_call_id() {
        use crate::conversation::ToolResultItem;
        let req = ConversationRequest {
            items: vec![ConversationItem::ToolResult(ToolResultItem {
                tool_call_id: "orphan_call".to_owned(),
                content: std::sync::Arc::from("stray result"),
                images: Vec::new(),
            })],
            ..Default::default()
        };
        let g = build_gemini_request(&req);
        let fr = g.contents[0].parts[0].function_response.as_ref().unwrap();
        assert_eq!(
            fr.name, "orphan_call",
            "with no matching call, degrade to the id rather than dropping the response"
        );
    }

    /// Every effort level reaches `generationConfig.thinkingConfig` on the
    /// serialized body, with the two sentinels intact.
    #[test]
    fn reasoning_effort_maps_onto_thinking_budget() {
        use crate::ReasoningEffort as E;
        let cases = [
            (E::None, 0),
            (E::Minimal, 1024),
            (E::Low, 4096),
            (E::Medium, 8192),
            (E::High, 16384),
            (E::Xhigh, 24576),
            (E::Max, -1),
        ];
        for (effort, want) in cases {
            let req = ConversationRequest {
                items: vec![ConversationItem::user("hi".to_owned())],
                reasoning_effort: Some(effort),
                ..Default::default()
            };
            let g = build_gemini_request(&req);
            let json = serde_json::to_value(&g).unwrap();
            assert_eq!(
                json.pointer("/generationConfig/thinkingConfig/thinkingBudget")
                    .and_then(serde_json::Value::as_i64),
                Some(i64::from(want)),
                "{effort:?} should send thinkingBudget={want}; got {json:#}",
            );
            // Thought parts have no branch in the L2 transform, so we must not
            // ask for them.
            assert!(
                json.pointer("/generationConfig/thinkingConfig/includeThoughts")
                    .is_none(),
                "includeThoughts must stay off: {json:#}",
            );
        }
    }

    /// No effort chosen = no `thinkingConfig` at all, so a model keeps its own
    /// default and a provider that never declared effort support is unaffected.
    #[test]
    fn absent_reasoning_effort_omits_thinking_config() {
        let req = ConversationRequest {
            items: vec![ConversationItem::user("hi".to_owned())],
            ..Default::default()
        };
        let g = build_gemini_request(&req);
        assert!(
            g.generation_config.is_none(),
            "no sampling knobs and no effort = no generationConfig"
        );

        let with_temp = ConversationRequest {
            items: vec![ConversationItem::user("hi".to_owned())],
            temperature: Some(0.5),
            ..Default::default()
        };
        let g = build_gemini_request(&with_temp);
        assert!(g.generation_config.unwrap().thinking_config.is_none());
    }

    /// `thinkingConfig` rides alongside the existing knobs rather than
    /// replacing them.
    #[test]
    fn thinking_config_coexists_with_sampling_knobs() {
        let req = ConversationRequest {
            items: vec![ConversationItem::user("hi".to_owned())],
            temperature: Some(0.3),
            top_p: Some(0.9),
            max_output_tokens: Some(128),
            reasoning_effort: Some(crate::ReasoningEffort::High),
            ..Default::default()
        };
        let gc = build_gemini_request(&req).generation_config.unwrap();
        assert_eq!(gc.temperature, Some(0.3));
        assert_eq!(gc.top_p, Some(0.9));
        assert_eq!(gc.max_output_tokens, Some(128));
        assert_eq!(
            gc.thinking_config.unwrap().thinking_budget,
            Some(thinking_budget_for_effort(crate::ReasoningEffort::High))
        );
    }
}
