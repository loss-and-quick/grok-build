//! Responses API wire format.

use super::*;

/// Whether a terminal Responses API `Response` is a content refusal, and the
/// explanation the model gave for it.
///
/// `Some(explanation)` means the model refused; the explanation may be empty
/// when the backend reported a refusal without text, so the *presence* of the
/// `Some` — not the string — is the refusal signal.
///
/// A refusal arrives as an `OutputMessageContent::Refusal` part inside the
/// output message (and, while streaming, as `response.refusal.delta` /
/// `.done`). It is not model output: it never becomes assistant content, so
/// [`response_to_conversation_items`] drops those parts and the streaming
/// transform lifts the text here instead, onto
/// [`ConversationResponse::stop_message`] alongside
/// [`StopReason::ContentFilter`]. That is the same normalization the Messages
/// backend applies to `message_delta.stop_details.explanation`.
///
/// Multiple refusal parts (never seen in practice) are joined with newlines.
pub fn response_refusal(response: &rs::Response) -> Option<String> {
    let mut refusal: Option<String> = None;
    for item in &response.output {
        if let rs::OutputItem::Message(msg) = item {
            for content_part in &msg.content {
                if let rs::OutputMessageContent::Refusal(refusal_content) = content_part {
                    let acc = refusal.get_or_insert_with(String::new);
                    if !acc.is_empty() && !refusal_content.refusal.is_empty() {
                        acc.push('\n');
                    }
                    acc.push_str(&refusal_content.refusal);
                }
            }
        }
    }
    refusal
}

/// Flatten `response.output` into `ConversationItem`s, preserving emission
/// order. Replaying that order byte for byte on the next turn is what keeps
/// the server-side prefix cache hot.
pub fn response_to_conversation_items(response: rs::Response) -> Vec<ConversationItem> {
    let model_id = response.model.clone();
    let model_fingerprint = response
        .metadata
        .as_ref()
        .and_then(|m| m.get("system_fingerprint"))
        .cloned()
        .filter(|s| !s.is_empty());
    let reasoning_effort = response
        .reasoning
        .as_ref()
        .and_then(|r| r.effort.clone())
        .map(crate::ReasoningEffort::from_responses_api);

    let mut items: Vec<ConversationItem> = Vec::with_capacity(response.output.len() + 1);
    let mut content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut backend_tool_count: usize = 0;

    for item in response.output {
        match item {
            rs::OutputItem::Message(msg) => {
                for content_part in msg.content {
                    // `Refusal` parts are deliberately NOT folded into the
                    // assistant text: a refusal is surfaced out of band as
                    // `StopReason::ContentFilter` + `stop_message` (see
                    // [`response_refusal`]), matching the Messages backend,
                    // where the refusal explanation likewise never becomes
                    // assistant content. Folding it in would make the response
                    // look non-empty and suppress the shell's refusal notice.
                    if let rs::OutputMessageContent::OutputText(text_content) = content_part {
                        if !content.is_empty() {
                            content.push('\n');
                        }
                        content.push_str(&text_content.text);
                    }
                }
            }
            rs::OutputItem::FunctionCall(fc) => {
                // Tied to the assistant turn: a ToolResult must follow each
                // one in conversation order, so they are not siblings.
                tool_calls.push(ToolCall {
                    id: Arc::<str>::from(fc.call_id),
                    name: fc.name,
                    arguments: Arc::<str>::from(fc.arguments),
                });
            }
            rs::OutputItem::Reasoning(r) => {
                items.push(ConversationItem::Reasoning(r));
            }
            // Already run server-side; kept so later turns replay the same
            // context.
            rs::OutputItem::WebSearchCall(ws) => {
                backend_tool_count += 1;
                items.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                    kind: BackendToolKind::WebSearch(ws),
                }));
            }
            rs::OutputItem::CustomToolCall(ct) => {
                backend_tool_count += 1;
                items.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                    kind: BackendToolKind::XSearch(ct),
                }));
            }
            rs::OutputItem::CodeInterpreterCall(ci) => {
                backend_tool_count += 1;
                items.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                    kind: BackendToolKind::CodeInterpreter(ci),
                }));
            }
            rs::OutputItem::McpCall(_) => {
                backend_tool_count += 1;
            }
            _ => {}
        }
    }

    if backend_tool_count > 0 {
        tracing::info!(
            backend_tool_count,
            "response contained backend-executed tool calls"
        );
    }

    tracing::info!(model_id = %model_id, ?model_fingerprint, ?reasoning_effort, "response_to_conversation_items setting model metadata on AssistantItem");
    items.push(ConversationItem::Assistant(AssistantItem {
        content: Arc::<str>::from(content),
        tool_calls,
        model_id: Some(model_id),
        model_fingerprint,
        reasoning_effort,
    }));

    items
}

