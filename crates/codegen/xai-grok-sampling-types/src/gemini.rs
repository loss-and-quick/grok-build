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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_data: Option<GeminiFileData>,
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
    /// The id of the `functionCall` this responds to. Optional on the wire and
    /// documented as "populated by the client to match the corresponding
    /// function call `id`" — it is the only field that distinguishes two
    /// responses to two calls of the *same* function in one turn, since `name`
    /// is identical for both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiInlineData {
    pub mime_type: String,
    pub data: String,
}

/// A media reference the *server* resolves. `fileUri` is not a general URL
/// fetcher: Gemini resolves URIs it owns (a Files API `.../files/{id}` URI, a
/// Cloud Storage `gs://` URI on Vertex, or a YouTube link) and rejects anything
/// else. `mimeType` is documented as required but is omitted when it cannot be
/// determined, so the server's error names the real problem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiFileData {
    pub file_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
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
/// * A run of consecutive tool results collapses into a single `"user"` turn of
///   `functionResponse` parts, and images those results carried follow in the
///   next `"user"` turn (see [`flush_tool_responses`]).
/// * Images become `inlineData` or `fileData` (see [`image_part`]).
/// * `tools` map to a single `functionDeclarations` group; sampling knobs map
///   to `generationConfig`.
pub fn build_gemini_request(req: &ConversationRequest) -> GeminiRequest {
    let mut contents: Vec<GeminiContent> = Vec::new();
    let mut system_text = String::new();
    // Maps a tool-call id to the function name it invoked. `functionResponse`
    // requires `name`, but the unified `ToolResultItem` only carries the call
    // id. We recover the name from the preceding assistant `functionCall`
    // parts, which the builder sees first because conversation items are
    // processed in order.
    let mut call_id_to_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // Tool results buffered until the next non-tool-result item, so a parallel
    // batch lands in one turn rather than one turn each. Images attached to
    // those results are buffered separately: they cannot ride inside the
    // `functionResponse` turn (see [`flush_tool_responses`]).
    let mut pending_tool_responses: Vec<GeminiPart> = Vec::new();
    let mut pending_tool_media: Vec<GeminiPart> = Vec::new();

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
                flush_tool_responses(
                    &mut contents,
                    &mut pending_tool_responses,
                    &mut pending_tool_media,
                );
                push_turn(&mut contents, "user", content_parts_to_gemini(&u.content));
            }
            ConversationItem::Assistant(a) => {
                flush_tool_responses(
                    &mut contents,
                    &mut pending_tool_responses,
                    &mut pending_tool_media,
                );
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
                // user turn. `name` is required and is the function's name, so
                // it cannot by itself say *which* call is being answered when a
                // turn called the same function twice; `id` is what disambiguates
                // (see `GeminiFunctionResponse::id`). Recover the name from the
                // preceding call and always echo the id.
                //
                // Not expressible: if no matching `functionCall` was seen (a
                // replayed or truncated history), the function name is
                // unrecoverable. We fall back to the call id as the name —
                // wrong, but it keeps the response and the `id` correct rather
                // than dropping the tool output on the floor.
                let name = call_id_to_name
                    .get(&t.tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| t.tool_call_id.clone());
                let response = serde_json::json!({ "content": t.content.as_ref() });
                pending_tool_responses.push(GeminiPart {
                    function_response: Some(GeminiFunctionResponse {
                        id: Some(t.tool_call_id.clone()),
                        name: name.clone(),
                        response,
                    }),
                    ..Default::default()
                });
                // Images a tool produced (a screenshot, `read_file` on a PNG)
                // used to be dropped here, leaving the model to answer about an
                // image it never saw. They cannot live in the response turn, so
                // they are queued for the turn that follows it.
                if !t.images.is_empty() {
                    pending_tool_media.push(text_part(format!(
                        "Images returned by the {name} result above:"
                    )));
                    pending_tool_media.extend(content_parts_to_gemini(&t.images));
                }
            }
            // Gemini has no wire slot for backend-tool or reasoning siblings.
            ConversationItem::BackendToolCall(_) | ConversationItem::Reasoning(_) => {}
        }
    }
    flush_tool_responses(
        &mut contents,
        &mut pending_tool_responses,
        &mut pending_tool_media,
    );

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