impl From<&ConversationRequest> for rs::CreateResponse {
    fn from(req: &ConversationRequest) -> Self {
        let input = build_responses_input(req);
        let tools = build_responses_tools(req);

        // Only set `tool_choice` when there are `tools` to constrain — an
        // OpenAI-compatible endpoint rejects `tool_choice` with no `tools`
        // (`required` most loudly). Same guard as the Chat Completions
        // conversion.
        let tool_choice = req
            .tool_choice
            .as_ref()
            .filter(|_| !tools.is_empty())
            .map(|tc| match tc {
                ConversationToolChoice::Auto => {
                    rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::Auto)
                }
                ConversationToolChoice::None => {
                    rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::None)
                }
                ConversationToolChoice::Required => {
                    rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::Required)
                }
                ConversationToolChoice::Function(name) => {
                    rs::ToolChoiceParam::Function(rs::ToolChoiceFunction { name: name.clone() })
                }
            });

        let text = req
            .json_schema
            .as_ref()
            .map(|schema| rs::ResponseTextParam {
                format: rs::TextResponseFormatConfiguration::JsonSchema(
                    rs::ResponseFormatJsonSchema {
                        description: None,
                        name: STRUCTURED_OUTPUT_SCHEMA_NAME.to_string(),
                        schema: Some(schema.clone()),
                        strict: Some(true),
                    },
                ),
                verbosity: None,
            });

        rs::CreateResponse {
            background: None,
            conversation: None,
            include: None,
            input,
            instructions: None,
            max_output_tokens: req.max_output_tokens,
            max_tool_calls: None,
            metadata: None,
            model: req.model.clone(),
            parallel_tool_calls: None,
            previous_response_id: None,
            prompt: None,
            prompt_cache_key: req
                .prompt_cache_key
                .clone()
                .or_else(|| req.x_grok_conv_id.clone()),
            prompt_cache_retention: None,
            // Send `reasoning` only when an effort was actually requested. A
            // model that does not reason rejects the parameter outright, and
            // "no effort requested" is how the caller reports exactly that:
            // the model catalog only fills `reasoning_effort` in for entries
            // flagged `supports_reasoning_effort`. Sending
            // `{"summary": "concise"}` with no effort asked a non-reasoning
            // model for a reasoning summary and got the whole request refused.
            //
            // The `summary` value is left alone on purpose. The vendored
            // async-openai documents `concise` as supported only for
            // `computer-use-preview` and reasoning models after `gpt-5`, so it
            // is wrong for the o3/o4-mini generation — but picking per model
            // means classifying an arbitrary third-party model by slug, which
            // is not something this crate can do: it sees only the slug the
            // user configured, with no provider identity attached. (An
            // OpenAI-compatible gateway can do it because it knows which
            // provider it is talking to and gates every such rewrite on that.)
            // The fix belongs in the model catalog as a per-model summary
            // style, not in a slug match here.
            reasoning: req.reasoning_effort.map(|effort| rs::Reasoning {
                effort: Some(effort.to_responses_api()),
                summary: Some(rs::ReasoningSummary::Concise),
            }),
            safety_identifier: None,
            service_tier: None,
            store: None,
            stream: None,
            stream_options: None,
            temperature: req.temperature,
            text,
            tool_choice,
            tools: if tools.is_empty() { None } else { Some(tools) },
            top_logprobs: None,
            top_p: req.top_p,
            truncation: None,
        }
    }
}

/// Reasoning items stay top-level siblings rather than folding into the
/// assistant, so the input replays the model's original order.
pub(super) fn build_responses_input(req: &ConversationRequest) -> rs::InputParam {
    let mut items: Vec<rs::InputItem> = req
        .items
        .iter()
        .flat_map(conversation_item_to_input_items)
        .collect();
    drop_dangling_reasoning(&mut items);
    rs::InputParam::Items(items)
}

/// How an emitted `input[]` entry relates to a reasoning item that precedes
/// it — the classification the dangling-reasoning filter runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReasoningFollower {
    /// Another reasoning item. A run of them is legal exactly when the item
    /// after the run is one the run can attach to.
    Reasoning,
    /// A model-generated item of the same assistant turn: the message, tool
    /// call or backend-tool call that a preceding reasoning item belongs to.
    SameTurn,
    /// Anything else — user/system input, a tool result, a compaction
    /// summary, an item reference. A reasoning item before one of these has
    /// nothing to attach to.
    Foreign,
}

/// Classify one emitted `input[]` entry for [`drop_dangling_reasoning`].
fn reasoning_follower(item: &rs::InputItem) -> ReasoningFollower {
    match item {
        rs::InputItem::Item(rs::Item::Reasoning(_)) => ReasoningFollower::Reasoning,
        // Assistant text is emitted as an `EasyMessage`; user/system text
        // uses the same variant, so the role is what decides here.
        rs::InputItem::EasyMessage(m) => match m.role {
            rs::Role::Assistant => ReasoningFollower::SameTurn,
            _ => ReasoningFollower::Foreign,
        },
        rs::InputItem::Item(rs::Item::Message(rs::MessageItem::Output(_))) => {
            ReasoningFollower::SameTurn
        }
        // Every item the model itself produces. `FunctionCall` and the three
        // backend-tool calls are what this conversion emits today; the rest
        // are listed because they are model output too, so a reasoning item
        // in front of one is attached, not dangling.
        rs::InputItem::Item(
            rs::Item::FunctionCall(_)
            | rs::Item::WebSearchCall(_)
            | rs::Item::CustomToolCall(_)
            | rs::Item::CodeInterpreterCall(_)
            | rs::Item::FileSearchCall(_)
            | rs::Item::ComputerCall(_)
            | rs::Item::ImageGenerationCall(_)
            | rs::Item::LocalShellCall(_)
            | rs::Item::ShellCall(_)
            | rs::Item::ApplyPatchCall(_)
            | rs::Item::McpCall(_)
            | rs::Item::McpApprovalRequest(_)
            | rs::Item::McpListTools(_),
        ) => ReasoningFollower::SameTurn,
        _ => ReasoningFollower::Foreign,
    }
}

/// Drop reasoning items that nothing in the emitted `input[]` attaches to.
///
/// The Responses API accepts a `reasoning` item only when the item it
/// belongs to follows it; otherwise it 400s with `Item 'rs_...' of type
/// 'reasoning' was provided without its required following item.`
/// A persisted reasoning-only turn hits exactly that: the trailing
/// `AssistantItem` has empty content and no tool calls, so it emits nothing,
/// and the reasoning item is left with no owner — either at the very end of
/// `input[]` or right before the next user message.
///
/// The rule is enforced here, over the whole emitted sequence, rather than at
/// one call site, so the interleaved case (a reasoning-only turn in the
/// middle of a conversation) is covered by the same pass. Dropping is the
/// only conformant repair: the alternative — synthesizing an empty assistant
/// message as the owner — assumes the API accepts empty-content messages,
/// which is unverified, and it would also put words in the model's mouth.
///
/// A single backward pass suffices: walking from the end, `attached` records
/// whether the item just visited can host a reasoning item, and a reasoning
/// item that is kept is itself a valid host for the one before it (a run of
/// reasoning items therefore survives or drops together).
///
/// This is stable under the reordering `response_to_conversation_items`
/// already performs (it accumulates text and function calls into one
/// trailing assistant, so `[rs1, fc1, rs2, fc2]` replays as
/// `[rs1, rs2, fc1, fc2]`): the reasoning run still precedes the turn's
/// function calls, so both items are correctly kept.
fn drop_dangling_reasoning(items: &mut Vec<rs::InputItem>) {
    let mut keep = vec![true; items.len()];
    let mut attached = false;
    for (idx, item) in items.iter().enumerate().rev() {
        match reasoning_follower(item) {
            // When this one is dropped `attached` stays false, so the
            // reasoning items ahead of it in the same run drop as well.
            ReasoningFollower::Reasoning => keep[idx] = attached,
            ReasoningFollower::SameTurn => attached = true,
            ReasoningFollower::Foreign => attached = false,
        }
    }
    let mut keep = keep.into_iter();
    items.retain(|_| keep.next().unwrap_or(true));
}