/// Emit the buffered `functionResponse` parts as one `"user"` turn, followed by
/// any images those results carried.
///
/// Gemini's parallel-function-calling contract is that the responses to a batch
/// of `functionCall` parts arrive as a single turn whose parts are *only*
/// `functionResponse`s, so the response count matches the call count and the
/// two lists line up positionally. Emitting one turn per result loses that
/// alignment: nothing then tells a server that ignores `id` which response
/// answers which call, and two calls to the same function become
/// indistinguishable.
///
/// That same rule is why tool-result images cannot go where the other formats
/// put them. `functionResponse.response` is a JSON object with no slot for
/// image bytes, and adding `inlineData` parts to the response turn would break
/// the part-count match. So the images follow in their own `"user"` turn, each
/// group introduced by a text part naming the call it came from — the
/// association is stated in prose because the format has no structural way to
/// say it.
fn flush_tool_responses(
    contents: &mut Vec<GeminiContent>,
    pending: &mut Vec<GeminiPart>,
    pending_media: &mut Vec<GeminiPart>,
) {
    if !pending.is_empty() {
        contents.push(GeminiContent {
            role: Some("user".to_owned()),
            parts: std::mem::take(pending),
        });
    }
    if !pending_media.is_empty() {
        contents.push(GeminiContent {
            role: Some("user".to_owned()),
            parts: std::mem::take(pending_media),
        });
    }
}

/// Convert unified content parts into Gemini parts.
fn content_parts_to_gemini(parts: &[ContentPart]) -> Vec<GeminiPart> {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text_part(text.as_ref().to_owned()),
            ContentPart::Image { url } => image_part(url.as_ref()),
        })
        .collect()
}

fn text_part(text: String) -> GeminiPart {
    GeminiPart {
        text: Some(text),
        ..Default::default()
    }
}

/// Convert one image reference into the matching Gemini part.
///
/// Gemini accepts image bytes exactly two ways: `inlineData` (base64 plus a
/// mime type) or `fileData` (a URI the server resolves itself). It never
/// fetches an arbitrary URL on the caller's behalf, so a plain `https://` image
/// cannot be "passed through" — it becomes `fileData` and the server decides.
/// That is deliberate: a request Gemini rejects with a URI error is far better
/// than the previous behavior of pasting the URL into a text part, which left
/// the model answering blind about an image it never received while looking to
/// the user like the attachment went through.
fn image_part(url: &str) -> GeminiPart {
    if let Some(rest) = url.strip_prefix("data:") {
        // `data:[<mime>][;<param>]*[;base64],<payload>`; per RFC 2397 `;base64`
        // is always the last parameter, so any parameters in between (e.g.
        // `;charset=`) must be trimmed off the mime type rather than folded
        // into it.
        let Some((meta, payload)) = rest.split_once(',') else {
            return text_part("[image omitted: malformed data: URL]".to_owned());
        };
        let Some(meta) = meta.strip_suffix(";base64") else {
            // A percent-encoded (non-base64) data: URL would have to be decoded
            // and re-encoded to become `inlineData`, and this crate carries no
            // base64 encoder. Say so instead of shipping something the model
            // cannot read.
            return text_part(
                "[image omitted: data: URL is not base64-encoded, which Gemini's inlineData requires]"
                    .to_owned(),
            );
        };
        let mime_type = match meta.split(';').next().unwrap_or_default() {
            "" => DEFAULT_IMAGE_MIME_TYPE,
            mime => mime,
        };
        return GeminiPart {
            inline_data: Some(GeminiInlineData {
                mime_type: mime_type.to_owned(),
                data: payload.to_owned(),
            }),
            ..Default::default()
        };
    }

    GeminiPart {
        file_data: Some(GeminiFileData {
            mime_type: mime_type_from_uri(url).map(str::to_owned),
            file_uri: url.to_owned(),
        }),
        ..Default::default()
    }
}

/// Mime type for a `data:` URL that declared none. RFC 2397's default is
/// `text/plain`, which is never right for an image part, so fall back to the
/// same guess the Anthropic conversion in [`crate::conversation`] makes.
const DEFAULT_IMAGE_MIME_TYPE: &str = "image/png";