/// Whether a reasoning item can legally appear in a Responses API `input[]`.
///
/// [`rs::ReasoningItem::id`] is a bare `String` with no `skip_serializing_if`
/// — unlike `content`, `encrypted_content` and `status`, which are all
/// omitted when absent. An item with an empty id therefore goes on the wire
/// as `"id": ""`, and OpenAI-shaped endpoints reject the whole request with
/// a 400. There is nothing to repair: the id names server-side state, so no
/// value the client could invent would resolve.
///
/// An empty id marks exactly the items synthesized off a stream that is not
/// the Responses API — chat completions `reasoning_content` (via
/// [`synthesized_reasoning_item`]) and Anthropic Messages `thinking` blocks,
/// neither protocol carrying a stable upstream item id — plus the streaming
/// fallback in [`inject_streaming_reasoning_fallback`] for a Responses turn
/// that emitted no reasoning item of its own. Those items remain legal on
/// their own backends' wires; [`conversation_to_chat_messages`] still folds
/// them into text and the Messages conversion still emits them as `thinking`
/// blocks. Only this conversion has to refuse them.
///
/// Dropping them is what lets a session survive a mid-conversation switch to
/// a model on a different `api_backend`: the replayed history would otherwise
/// carry a reasoning item that the new endpoint rejects, failing the first
/// turn after the switch.
///
/// Note this is a validity check, not a provenance check. A well-formed item
/// whose `encrypted_content` was produced by a different backend (an
/// Anthropic thinking signature, say) is indistinguishable here from a native
/// one — [`ConversationItem::Reasoning`] wraps [`rs::ReasoningItem`] bare, and
/// neither type records which backend wrote it. In practice both non-Responses
/// synthesizers leave the id empty, so such items are caught anyway; a
/// principled fix for the general case needs a provenance field this data
/// model does not have.
fn reasoning_item_is_wire_legal(item: &rs::ReasoningItem) -> bool {
    !item.id.is_empty()
}

/// The reasoning item id a provider named in an encrypted-content rejection,
/// e.g. `The encrypted content for item rs_abc123 could not be verified.`
///
/// `rs_` is the Responses API's own prefix for a reasoning item, not a
/// provider-specific spelling, so the scan keys on it rather than on the
/// sentence around it. `None` when the message names no item — the caller
/// then repairs the history on the blobs alone.
pub fn encrypted_content_item_id(message: &str) -> Option<&str> {
    message
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .find(|token| token.len() > 3 && token.starts_with("rs_"))
}

/// Drop reasoning items whose `encrypted_content` the endpoint on the other
/// end cannot verify, plus `named_id` if the rejection named one. Returns how
/// many items were removed.
///
/// Encrypted reasoning is bound to whoever minted it: replaying an item to a
/// different provider, account or deployment answers
/// `400 ... could not be decrypted or parsed`, and the request fails
/// identically on every retry because the payload is unchanged. It happens
/// whenever a conversation outlives its issuer — a `model_fallbacks` hop, a
/// re-pointed `base_url`, a rotated account, a resumed session.
///
/// Everything carrying a blob goes, not only the named item, because
/// [`ConversationItem::Reasoning`] records no issuer (the same gap
/// [`reasoning_item_is_wire_legal`] documents): with no way to tell whose
/// blob is whose, the named item only proves the history holds foreign ones.
/// Dropping just that one would cost a full round trip per stale item — the
/// provider reports them one at a time — and each round trip is a failed
/// turn the user waits through. What is lost is the model's own reasoning
/// trace, never user-visible content or tool results; the alternative on
/// offer was discarding the whole session.
pub fn drop_unverifiable_reasoning(
    items: &mut Vec<ConversationItem>,
    named_id: Option<&str>,
) -> usize {
    let before = items.len();
    items.retain(|item| match item {
        ConversationItem::Reasoning(r) => {
            r.encrypted_content.is_none() && Some(r.id.as_str()) != named_id
        }
        _ => true,
    });
    before - items.len()
}

/// Inject the `type: "reasoning_text"` discriminator the API requires.
/// `async-openai`'s `ReasoningTextContent` has no `type` field, so it
/// serializes to `{"text": ...}` and the API answers 400. Delete this once
/// upstream grows the field.
pub fn patch_reasoning_text_types(body: &mut serde_json::Value) {
    let Some(input) = body.get_mut("input").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for item in input.iter_mut() {
        if item.get("type").and_then(|t| t.as_str()) != Some("reasoning") {
            continue;
        }
        let Some(content) = item.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        for c in content.iter_mut() {
            if let Some(obj) = c.as_object_mut() {
                obj.entry("type")
                    .or_insert_with(|| serde_json::Value::String("reasoning_text".into()));
            }
        }
    }
}

fn conversation_item_to_input_items(item: &ConversationItem) -> Vec<rs::InputItem> {
    match item {
        ConversationItem::System(s) => {
            vec![rs::InputItem::EasyMessage(rs::EasyInputMessage {
                r#type: rs::MessageType::Message,
                role: rs::Role::System,
                content: rs::EasyInputContent::Text(s.content.as_ref().to_owned()),
            })]
        }
        ConversationItem::User(u) => {
            let content = content_parts_to_easy_input_content(&u.content);
            vec![rs::InputItem::EasyMessage(rs::EasyInputMessage {
                r#type: rs::MessageType::Message,
                role: rs::Role::User,
                content,
            })]
        }
        ConversationItem::Reasoning(r) => {
            // Reasoning items round-trip in their native typed form — but only
            // the ones that can be legal there. See
            // [`reasoning_item_is_wire_legal`].
            if !reasoning_item_is_wire_legal(r) {
                return Vec::new();
            }
            // `status` is output-only and rejected on input.
            let mut r = r.clone();
            r.status = None;
            vec![rs::InputItem::Item(rs::Item::Reasoning(r))]
        }
        ConversationItem::Assistant(a) => {
            let mut items = Vec::new();

            if !a.content.is_empty() {
                items.push(rs::InputItem::EasyMessage(rs::EasyInputMessage {
                    r#type: rs::MessageType::Message,
                    role: rs::Role::Assistant,
                    content: rs::EasyInputContent::Text(a.content.as_ref().to_owned()),
                }));
            }

            for tc in &a.tool_calls {
                let arguments = sanitize_tool_arguments(&tc.id, &tc.name, tc.arguments.clone());
                items.push(rs::InputItem::Item(rs::Item::FunctionCall(
                    rs::FunctionToolCall {
                        call_id: tc.id.as_ref().to_owned(),
                        name: tc.name.clone(),
                        arguments: arguments.as_ref().to_owned(),
                        id: None,
                        status: None,
                    },
                )));
            }

            items
        }
        ConversationItem::ToolResult(t) => {
            let output = if t.images.is_empty() {
                rs::FunctionCallOutput::Text(t.content.as_ref().to_owned())
            } else {
                let mut parts: Vec<rs::InputContent> =
                    vec![rs::InputContent::InputText(rs::InputTextContent {
                        text: t.content.as_ref().to_owned(),
                    })];
                for img in &t.images {
                    if let ContentPart::Image { url } = img {
                        parts.push(rs::InputContent::InputImage(rs::InputImageContent {
                            detail: rs::ImageDetail::Auto,
                            file_id: None,
                            image_url: Some(url.as_ref().to_owned()),
                        }));
                    }
                }
                rs::FunctionCallOutput::Content(parts)
            };
            vec![rs::InputItem::Item(rs::Item::FunctionCallOutput(
                rs::FunctionCallOutputItemParam {
                    call_id: t.tool_call_id.clone(),
                    output,
                    id: None,
                    status: None,
                },
            ))]
        }
        ConversationItem::BackendToolCall(b) => {
            vec![match &b.kind {
                BackendToolKind::WebSearch(ws) => {
                    rs::InputItem::Item(rs::Item::WebSearchCall(ws.clone()))
                }
                BackendToolKind::XSearch(ct) => {
                    rs::InputItem::Item(rs::Item::CustomToolCall(ct.clone()))
                }
                BackendToolKind::CodeInterpreter(ci) => {
                    rs::InputItem::Item(rs::Item::CodeInterpreterCall(ci.clone()))
                }
            }]
        }
    }
}