/// Guess `fileData.mimeType` from the URI's extension.
///
/// Returns `None` rather than a guess when the extension says nothing: for a
/// Files API URI the server already knows the type, and for anything else a
/// wrong mime type is worse than an absent one — Gemini decodes by the declared
/// type, so mislabeling a PNG as a JPEG corrupts the image instead of failing.
fn mime_type_from_uri(uri: &str) -> Option<&'static str> {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    // Only the last path segment can hold the extension — a dot in the host
    // (`example.com/photo`) is not one.
    let last_segment = path.rsplit('/').next().unwrap_or(path);
    let ext = last_segment.rsplit_once('.')?.1.to_ascii_lowercase();
    // Gemini's documented image and document input types.
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "pdf" => "application/pdf",
        _ => return None,
    })
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

    /// Two calls to the *same* function in one turn: the two responses must stay
    /// distinguishable. `name` is identical for both, so the only discriminator
    /// is `functionResponse.id`, and both responses must ride in one turn so
    /// their order still matches the order of the `functionCall` parts.
    #[test]
    fn two_calls_to_one_function_keep_their_results_attributed() {
        use crate::conversation::{AssistantItem, ToolCall, ToolResultItem};
        let call = |id: &str, args: &str| ToolCall {
            id: std::sync::Arc::from(id),
            name: "read_file".to_owned(),
            arguments: std::sync::Arc::from(args),
        };
        let req = ConversationRequest {
            items: vec![
                ConversationItem::user("read both".to_owned()),
                ConversationItem::Assistant(AssistantItem {
                    content: std::sync::Arc::from(""),
                    tool_calls: vec![
                        call("call_a", r#"{"path":"a.txt"}"#),
                        call("call_b", r#"{"path":"b.txt"}"#),
                    ],
                    model_id: None,
                    model_fingerprint: None,
                    reasoning_effort: None,
                }),
                ConversationItem::ToolResult(ToolResultItem {
                    tool_call_id: "call_a".to_owned(),
                    content: std::sync::Arc::from("body of a"),
                    images: Vec::new(),
                }),
                ConversationItem::ToolResult(ToolResultItem {
                    tool_call_id: "call_b".to_owned(),
                    content: std::sync::Arc::from("body of b"),
                    images: Vec::new(),
                }),
            ],
            ..Default::default()
        };
        let g = build_gemini_request(&req);

        // contents = [user, model(2 calls), user(2 functionResponses)] — one turn
        // for the batch, not one per result.
        assert_eq!(g.contents.len(), 3, "{:#?}", g.contents);
        let calls = &g.contents[1].parts;
        let responses = &g.contents[2].parts;
        assert_eq!(calls.len(), 2);
        assert_eq!(
            responses.len(),
            2,
            "both responses belong to the same turn so they line up with the calls",
        );
        assert!(
            responses.iter().all(|p| p.function_response.is_some()),
            "the response turn carries only functionResponse parts",
        );

        for (call, response) in calls.iter().zip(responses) {
            let fc = call.function_call.as_ref().unwrap();
            let fr = response.function_response.as_ref().unwrap();
            assert_eq!(fr.name, fc.name, "same function name for both");
            assert_eq!(
                fr.id, fc.id,
                "functionResponse.id must match its functionCall.id",
            );
        }
        // The bodies did not get swapped.
        let body_of = |part: &GeminiPart| {
            part.function_response.as_ref().unwrap().response["content"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(body_of(&responses[0]), "body of a");
        assert_eq!(body_of(&responses[1]), "body of b");

        // And the id survives serialization under its wire key.
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(
            json.pointer("/contents/2/parts/1/functionResponse/id")
                .and_then(serde_json::Value::as_str),
            Some("call_b"),
            "{json:#}",
        );
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

    /// A non-`data:` image URL must reach the wire as a resolvable reference.
    /// Gemini cannot fetch it and will say so; the old text placeholder said
    /// nothing and left the model answering blind.
    #[test]
    fn http_image_url_becomes_file_data_not_a_text_placeholder() {
        use crate::conversation::{ContentPart, UserItem};
        let req = ConversationRequest {
            items: vec![ConversationItem::User(UserItem {
                content: vec![
                    ContentPart::Text {
                        text: std::sync::Arc::from("what is this?"),
                    },
                    ContentPart::Image {
                        url: std::sync::Arc::from("https://example.com/photo.PNG?v=2"),
                    },
                ],
                ..Default::default()
            })],
            ..Default::default()
        };
        let g = build_gemini_request(&req);
        let image = &g.contents[0].parts[1];
        assert!(
            image.text.is_none(),
            "an image must never degrade into a text part: {image:?}",
        );
        let fd = image.file_data.as_ref().expect("expected fileData");
        assert_eq!(fd.file_uri, "https://example.com/photo.PNG?v=2");
        assert_eq!(
            fd.mime_type.as_deref(),
            Some("image/png"),
            "mime type comes from the extension, case- and query-insensitively",
        );

        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(
            json.pointer("/contents/0/parts/1/fileData/mimeType")
                .and_then(serde_json::Value::as_str),
            Some("image/png"),
            "{json:#}",
        );
    }

    /// An extension that says nothing leaves `mimeType` off rather than guessing:
    /// Gemini decodes by the declared type, so a wrong one corrupts the image.
    #[test]
    fn unknown_extension_omits_the_file_data_mime_type() {
        use crate::conversation::{ContentPart, UserItem};
        let req = ConversationRequest {
            items: vec![ConversationItem::User(UserItem {
                content: vec![ContentPart::Image {
                    url: std::sync::Arc::from("https://example.com/files/opaque-id"),
                }],
                ..Default::default()
            })],
            ..Default::default()
        };
        let g = build_gemini_request(&req);
        let fd = g.contents[0].parts[0].file_data.as_ref().unwrap();
        assert_eq!(fd.file_uri, "https://example.com/files/opaque-id");
        assert_eq!(fd.mime_type, None);
        let json = serde_json::to_value(&g).unwrap();
        assert!(
            json.pointer("/contents/0/parts/0/fileData/mimeType")
                .is_none(),
            "an absent mime type must not serialize as null: {json:#}",
        );
    }

    /// `data:` URLs keep going to `inlineData`, including when extra parameters
    /// sit between the mime type and `;base64`.
    #[test]
    fn data_url_becomes_inline_data_with_parameters_stripped() {
        assert_eq!(
            image_part("data:image/jpeg;base64,QUJD").inline_data,
            Some(GeminiInlineData {
                mime_type: "image/jpeg".to_owned(),
                data: "QUJD".to_owned(),
            }),
        );
        assert_eq!(
            image_part("data:image/webp;charset=utf-8;base64,QUJD").inline_data,
            Some(GeminiInlineData {
                mime_type: "image/webp".to_owned(),
                data: "QUJD".to_owned(),
            }),
            "a `;charset=` parameter must not end up inside the mime type",
        );

        // Not expressible: without a base64 payload there is nothing to put in
        // `inlineData`, so the part says so instead of pretending.
        let percent_encoded = image_part("data:image/svg+xml,%3Csvg%2F%3E");
        assert!(percent_encoded.inline_data.is_none());
        assert!(percent_encoded.file_data.is_none());
        let text = percent_encoded.text.unwrap();
        assert!(text.contains("not base64-encoded"), "{text}");
    }

    /// Images attached to a tool result used to be dropped outright. They now
    /// ride in the turn after the `functionResponse` turn, which must keep
    /// carrying only `functionResponse` parts.
    #[test]
    fn tool_result_images_reach_the_model_after_the_response_turn() {
        use crate::conversation::{AssistantItem, ContentPart, ToolCall, ToolResultItem};
        let req = ConversationRequest {
            items: vec![
                ConversationItem::Assistant(AssistantItem {
                    content: std::sync::Arc::from(""),
                    tool_calls: vec![ToolCall {
                        id: std::sync::Arc::from("call_1"),
                        name: "screenshot".to_owned(),
                        arguments: std::sync::Arc::from("{}"),
                    }],
                    model_id: None,
                    model_fingerprint: None,
                    reasoning_effort: None,
                }),
                ConversationItem::ToolResult(ToolResultItem {
                    tool_call_id: "call_1".to_owned(),
                    content: std::sync::Arc::from("captured"),
                    images: vec![ContentPart::Image {
                        url: std::sync::Arc::from("data:image/png;base64,QUJD"),
                    }],
                }),
            ],
            ..Default::default()
        };
        let g = build_gemini_request(&req);
        // contents = [model(call), user(functionResponse), user(images)]
        assert_eq!(g.contents.len(), 3, "{:#?}", g.contents);
        assert_eq!(g.contents[1].parts.len(), 1);
        assert!(
            g.contents[1].parts[0].function_response.is_some(),
            "the response turn stays functionResponse-only",
        );
        let media = &g.contents[2];
        assert_eq!(media.role.as_deref(), Some("user"));
        assert!(
            media.parts[0]
                .text
                .as_deref()
                .is_some_and(|t| t.contains("screenshot")),
            "the images are introduced by the call they came from: {:?}",
            media.parts[0],
        );
        assert_eq!(
            media.parts[1].inline_data,
            Some(GeminiInlineData {
                mime_type: "image/png".to_owned(),
                data: "QUJD".to_owned(),
            }),
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