fn content_parts_to_easy_input_content(parts: &[ContentPart]) -> rs::EasyInputContent {
    if parts.len() == 1
        && let ContentPart::Text { text } = &parts[0]
    {
        return rs::EasyInputContent::Text(text.as_ref().to_owned());
    }

    let items: Vec<rs::InputContent> = parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => rs::InputContent::InputText(rs::InputTextContent {
                text: text.as_ref().to_owned(),
            }),
            ContentPart::Image { url } => rs::InputContent::InputImage(rs::InputImageContent {
                image_url: Some(url.as_ref().to_owned()),
                file_id: None,
                detail: rs::ImageDetail::default(),
            }),
        })
        .collect();

    rs::EasyInputContent::ContentList(items)
}

/// The request's client function tools. A function tool whose name collides with a backend-hosted
/// tool is dropped, because sending both is rejected as a duplicate, so the hosted tool wins.
///
/// No hosted tool is emitted here. Both ride the raw-JSON [`extra_tool_entries`] channel instead.
fn build_responses_tools(req: &ConversationRequest) -> Vec<rs::Tool> {
    let tools: Vec<rs::Tool> = req
        .tools
        .iter()
        .filter(|t| {
            let collides = req.hosted_tools.iter().any(|h| h.wire_name() == t.name);
            if collides {
                tracing::warn!(
                    tool = %t.name,
                    "dropping function tool that collides with a backend-hosted tool"
                );
            }
            !collides
        })
        .map(|t| {
            rs::Tool::Function(rs::FunctionTool {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: Some(t.parameters.clone()),
                strict: None,
            })
        })
        .collect();

    tools
}

/// Every hosted tool as a raw JSON entry, which the sampler client splices into the serialized
/// `tools` array. `x_search` rides this channel because it has no `rs::Tool` variant, and
/// `web_search` rides it because async_openai's `rs::WebSearchToolFilters` models only
/// `allowed_domains` and cannot carry `excluded_domains`. Emitting either as a typed `rs::Tool`
/// as well would send it twice, which the API rejects as a duplicate; the JSON built here is
/// byte-identical to the native `rs::Tool::WebSearch` for the no-filter and allowlist-only cases.
pub fn extra_tool_entries(hosted_tools: &[HostedTool]) -> Vec<serde_json::Value> {
    let mut entries = Vec::new();
    for tool in hosted_tools {
        match tool {
            HostedTool::WebSearch { options } => {
                entries.push(match options {
                    Some(o) => o.to_tool_entry(),
                    None => WebSearchOptions::default().to_tool_entry(),
                });
            }
            HostedTool::XSearch { options } => {
                entries.push(match options {
                    Some(o) => o.to_tool_entry(),
                    None => XSearchOptions::default().to_tool_entry(),
                });
            }
        }
    }
    entries
}
