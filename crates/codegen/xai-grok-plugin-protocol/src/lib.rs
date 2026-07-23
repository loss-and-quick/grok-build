//! Wire-contract DTOs for the grok-build TypeScript plugin sidecar protocol.
//!
//! Transport is bidirectional JSON-RPC 2.0 over stdio (newline-delimited compact
//! JSON). This crate is the *only* surface plugins see; core internals refactor
//! freely behind it. Every type derives `ts_rs::TS` and exports to
//! `sdk/plugin/src/generated/` via `cargo test` (see `bindings_export`).
//!
//! Evolution is additive-only: new methods/fields/events never break older
//! plugins. Optional fields are `Option<_>` + `#[serde(default)]`; no type uses
//! `deny_unknown_fields`. Wire naming is snake_case. See the wire-contract spec.
//!
//! `serde_json::Value` fields carry opaque payloads; on the TS side they surface
//! as `unknown` (via `#[ts(type = "unknown")]`) rather than pulling ts-rs's
//! `JsonValue` shim into the export tree.
//!
//! Hook payloads are typed per event: each `xai-grok-hooks::event::HookPayload`
//! variant has a matching `*Payload` DTO here (see the per-event section below),
//! and the SDK keys them by `EventName` so a handler receives the payload typed
//! to its event rather than a bare `unknown`. Only the genuinely opaque inner
//! fields — tool input/result, the request body — stay `serde_json::Value` /
//! `unknown`, since their shape is the tool's or provider's, not this contract's.
//! The `hook_invoke` transport (`HookInvokeParams::payload`) still carries an
//! `unknown` value on the wire; the typing is a compile-time SDK convenience over
//! that same envelope, proven against the source `HookPayload` by the
//! plugin-host drift guard.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Wire protocol version. Bumped only on breaking (non-additive) changes; a
/// major mismatch makes the host log and disable the plugin, never crash the
/// session.
pub const PROTOCOL_VERSION: u32 = 1;

// Shared export destination for every binding: repo-root `sdk/plugin/src/generated/`.
// Path is relative to ts-rs's default base (`<crate>/bindings`), so four `..`
// climb crate → codegen → crates → repo root. `cargo test` sets CWD to the crate.

// ─────────────────────────────────────────────────────────────────────────────
// Shared vocabulary
// ─────────────────────────────────────────────────────────────────────────────

/// Gate semantics of an event, mirroring `xai-grok-hooks::GateKind` plus the
/// plugin-only `Replace`/`Intercept` seams. Shared vocabulary, both directions.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum GateKindDto {
    Observe,
    Tool,
    Stop,
    Replace,
    Intercept,
}

/// Tool-gate verdict returned by a plugin. Shared vocabulary, plugin→core,
/// from `pre_tool_use` onward.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum DecisionDto {
    Allow,
    Deny,
}

/// Severity of a `log_emit` line. Shared vocabulary, plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum LogLevelDto {
    Debug,
    Info,
    Warn,
    Error,
}

/// The v1 event dictionary a plugin may subscribe to. Shared vocabulary, both
/// directions. The first 15 are live (bridged from the hook dispatcher); the
/// reserved names have fixed gates but no seam until they are wired. Subscribing
/// to a seam-less event is valid — it simply never fires; status is visible in
/// the UI.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum EventName {
    // Live: the 15 events bridged from `xai-grok-hooks`.
    SessionStart,
    SessionEnd,
    Stop,
    StopFailure,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionDenied,
    UserPromptSubmit,
    Notification,
    SubagentStart,
    SubagentStop,
    /// Alias of `SubagentStop`; kept distinct on the wire (`subagent_end`) for
    /// third-party compatibility.
    SubagentEnd,
    PreCompact,
    PostCompact,

    // Reserved (gates fixed; host replies `method_not_found` until wired).
    /// Replace gate; intercepts the outgoing LLM request (incl. system prompt).
    ProviderRequest,
    /// Replace gate; rewrites the names of tool calls in a model response before
    /// the shell dispatches them. Reply shape: `{ toolCalls: [{ id, name }] }`.
    ProviderResponse,
    /// Replace gate; provider failure → retry (model/base_url alias) or fail.
    ProviderError,
    /// Replace gate; resolves a subagent spec before spawn.
    SubagentResolve,
    /// Tool gate; permission prompt seam.
    PermissionAsk,

    // Reserved — auth/OAuth.
    /// Replace gate; supplies a credential on demand.
    ResolveCredential,
    /// Replace gate; refreshes an expiring credential.
    RefreshCredential,
    /// Intercept gate; plugin drives the browser/PKCE OAuth flow.
    StartOauthFlow,
}

impl EventName {
    /// The stable snake_case wire token (identical to the serde form).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::Stop => "stop",
            Self::StopFailure => "stop_failure",
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::PostToolUseFailure => "post_tool_use_failure",
            Self::PermissionDenied => "permission_denied",
            Self::UserPromptSubmit => "user_prompt_submit",
            Self::Notification => "notification",
            Self::SubagentStart => "subagent_start",
            Self::SubagentStop => "subagent_stop",
            Self::SubagentEnd => "subagent_end",
            Self::PreCompact => "pre_compact",
            Self::PostCompact => "post_compact",
            Self::ProviderRequest => "provider_request",
            Self::ProviderResponse => "provider_response",
            Self::ProviderError => "provider_error",
            Self::SubagentResolve => "subagent_resolve",
            Self::PermissionAsk => "permission_ask",
            Self::ResolveCredential => "resolve_credential",
            Self::RefreshCredential => "refresh_credential",
            Self::StartOauthFlow => "start_oauth_flow",
        }
    }
}

impl std::fmt::Display for EventName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-event hook payloads (core→plugin)
//
// One typed struct per `xai-grok-hooks::event::HookPayload` variant, mirroring
// that variant's wire shape byte-for-byte. `HookPayload` is `#[serde(untagged)]`
// and flattened into the hook envelope; each variant renames its fields to
// camelCase, so — unlike the snake_case RPC vocabulary above — these payload
// DTOs carry `#[serde(rename_all = "camelCase")]` to reproduce the hook wire
// exactly. The SDK keys them by `EventName` (see `HookPayloadMap` in
// `sdk/plugin/src/define.ts`) so a handler receives the payload typed to its
// event. Inner `serde_json::Value` fields (tool input/result, request body)
// stay `unknown`: they are opaque, tool- and request-shaped, not part of this
// contract. The plugin-host drift guard proves these stay identical to the
// source `HookPayload`; a rename here fails that assert, a new variant there
// fails to compile.
//
// Optional fields keep `skip_serializing_if = "Option::is_none"` (so the wire
// omits absent fields exactly as the source does) plus `#[serde(default)]` for
// forward-tolerant deserialization; ts-rs renders them `field?: T | null`.
// ─────────────────────────────────────────────────────────────────────────────

/// Mirror of `xai-grok-hooks::event::SubagentStopPhase`. `SubagentStop` fire
/// phase; lowercase on the wire.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum SubagentStopPhaseDto {
    Gate,
    Observe,
}

/// Mirror of `xai-grok-hooks::event::BackgroundTaskType`. snake_case wire.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum BackgroundTaskTypeDto {
    Shell,
    Monitor,
    Subagent,
}

/// Mirror of `xai-grok-hooks::event::StopFailureKind`. snake_case wire.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum StopFailureKindDto {
    RateLimit,
    AuthenticationFailed,
    InvalidRequest,
    ServerError,
    MaxOutputTokens,
    Unknown,
}

/// Mirror of `xai-grok-hooks::event::StopBackgroundTask`: one in-flight
/// background task in a `Stop` payload. camelCase wire (`type`, `agentType`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StopBackgroundTaskDto {
    pub id: String,
    pub r#type: BackgroundTaskTypeDto,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// Mirror of `xai-grok-hooks::event::StopSessionCron`: one session-scoped
/// scheduled wakeup in a `Stop` payload. camelCase wire.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StopSessionCronDto {
    pub id: String,
    pub schedule: String,
    pub recurring: bool,
    pub prompt: String,
}

/// Mirror of `xai-grok-hooks::event::ProviderResponseToolCall`: one tool call
/// in a `ProviderResponse` payload. Both field names are already the wire
/// tokens, so no rename.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ProviderResponseToolCallDto {
    pub id: String,
    pub name: String,
}

/// `session_start` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct SessionStartPayload {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// `session_end` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct SessionEndPayload {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional = nullable)]
    pub turn_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional = nullable)]
    pub tool_call_count: Option<u64>,
}

/// `stop` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StopPayload {
    pub reason: String,
    pub stop_hook_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_tasks: Option<Vec<StopBackgroundTaskDto>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_crons: Option<Vec<StopSessionCronDto>>,
}

/// `stop_failure` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StopFailurePayload {
    pub error: StopFailureKindDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
}

/// `pre_tool_use` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PreToolUsePayload {
    pub tool_name: String,
    pub tool_use_id: String,
    #[ts(type = "unknown")]
    pub tool_input: serde_json::Value,
    pub tool_input_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
}

/// `post_tool_use` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PostToolUsePayload {
    pub tool_name: String,
    pub tool_use_id: String,
    #[ts(type = "unknown")]
    pub tool_input: serde_json::Value,
    #[ts(type = "unknown")]
    pub tool_result: serde_json::Value,
    pub tool_input_truncated: bool,
    pub tool_result_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional = nullable)]
    pub duration_ms: Option<u64>,
    pub is_backgrounded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
}

/// `post_tool_use_failure` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PostToolUseFailurePayload {
    pub tool_name: String,
    pub tool_use_id: String,
    #[ts(type = "unknown")]
    pub tool_input: serde_json::Value,
    pub tool_input_truncated: bool,
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
}

/// `permission_denied` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PermissionDeniedPayload {
    pub tool_name: String,
    pub tool_use_id: String,
    #[ts(type = "unknown")]
    pub tool_input: serde_json::Value,
    pub tool_input_truncated: bool,
}

/// `user_prompt_submit` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct UserPromptSubmitPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// `notification` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct NotificationPayload {
    pub notification_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

/// `subagent_start` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct SubagentStartPayload {
    pub subagent_id: String,
    pub subagent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `subagent_stop` payload (also the payload of the `subagent_end` alias).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct SubagentStopPayload {
    pub phase: SubagentStopPhaseDto,
    pub subagent_id: String,
    pub subagent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_hook_active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
}

/// `pre_compact` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PreCompactPayload {
    pub source: String,
}

/// `post_compact` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PostCompactPayload {
    pub source: String,
}

/// `provider_request` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ProviderRequestPayload {
    pub endpoint: String,
    pub model: String,
    pub base_url_alias: String,
    pub agent: String,
    pub tools: Vec<String>,
    #[ts(type = "Array<[string, string]>")]
    pub headers: Vec<(String, String)>,
    #[ts(type = "unknown")]
    pub body: serde_json::Value,
}

/// `provider_response` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ProviderResponsePayload {
    pub base_url: String,
    pub endpoint: String,
    pub tool_calls: Vec<ProviderResponseToolCallDto>,
}

/// `provider_error` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ProviderErrorPayload {
    pub error_class: String,
    pub model: String,
    #[ts(type = "number")]
    pub attempt: u32,
    pub base_url_alias: String,
}

/// `subagent_resolve` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct SubagentResolvePayload {
    pub subagent_id: String,
    pub subagent_type: String,
    pub description: String,
    pub prompt_preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub parent_model: String,
}

/// `resolve_credential` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ResolveCredentialPayload {
    pub reason: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_hint: Option<String>,
}

/// `refresh_credential` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct RefreshCredentialPayload {
    pub reason: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
}

/// `start_oauth_flow` payload.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StartOauthFlowPayload {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_hint: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// initialize (core→plugin request, handshake)
// ─────────────────────────────────────────────────────────────────────────────

/// Host abilities advertised to the plugin at handshake. Part of `initialize`,
/// core→plugin. `leader_socket` is the session leader's Unix-socket path when
/// the host process runs in leader mode (also exported to the sidecar's env as
/// `GROK_LEADER_SOCKET`): a plugin may connect to it as one more headless ACP
/// client. `None` outside leader mode.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct HostCapabilities {
    pub storage: bool,
    #[serde(default)]
    pub leader_socket: Option<String>,
}

/// `initialize` request params. Core→plugin, handshake.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub plugin_name: String,
    #[ts(type = "unknown")]
    pub plugin_config: serde_json::Value,
    pub workspace_root: String,
    pub session_id: String,
    pub capabilities: HostCapabilities,
}

/// `initialize` reply. Plugin→core, handshake. `subscriptions` are event
/// names from the dictionary; `plugin_version` is informational. `tools` are
/// the tool handlers the plugin's code registered — informational only: the
/// manifest's `tools` array is what populates the model-facing catalog (the
/// catalog is built before any sidecar starts), and the host warns when the
/// two drift.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct InitializeResult {
    pub protocol_version: u32,
    #[serde(default)]
    pub subscriptions: Vec<String>,
    #[serde(default)]
    pub plugin_version: Option<String>,
    #[serde(default)]
    pub tools: Vec<ToolDescriptorDto>,
}

// ─────────────────────────────────────────────────────────────────────────────
// hook_invoke (core→plugin request)
// ─────────────────────────────────────────────────────────────────────────────

/// `hook_invoke` request params. Core→plugin. `timeout_ms` bounds the
/// plugin's reply (fail-open on timeout).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct HookInvokeParams {
    pub invocation_id: String,
    pub event: String,
    pub gate: GateKindDto,
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
    // JSON number on the wire; ts-rs would otherwise map u64 to `bigint`.
    #[ts(type = "number")]
    pub timeout_ms: u64,
}

/// `hook_invoke` reply, shaped by the event's gate. Plugin→core.
/// Internally tagged on `kind`. The `Stop` variant's `continue_` field is
/// `continue` on the wire.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub enum HookInvokeResult {
    /// Observe gate: acknowledged, no control.
    Observed,
    /// Tool gate: allow/deny with an optional reason.
    Decision {
        decision: DecisionDto,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Stop gate: block the stop and/or steer continuation.
    Stop {
        block: bool,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default, rename = "continue")]
        continue_: Option<bool>,
        #[serde(default)]
        additional_context: Option<String>,
    },
    /// Replace gate: substitute the payload, or `None` to pass the original through.
    Replace {
        #[serde(default)]
        #[ts(type = "unknown", optional = nullable)]
        payload: Option<serde_json::Value>,
    },
}

/// A credential a plugin supplies to the core. Returned as the `Replace`
/// payload of `resolve_credential` / `refresh_credential`, and as the
/// `start_oauth_flow` (Intercept) outcome. The core stores the bearer, masks it
/// in logs, and mirrors the metadata onto outbound requests. Shared vocabulary,
/// plugin→core.
///
/// The masking of outbound requests (rewriting the credential onto the wire) is
/// handled by the existing `provider_request` seam, not by this type.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct PluginCredentialDto {
    /// The bearer token to send on outbound requests.
    pub token: String,
    /// Whether the token-auth marker header accompanies the bearer. Defaults to
    /// `true`; set `false` for bare-bearer credentials.
    #[serde(default = "default_true")]
    pub needs_token_auth_header: bool,
    /// Absolute expiry as a Unix-epoch millisecond timestamp; `None` for a
    /// credential with no known expiry (the core will not pre-emptively refresh).
    #[serde(default)]
    #[ts(type = "number | null", optional = nullable)]
    pub expires_at_ms: Option<i64>,
    /// Stable identifier of the credential's owner (account/subject id), echoed
    /// back on a later `refresh_credential`; `None` when the plugin has none.
    #[serde(default)]
    pub owner_id: Option<String>,
}

fn default_true() -> bool {
    true
}

// ─────────────────────────────────────────────────────────────────────────────
// tool_invoke (core→plugin request) — model-visible plugin tools.
//
// A plugin's manifest declares tools (name, description, input schema); the
// shell registers them in the session tool catalog under the MCP-style
// qualified name `<plugin>__<tool>`, so the model calls them like any other
// tool (permissions and pre/post_tool_use hooks apply on the normal dispatch
// path). Execution is this RPC: the host forwards the call to the sidecar,
// which runs the matching handler with the full plugin context (storage,
// agents, config, log) plus the per-call context below.
// ─────────────────────────────────────────────────────────────────────────────

/// One tool a plugin offers to the model. Shared vocabulary: the manifest's
/// `tools` array parses into this shape, and `initialize` replies carry the
/// code-registered handlers for drift warnings.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ToolDescriptorDto {
    /// Bare tool name (no plugin prefix); the host namespaces it for the model.
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's input.
    #[ts(type = "unknown")]
    pub input_schema: serde_json::Value,
}

/// Per-call context forwarded with every `tool_invoke`. Core→plugin.
/// `agent` names the caller: `"main"` for the root session, otherwise the
/// subagent type label.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ToolCallContextDto {
    pub session_id: String,
    /// Working directory the call runs in (per-call, not session-static).
    pub cwd: String,
    pub agent: String,
}

/// `tool_invoke` request params. Core→plugin. `tool` is the bare name as
/// declared; `timeout_ms` is the host's hard deadline (informational to the
/// plugin — the host enforces it).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ToolInvokeParams {
    pub invocation_id: String,
    pub tool: String,
    #[ts(type = "unknown")]
    pub arguments: serde_json::Value,
    pub context: ToolCallContextDto,
    #[ts(type = "number")]
    pub timeout_ms: u64,
}

/// `tool_invoke` reply. Plugin→core. `content` is the tool result text
/// returned to the conversation; `is_error` marks it as a failed call
/// (surfaced to the model exactly like an MCP tool error).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ToolInvokeResult {
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// `tool_cancel` notification params. Core→plugin. Fired when an in-flight
/// `tool_invoke` is abandoned — the parent turn was aborted (Esc) while the
/// session stays alive — so the plugin can wind down invocation-scoped work
/// (e.g. cancel the subagents it spawned). `invocation_id` matches the
/// [`ToolInvokeParams::invocation_id`] of the call being cancelled; the SDK
/// aborts that handler's `AbortSignal`. Best-effort: the host does not wait
/// for a reply and the tool result (if any) is discarded.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ToolCancelParams {
    pub invocation_id: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// shutdown (core→plugin notification)
// ─────────────────────────────────────────────────────────────────────────────

/// `shutdown` notification params. Core→plugin. The plugin must exit
/// within ~2s or it is SIGKILLed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ShutdownParams {
    pub reason: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// log_emit (plugin→core notification)
// ─────────────────────────────────────────────────────────────────────────────

/// `log_emit` notification params. Plugin→core. `fields` is optional
/// structured context.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct LogEmitParams {
    pub level: LogLevelDto,
    pub message: String,
    #[serde(default)]
    #[ts(type = "unknown", optional = nullable)]
    pub fields: Option<serde_json::Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// storage_* (plugin→core requests) — per-plugin namespace, core locks.
// ─────────────────────────────────────────────────────────────────────────────

/// `storage_get` request params. Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StorageGetParams {
    pub key: String,
}

/// `storage_get` reply. Plugin→core. `value` is `None` when absent.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct StorageGetResult {
    #[serde(default)]
    #[ts(type = "unknown", optional = nullable)]
    pub value: Option<serde_json::Value>,
}

/// `storage_set` request params. Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StorageSetParams {
    pub key: String,
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
}

/// `storage_set` reply (empty). Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StorageSetResult {}

/// `storage_delete` request params. Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StorageDeleteParams {
    pub key: String,
}

/// `storage_delete` reply. Plugin→core. `existed` reports whether a
/// value was present.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StorageDeleteResult {
    pub existed: bool,
}

/// `storage_list` request params. Plugin→core. `prefix` filters keys.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct StorageListParams {
    #[serde(default)]
    pub prefix: Option<String>,
}

/// `storage_list` reply. Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct StorageListResult {
    #[serde(default)]
    pub keys: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// config_get (plugin→core request)
// ─────────────────────────────────────────────────────────────────────────────

/// `config_get` request params (empty). Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ConfigGetParams {}

/// `config_get` reply. Plugin→core. `value` is the plugin config from
/// the manifest/settings.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct ConfigGetResult {
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
}

// ─────────────────────────────────────────────────────────────────────────────
// agent_* (plugin→core requests) — subagent orchestration.
//
// A plugin spawns real children of its session (they route through the same
// coordinator as model-initiated Task spawns and are visible in the TUI like
// any other subagent). Progress delivery is cursor-based polling
// (`agent_events`), not server→plugin notifications: the capability server is
// transport-agnostic request/reply, and poll state survives sidecar restarts.
// ─────────────────────────────────────────────────────────────────────────────

/// `agent_spawn` request params. Plugin→core. `agent_type` defaults to
/// `general-purpose`; `timeout_ms` (per-spawn) auto-cancels the subagent when
/// it has not finished in time. The spawn is validated exactly like a
/// model-initiated Task call (type allow-list, toggle, model catalog);
/// validation failures surface as the terminal result of `agent_wait`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct AgentSpawnParams {
    #[serde(default)]
    pub agent_type: Option<String>,
    pub prompt: String,
    /// Short human-readable description shown in the TUI. Defaults to a
    /// plugin-derived label.
    #[serde(default)]
    pub description: Option<String>,
    /// Model override, validated against the model catalog.
    #[serde(default)]
    pub model: Option<String>,
    /// Working directory for the child session (defaults to the parent's).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Per-spawn budget: when set, the host cancels the subagent after this
    /// many milliseconds and reports a `cancelled` terminal result.
    #[serde(default)]
    #[ts(type = "number | null", optional = nullable)]
    pub timeout_ms: Option<u64>,
}

/// `agent_spawn` reply. Plugin→core. `id` keys every other `agent_*` call.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct AgentSpawnResult {
    pub id: String,
}

/// Subagent lifecycle status. Shared vocabulary, plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum AgentStatusDto {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// `agent_wait` request params. Plugin→core. `timeout_ms` defaults to 30 000;
/// on timeout the reply carries `status: running` (poll again or cancel).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct AgentWaitParams {
    pub id: String,
    #[serde(default)]
    #[ts(type = "number | null", optional = nullable)]
    pub timeout_ms: Option<u64>,
}

/// `agent_wait` reply. Plugin→core. Terminal when `status != running`:
/// `output`/`error` and the usage counters are then populated inline.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct AgentWaitResult {
    pub status: AgentStatusDto,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[ts(type = "number")]
    pub tokens_used: u64,
    #[ts(type = "number")]
    pub duration_ms: u64,
    pub tool_calls: u32,
    pub turns: u32,
}

/// `agent_events` request params. Plugin→core. Cursor-based long-poll:
/// `cursor` is the first sequence number the caller has not seen (start at 0);
/// `timeout_ms` (default 0 = reply immediately) bounds how long the host may
/// hold the request open waiting for a new event.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct AgentEventsParams {
    pub id: String,
    #[serde(default)]
    #[ts(type = "number")]
    pub cursor: u64,
    #[serde(default)]
    #[ts(type = "number | null", optional = nullable)]
    pub timeout_ms: Option<u64>,
}

/// Kind of one subagent progress event. Shared vocabulary, plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum AgentEventKindDto {
    /// The spawn was accepted (seq 0; `data` carries the resolved request).
    Spawned,
    /// Live counters changed (`data`: turns, tool_calls, tokens_used, …).
    Progress,
    /// Terminal (`data`: the same summary `agent_wait` returns).
    Completed,
    Failed,
    Cancelled,
}

/// One subagent progress event. Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct AgentEventDto {
    #[ts(type = "number")]
    pub seq: u64,
    pub kind: AgentEventKindDto,
    #[ts(type = "unknown")]
    pub data: serde_json::Value,
}

/// `agent_events` reply. Plugin→core. `next_cursor` is the cursor for the
/// next call; `done` is set once a terminal event exists (stop polling). The
/// per-subagent buffer is capped: a slow consumer may observe a seq gap
/// (oldest progress events dropped first).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct AgentEventsResult {
    #[serde(default)]
    pub events: Vec<AgentEventDto>,
    #[ts(type = "number")]
    pub next_cursor: u64,
    pub done: bool,
}

/// `agent_list` request params (empty). Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct AgentListParams {}

/// One spawnable agent type, with the metadata a plugin needs to present a
/// "who's who" table in a multi-agent scenario. `description` mirrors the
/// agent's `.md` frontmatter; `model` is the agent's explicit model override
/// (absent when the agent inherits the parent session's model).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct AgentDescriptorDto {
    /// The spawnable type name (qualified `plugin:agent` for plugin agents).
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Explicit model id; omitted when the agent inherits the session's model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// `agent_list` reply. Plugin→core. Spawnable agent types for this session
/// (sorted; filtered by config toggles), each with name/description/model.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct AgentListResult {
    #[serde(default)]
    pub agents: Vec<AgentDescriptorDto>,
}

/// `agent_cancel` request params. Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct AgentCancelParams {
    pub id: String,
}

/// `agent_cancel` outcome. Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum AgentCancelOutcomeDto {
    Cancelled,
    AlreadyFinished,
    NotFound,
}

/// `agent_cancel` reply. Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct AgentCancelResult {
    pub outcome: AgentCancelOutcomeDto,
}

/// `agent_send` request params. Plugin→core. Continues a prior subagent with a
/// follow-up `prompt`: `id` is a terminal subagent this plugin spawned, whose
/// conversation (raw transcript, tool state, model) is resumed into a fresh
/// child that runs `prompt` and produces the next terminal result. The child
/// is stateless-continued via resume — a new subagent with a new id — so the
/// full `agent_*` surface (`wait`/`events`/`cancel`, and `timeout_ms`) applies
/// to the returned id exactly as for `agent_spawn`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct AgentSendParams {
    /// A terminal subagent id previously returned by `agent_spawn`/`agent_send`
    /// for this plugin.
    pub id: String,
    /// The follow-up prompt for the resumed child.
    pub prompt: String,
    /// Per-spawn budget for the continuation, same semantics as
    /// [`AgentSpawnParams::timeout_ms`].
    #[serde(default)]
    #[ts(type = "number | null", optional = nullable)]
    pub timeout_ms: Option<u64>,
}

/// `agent_send` reply. Plugin→core. `id` is the NEW subagent id for the
/// continuation (the prior id stays terminal); key subsequent `agent_*` calls
/// on it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct AgentSendResult {
    pub id: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// ui_publish_panel / ui_close_panel (plugin→core requests) and panel_action
// (core→plugin notification) — the generic plugin UI panel surface.
//
// A plugin pushes a declarative [`PanelViewModel`] to the host; the pager renders
// it both as a full-screen overlay (opened with Ctrl+P) and as a compact sidebar
// widget. The pager owns all interaction state — Table row selection/scroll and
// button focus — so the view model is pure data, re-published wholesale on every
// change (latest-wins, keyed by `id`). When a button is activated the pager
// routes it back to the publishing plugin as a [`PanelActionParams`] notification.
//
// The host maps each `id` to the sidecar that published it; `id` is the plugin's
// own local string and carries no plugin identity of its own.
// ─────────────────────────────────────────────────────────────────────────────

/// Tone of a status chip, driving its colour in the pager. Shared vocabulary,
/// plugin→core. Defaults to `neutral` when a plugin omits it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum PanelTone {
    #[default]
    Neutral,
    Success,
    Warning,
    Error,
}

/// One key/value chip in a [`PanelBlock::Status`] block. Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PanelStatusItem {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub tone: PanelTone,
}

/// One button in a [`PanelBlock::Actions`] block. Plugin→core. `key` is an
/// optional single-character keybind the pager binds while the panel is focused;
/// activating any button routes [`PanelActionParams`] back to the plugin.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct PanelButton {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub key: Option<String>,
}

/// One block of panel content. Internally tagged on `kind` (mirrors
/// [`HookInvokeResult`]). Plugin→core. The pager renders every block of a panel
/// at once, top to bottom.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub enum PanelBlock {
    /// A row of status chips, each `label: value` coloured by its `tone`.
    Status { items: Vec<PanelStatusItem> },
    /// Markdown, rendered by the pager's own markdown renderer (headings,
    /// lists, tables, code — the same one used for model output).
    Markdown { text: String },
    /// A table the pager owns: it windows the `rows` and, when `selectable`,
    /// tracks the highlighted row with its own up/down navigation. The plugin
    /// supplies only the data.
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        #[serde(default)]
        selectable: bool,
    },
    /// A single-line editable field the pager owns. The plugin supplies the
    /// initial `value`; the pager tracks edits and, when a button is activated,
    /// returns the current field values (keyed by `id`) in the action's
    /// `inputs`. `secret` masks the field like a password. Enables flows such as
    /// pasting an OAuth authorization code back to the plugin.
    Input {
        id: String,
        label: String,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        value: Option<String>,
        #[serde(default)]
        secret: bool,
    },
    /// A row of focusable buttons; activation routes back to the plugin.
    Actions { buttons: Vec<PanelButton> },
}

/// A declarative UI panel a plugin publishes to the host. Plugin→core; this is
/// the `ui_publish_panel` request params. `id` is the plugin's own stable key
/// for the panel (re-publishing the same `id` replaces it, latest-wins).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PanelViewModel {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub blocks: Vec<PanelBlock>,
}

/// `ui_publish_panel` reply (empty). Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PanelPublishResult {}

/// `ui_close_panel` request params. Plugin→core. Removes the panel with this
/// `id`; no-op when unknown.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PanelCloseParams {
    pub id: String,
}

/// `ui_close_panel` reply (empty). Plugin→core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PanelCloseResult {}

/// `panel_action` notification params. Core→plugin. Fired when the user
/// activates a button in a panel this plugin published: `panel_id` is the
/// [`PanelViewModel::id`], `button_id` the [`PanelButton::id`]. `inputs` carries
/// the current value of every [`PanelBlock::Input`] field in the panel, keyed by
/// the field's `id` — so a button press delivers, say, a typed OAuth code
/// alongside the button. Best-effort, like `tool_cancel` — the host does not
/// wait for a reply.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/")]
pub struct PanelActionParams {
    pub panel_id: String,
    pub button_id: String,
    #[serde(default)]
    pub inputs: std::collections::BTreeMap<String, String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// ts-rs export
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod bindings_export {
    use super::*;

    /// Regenerate every binding into `sdk/plugin/src/generated/`. `#[ts(export)]`
    /// also emits a hidden per-type test; this is the single explicit entry
    /// point that fails loudly if any type cannot export.
    #[test]
    fn export_all_bindings() {
        let cfg = ts_rs::Config::from_env();
        macro_rules! export {
            ($($t:ty),+ $(,)?) => {$(
                <$t as TS>::export(&cfg)
                    .unwrap_or_else(|e| panic!("exporting {}: {e}", stringify!($t)));
            )+};
        }
        export!(
            GateKindDto,
            DecisionDto,
            LogLevelDto,
            EventName,
            SubagentStopPhaseDto,
            BackgroundTaskTypeDto,
            StopFailureKindDto,
            StopBackgroundTaskDto,
            StopSessionCronDto,
            ProviderResponseToolCallDto,
            SessionStartPayload,
            SessionEndPayload,
            StopPayload,
            StopFailurePayload,
            PreToolUsePayload,
            PostToolUsePayload,
            PostToolUseFailurePayload,
            PermissionDeniedPayload,
            UserPromptSubmitPayload,
            NotificationPayload,
            SubagentStartPayload,
            SubagentStopPayload,
            PreCompactPayload,
            PostCompactPayload,
            ProviderRequestPayload,
            ProviderResponsePayload,
            ProviderErrorPayload,
            SubagentResolvePayload,
            ResolveCredentialPayload,
            RefreshCredentialPayload,
            StartOauthFlowPayload,
            HostCapabilities,
            InitializeParams,
            InitializeResult,
            HookInvokeParams,
            HookInvokeResult,
            PluginCredentialDto,
            ToolDescriptorDto,
            ToolCallContextDto,
            ToolInvokeParams,
            ToolInvokeResult,
            ToolCancelParams,
            ShutdownParams,
            LogEmitParams,
            StorageGetParams,
            StorageGetResult,
            StorageSetParams,
            StorageSetResult,
            StorageDeleteParams,
            StorageDeleteResult,
            StorageListParams,
            StorageListResult,
            ConfigGetParams,
            ConfigGetResult,
            AgentSpawnParams,
            AgentSpawnResult,
            AgentStatusDto,
            AgentDescriptorDto,
            AgentWaitParams,
            AgentWaitResult,
            AgentEventsParams,
            AgentEventKindDto,
            AgentEventDto,
            AgentEventsResult,
            AgentListParams,
            AgentListResult,
            AgentCancelParams,
            AgentCancelOutcomeDto,
            AgentCancelResult,
            AgentSendParams,
            AgentSendResult,
            PanelTone,
            PanelStatusItem,
            PanelButton,
            PanelBlock,
            PanelViewModel,
            PanelPublishResult,
            PanelCloseParams,
            PanelCloseResult,
            PanelActionParams,
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Assert `value` serializes to `expected_json` and round-trips back equal.
    fn round_trip<T>(value: &T, expected_json: serde_json::Value)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let got = serde_json::to_value(value).expect("serialize");
        assert_eq!(got, expected_json, "wire shape mismatch");
        let back: T = serde_json::from_value(got).expect("deserialize");
        assert_eq!(&back, value, "round-trip mismatch");
    }

    #[test]
    fn initialize_round_trip() {
        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION,
            plugin_name: "council".into(),
            plugin_config: json!({ "k": 1 }),
            workspace_root: "/ws".into(),
            session_id: "sess-1".into(),
            capabilities: HostCapabilities {
                storage: true,
                leader_socket: None,
            },
        };
        round_trip(
            &params,
            json!({
                "protocol_version": 1,
                "plugin_name": "council",
                "plugin_config": { "k": 1 },
                "workspace_root": "/ws",
                "session_id": "sess-1",
                "capabilities": { "storage": true, "leader_socket": null },
            }),
        );

        round_trip(
            &InitializeResult {
                protocol_version: PROTOCOL_VERSION,
                subscriptions: vec!["session_start".into(), "pre_tool_use".into()],
                plugin_version: Some("0.2.0".into()),
                tools: vec![ToolDescriptorDto {
                    name: "echo".into(),
                    description: "echo back".into(),
                    input_schema: json!({ "type": "object" }),
                }],
            },
            json!({
                "protocol_version": 1,
                "subscriptions": ["session_start", "pre_tool_use"],
                "plugin_version": "0.2.0",
                "tools": [{
                    "name": "echo",
                    "description": "echo back",
                    "input_schema": { "type": "object" },
                }],
            }),
        );
    }

    /// Missing optional fields default cleanly (forward-compat: no
    /// `deny_unknown_fields`, `Option` + `#[serde(default)]`).
    #[test]
    fn initialize_result_tolerates_missing_and_unknown() {
        let r: InitializeResult =
            serde_json::from_value(json!({ "protocol_version": 1, "future_field": 42 }))
                .expect("deserialize with missing optionals + unknown field");
        assert_eq!(r.subscriptions, Vec::<String>::new());
        assert_eq!(r.plugin_version, None);
        assert_eq!(r.tools, Vec::<ToolDescriptorDto>::new());
    }

    #[test]
    fn tool_invoke_round_trip() {
        round_trip(
            &ToolInvokeParams {
                invocation_id: "tinv-1".into(),
                tool: "planner".into(),
                arguments: json!({ "question": "ship it?" }),
                context: ToolCallContextDto {
                    session_id: "sess-1".into(),
                    cwd: "/repo".into(),
                    agent: "main".into(),
                },
                timeout_ms: 120_000,
            },
            json!({
                "invocation_id": "tinv-1",
                "tool": "planner",
                "arguments": { "question": "ship it?" },
                "context": { "session_id": "sess-1", "cwd": "/repo", "agent": "main" },
                "timeout_ms": 120000,
            }),
        );

        round_trip(
            &ToolInvokeResult {
                content: "verdict: yes".into(),
                is_error: false,
            },
            json!({ "content": "verdict: yes", "is_error": false }),
        );

        // `is_error` defaults to false when a plugin omits it.
        let r: ToolInvokeResult = serde_json::from_value(json!({ "content": "ok" })).unwrap();
        assert!(!r.is_error);
        assert_eq!(r.content, "ok");

        round_trip(
            &ToolCancelParams {
                invocation_id: "tinv-1".into(),
            },
            json!({ "invocation_id": "tinv-1" }),
        );
    }

    #[test]
    fn plugin_credential_round_trip() {
        round_trip(
            &PluginCredentialDto {
                token: "bearer-xyz".into(),
                needs_token_auth_header: false,
                expires_at_ms: Some(1_700_000_000_000),
                owner_id: Some("acct-1".into()),
            },
            json!({
                "token": "bearer-xyz",
                "needs_token_auth_header": false,
                "expires_at_ms": 1_700_000_000_000_i64,
                "owner_id": "acct-1",
            }),
        );

        // Minimal shape: only `token` present. `needs_token_auth_header`
        // defaults to true; expiry/owner default to absent. Unknown future
        // fields tolerated (no deny_unknown_fields).
        let c: PluginCredentialDto =
            serde_json::from_value(json!({ "token": "t", "future": 1 })).unwrap();
        assert_eq!(c.token, "t");
        assert!(c.needs_token_auth_header);
        assert_eq!(c.expires_at_ms, None);
        assert_eq!(c.owner_id, None);
    }

    #[test]
    fn hook_invoke_params_round_trip() {
        round_trip(
            &HookInvokeParams {
                invocation_id: "inv-1".into(),
                event: "pre_tool_use".into(),
                gate: GateKindDto::Tool,
                payload: json!({ "tool": "bash" }),
                timeout_ms: 5000,
            },
            json!({
                "invocation_id": "inv-1",
                "event": "pre_tool_use",
                "gate": "tool",
                "payload": { "tool": "bash" },
                "timeout_ms": 5000,
            }),
        );
    }

    /// Every internally-tagged `HookInvokeResult` variant, including the wire
    /// `continue` field name on the `Stop` variant.
    #[test]
    fn hook_invoke_result_variants_round_trip() {
        round_trip(&HookInvokeResult::Observed, json!({ "kind": "observed" }));

        round_trip(
            &HookInvokeResult::Decision {
                decision: DecisionDto::Deny,
                reason: Some("nope".into()),
            },
            json!({ "kind": "decision", "decision": "deny", "reason": "nope" }),
        );

        round_trip(
            &HookInvokeResult::Stop {
                block: true,
                reason: Some("wait".into()),
                continue_: Some(false),
                additional_context: Some("ctx".into()),
            },
            json!({
                "kind": "stop",
                "block": true,
                "reason": "wait",
                "continue": false,
                "additional_context": "ctx",
            }),
        );

        round_trip(
            &HookInvokeResult::Replace { payload: None },
            json!({ "kind": "replace", "payload": null }),
        );
        round_trip(
            &HookInvokeResult::Replace {
                payload: Some(json!({ "swapped": true })),
            },
            json!({ "kind": "replace", "payload": { "swapped": true } }),
        );
    }

    /// The `Stop` result must use `continue` (not `continue_`) on the wire.
    #[test]
    fn stop_result_uses_continue_wire_name() {
        let s = HookInvokeResult::Stop {
            block: false,
            reason: None,
            continue_: Some(true),
            additional_context: None,
        };
        let text = serde_json::to_string(&s).expect("serialize");
        assert!(
            text.contains("\"continue\""),
            "expected wire `continue`: {text}"
        );
        assert!(
            !text.contains("continue_"),
            "leaked Rust field name: {text}"
        );
    }

    #[test]
    fn storage_and_config_round_trip() {
        round_trip(&StorageGetParams { key: "a".into() }, json!({ "key": "a" }));
        round_trip(
            &StorageGetResult {
                value: Some(json!(7)),
            },
            json!({ "value": 7 }),
        );
        round_trip(
            &StorageSetParams {
                key: "a".into(),
                value: json!("v"),
            },
            json!({ "key": "a", "value": "v" }),
        );
        round_trip(&StorageSetResult {}, json!({}));
        round_trip(
            &StorageDeleteParams { key: "a".into() },
            json!({ "key": "a" }),
        );
        round_trip(
            &StorageDeleteResult { existed: true },
            json!({ "existed": true }),
        );
        round_trip(
            &StorageListParams {
                prefix: Some("p/".into()),
            },
            json!({ "prefix": "p/" }),
        );
        round_trip(
            &StorageListResult {
                keys: vec!["p/1".into()],
            },
            json!({ "keys": ["p/1"] }),
        );
        round_trip(&ConfigGetParams {}, json!({}));
        round_trip(
            &ConfigGetResult {
                value: json!({ "on": true }),
            },
            json!({ "value": { "on": true } }),
        );
    }

    #[test]
    fn agent_rpc_round_trip() {
        round_trip(
            &AgentSpawnParams {
                agent_type: Some("explore".into()),
                prompt: "map the repo".into(),
                description: Some("mapper".into()),
                model: None,
                cwd: None,
                timeout_ms: Some(60_000),
            },
            json!({
                "agent_type": "explore",
                "prompt": "map the repo",
                "description": "mapper",
                "model": null,
                "cwd": null,
                "timeout_ms": 60000,
            }),
        );
        round_trip(&AgentSpawnResult { id: "a-1".into() }, json!({ "id": "a-1" }));

        round_trip(
            &AgentWaitParams {
                id: "a-1".into(),
                timeout_ms: None,
            },
            json!({ "id": "a-1", "timeout_ms": null }),
        );
        round_trip(
            &AgentWaitResult {
                status: AgentStatusDto::Completed,
                output: Some("done".into()),
                error: None,
                tokens_used: 1234,
                duration_ms: 9000,
                tool_calls: 7,
                turns: 3,
            },
            json!({
                "status": "completed",
                "output": "done",
                "error": null,
                "tokens_used": 1234,
                "duration_ms": 9000,
                "tool_calls": 7,
                "turns": 3,
            }),
        );

        round_trip(
            &AgentEventsParams {
                id: "a-1".into(),
                cursor: 2,
                timeout_ms: Some(500),
            },
            json!({ "id": "a-1", "cursor": 2, "timeout_ms": 500 }),
        );
        round_trip(
            &AgentEventsResult {
                events: vec![AgentEventDto {
                    seq: 2,
                    kind: AgentEventKindDto::Progress,
                    data: json!({ "turns": 1 }),
                }],
                next_cursor: 3,
                done: false,
            },
            json!({
                "events": [{ "seq": 2, "kind": "progress", "data": { "turns": 1 } }],
                "next_cursor": 3,
                "done": false,
            }),
        );

        round_trip(&AgentListParams {}, json!({}));
        round_trip(
            &AgentListResult {
                agents: vec![
                    AgentDescriptorDto {
                        name: "Explore".into(),
                        description: "search the repo".into(),
                        model: Some("grok-code-fast-1".into()),
                    },
                    // `model: None` must omit the key entirely (skip_serializing_if).
                    AgentDescriptorDto {
                        name: "general-purpose".into(),
                        description: String::new(),
                        model: None,
                    },
                ],
            },
            json!({
                "agents": [
                    { "name": "Explore", "description": "search the repo", "model": "grok-code-fast-1" },
                    { "name": "general-purpose", "description": "" },
                ],
            }),
        );
        round_trip(
            &AgentCancelParams { id: "a-1".into() },
            json!({ "id": "a-1" }),
        );
        round_trip(
            &AgentCancelResult {
                outcome: AgentCancelOutcomeDto::AlreadyFinished,
            },
            json!({ "outcome": "already_finished" }),
        );
        round_trip(
            &AgentSendParams {
                id: "a-1".into(),
                prompt: "and now review it".into(),
                timeout_ms: Some(60_000),
            },
            json!({ "id": "a-1", "prompt": "and now review it", "timeout_ms": 60000 }),
        );
        // `timeout_ms` defaults to absent.
        let p: AgentSendParams =
            serde_json::from_value(json!({ "id": "a-2", "prompt": "go" })).unwrap();
        assert_eq!(p.timeout_ms, None);
        round_trip(&AgentSendResult { id: "a-9".into() }, json!({ "id": "a-9" }));
    }

    /// An agent descriptor deserializes from just a `name`: `description`
    /// defaults to `""` and `model` to `None` (forward-compat with older cores
    /// that only sent names).
    #[test]
    fn agent_descriptor_tolerates_missing_optionals() {
        let d: AgentDescriptorDto =
            serde_json::from_value(json!({ "name": "Explore", "future": 1 }))
                .expect("deserialize name-only descriptor");
        assert_eq!(d.name, "Explore");
        assert_eq!(d.description, "");
        assert_eq!(d.model, None);
    }

    /// Spawn params tolerate a minimal `{ prompt }` object (all other fields
    /// defaulted) and unknown future fields.
    #[test]
    fn agent_spawn_params_tolerate_minimal_and_unknown() {
        let p: AgentSpawnParams =
            serde_json::from_value(json!({ "prompt": "go", "future": 1 })).unwrap();
        assert_eq!(p.prompt, "go");
        assert_eq!(p.agent_type, None);
        assert_eq!(p.timeout_ms, None);

        let e: AgentEventsParams = serde_json::from_value(json!({ "id": "x" })).unwrap();
        assert_eq!(e.cursor, 0);
        assert_eq!(e.timeout_ms, None);
    }

    #[test]
    fn agent_status_and_event_kind_wire_forms() {
        for (v, s) in [
            (AgentStatusDto::Running, "running"),
            (AgentStatusDto::Completed, "completed"),
            (AgentStatusDto::Failed, "failed"),
            (AgentStatusDto::Cancelled, "cancelled"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(s));
        }
        for (v, s) in [
            (AgentEventKindDto::Spawned, "spawned"),
            (AgentEventKindDto::Progress, "progress"),
            (AgentEventKindDto::Completed, "completed"),
            (AgentEventKindDto::Failed, "failed"),
            (AgentEventKindDto::Cancelled, "cancelled"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(s));
        }
    }

    #[test]
    fn log_emit_round_trip() {
        round_trip(
            &LogEmitParams {
                level: LogLevelDto::Warn,
                message: "hi".into(),
                fields: Some(json!({ "n": 1 })),
            },
            json!({ "level": "warn", "message": "hi", "fields": { "n": 1 } }),
        );
    }

    #[test]
    fn panel_view_model_round_trip() {
        round_trip(
            &PanelViewModel {
                id: "review".into(),
                title: "Code Review".into(),
                blocks: vec![
                    PanelBlock::Status {
                        items: vec![
                            PanelStatusItem {
                                label: "status".into(),
                                value: "green".into(),
                                tone: PanelTone::Success,
                            },
                            // tone defaults to neutral when omitted (below).
                            PanelStatusItem {
                                label: "files".into(),
                                value: "3".into(),
                                tone: PanelTone::Neutral,
                            },
                        ],
                    },
                    PanelBlock::Markdown {
                        text: "# Summary\nLooks good.".into(),
                    },
                    PanelBlock::Table {
                        columns: vec!["file".into(), "risk".into()],
                        rows: vec![
                            vec!["a.rs".into(), "low".into()],
                            vec!["b.rs".into(), "high".into()],
                        ],
                        selectable: true,
                    },
                    PanelBlock::Actions {
                        buttons: vec![PanelButton {
                            id: "approve".into(),
                            label: "Approve".into(),
                            key: Some("a".into()),
                        }],
                    },
                ],
            },
            json!({
                "id": "review",
                "title": "Code Review",
                "blocks": [
                    {
                        "kind": "status",
                        "items": [
                            { "label": "status", "value": "green", "tone": "success" },
                            { "label": "files", "value": "3", "tone": "neutral" },
                        ],
                    },
                    { "kind": "markdown", "text": "# Summary\nLooks good." },
                    {
                        "kind": "table",
                        "columns": ["file", "risk"],
                        "rows": [["a.rs", "low"], ["b.rs", "high"]],
                        "selectable": true,
                    },
                    {
                        "kind": "actions",
                        "buttons": [{ "id": "approve", "label": "Approve", "key": "a" }],
                    },
                ],
            }),
        );

        // Empty view model: no blocks. `blocks` defaults to `[]`, and a bare
        // status item omits `tone` (defaults to neutral).
        let vm: PanelViewModel =
            serde_json::from_value(json!({ "id": "p1", "title": "T", "future": 1 })).unwrap();
        assert_eq!(vm.blocks, Vec::<PanelBlock>::new());
        let item: PanelStatusItem =
            serde_json::from_value(json!({ "label": "l", "value": "v" })).unwrap();
        assert_eq!(item.tone, PanelTone::Neutral);
        let btn: PanelButton =
            serde_json::from_value(json!({ "id": "b", "label": "L" })).unwrap();
        assert_eq!(btn.key, None);
    }

    /// Each internally-tagged `PanelBlock` variant serializes with a `kind`
    /// discriminator and round-trips (mirrors `hook_invoke_result_variants`).
    #[test]
    fn panel_block_variants_round_trip() {
        round_trip(
            &PanelBlock::Status {
                items: vec![PanelStatusItem {
                    label: "l".into(),
                    value: "v".into(),
                    tone: PanelTone::Warning,
                }],
            },
            json!({ "kind": "status", "items": [{ "label": "l", "value": "v", "tone": "warning" }] }),
        );
        round_trip(
            &PanelBlock::Markdown { text: "hi".into() },
            json!({ "kind": "markdown", "text": "hi" }),
        );
        round_trip(
            &PanelBlock::Table {
                columns: vec!["c".into()],
                rows: vec![vec!["r".into()]],
                selectable: false,
            },
            json!({ "kind": "table", "columns": ["c"], "rows": [["r"]], "selectable": false }),
        );
        round_trip(
            &PanelBlock::Input {
                id: "code".into(),
                label: "Authorization code".into(),
                placeholder: Some("paste here".into()),
                value: None,
                secret: false,
            },
            json!({
                "kind": "input",
                "id": "code",
                "label": "Authorization code",
                "placeholder": "paste here",
                "value": null,
                "secret": false,
            }),
        );
        // A minimal Input tolerates missing optionals (placeholder/value/secret).
        let b: PanelBlock =
            serde_json::from_value(json!({ "kind": "input", "id": "t", "label": "Token" }))
                .unwrap();
        assert_eq!(
            b,
            PanelBlock::Input {
                id: "t".into(),
                label: "Token".into(),
                placeholder: None,
                value: None,
                secret: false,
            }
        );
        round_trip(
            &PanelBlock::Actions {
                buttons: vec![PanelButton {
                    id: "b".into(),
                    label: "B".into(),
                    key: None,
                }],
            },
            json!({ "kind": "actions", "buttons": [{ "id": "b", "label": "B", "key": null }] }),
        );
    }

    #[test]
    fn panel_tone_wire_forms() {
        for (v, s) in [
            (PanelTone::Neutral, "neutral"),
            (PanelTone::Success, "success"),
            (PanelTone::Warning, "warning"),
            (PanelTone::Error, "error"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(s));
        }
        assert_eq!(PanelTone::default(), PanelTone::Neutral);
    }

    #[test]
    fn panel_rpc_round_trip() {
        round_trip(&PanelPublishResult {}, json!({}));
        round_trip(&PanelCloseParams { id: "p1".into() }, json!({ "id": "p1" }));
        round_trip(&PanelCloseResult {}, json!({}));
        round_trip(
            &PanelActionParams {
                panel_id: "oauth".into(),
                button_id: "submit".into(),
                inputs: std::collections::BTreeMap::from([("code".into(), "abc-123".into())]),
            },
            json!({
                "panel_id": "oauth",
                "button_id": "submit",
                "inputs": { "code": "abc-123" },
            }),
        );
        // A button with no input fields serializes an empty `inputs` map, and
        // `inputs` defaults to empty when an older core omits it.
        round_trip(
            &PanelActionParams {
                panel_id: "review".into(),
                button_id: "approve".into(),
                inputs: std::collections::BTreeMap::new(),
            },
            json!({ "panel_id": "review", "button_id": "approve", "inputs": {} }),
        );
        let p: PanelActionParams =
            serde_json::from_value(json!({ "panel_id": "p", "button_id": "b" })).unwrap();
        assert!(p.inputs.is_empty());
    }

    #[test]
    fn shutdown_round_trip() {
        round_trip(
            &ShutdownParams {
                reason: "session_end".into(),
            },
            json!({ "reason": "session_end" }),
        );
    }

    #[test]
    fn gate_and_decision_and_log_level_wire_forms() {
        for (g, s) in [
            (GateKindDto::Observe, "observe"),
            (GateKindDto::Tool, "tool"),
            (GateKindDto::Stop, "stop"),
            (GateKindDto::Replace, "replace"),
            (GateKindDto::Intercept, "intercept"),
        ] {
            assert_eq!(serde_json::to_value(&g).unwrap(), json!(s));
        }
        assert_eq!(
            serde_json::to_value(DecisionDto::Allow).unwrap(),
            json!("allow")
        );
        assert_eq!(
            serde_json::to_value(DecisionDto::Deny).unwrap(),
            json!("deny")
        );
        assert_eq!(
            serde_json::to_value(LogLevelDto::Debug).unwrap(),
            json!("debug")
        );
        assert_eq!(
            serde_json::to_value(LogLevelDto::Error).unwrap(),
            json!("error")
        );
    }

    /// Golden list: `EventName`'s snake_case wire forms are frozen. `Display`,
    /// `as_str`, serde output, and round-trip must all agree, and the set is
    /// exactly the 15 live + 8 reserved events.
    #[test]
    fn event_name_wire_forms_are_stable() {
        let golden = [
            (EventName::SessionStart, "session_start"),
            (EventName::SessionEnd, "session_end"),
            (EventName::Stop, "stop"),
            (EventName::StopFailure, "stop_failure"),
            (EventName::PreToolUse, "pre_tool_use"),
            (EventName::PostToolUse, "post_tool_use"),
            (EventName::PostToolUseFailure, "post_tool_use_failure"),
            (EventName::PermissionDenied, "permission_denied"),
            (EventName::UserPromptSubmit, "user_prompt_submit"),
            (EventName::Notification, "notification"),
            (EventName::SubagentStart, "subagent_start"),
            (EventName::SubagentStop, "subagent_stop"),
            (EventName::SubagentEnd, "subagent_end"),
            (EventName::PreCompact, "pre_compact"),
            (EventName::PostCompact, "post_compact"),
            (EventName::ProviderRequest, "provider_request"),
            (EventName::ProviderResponse, "provider_response"),
            (EventName::ProviderError, "provider_error"),
            (EventName::SubagentResolve, "subagent_resolve"),
            (EventName::PermissionAsk, "permission_ask"),
            (EventName::ResolveCredential, "resolve_credential"),
            (EventName::RefreshCredential, "refresh_credential"),
            (EventName::StartOauthFlow, "start_oauth_flow"),
        ];
        assert_eq!(golden.len(), 23, "15 live + 8 reserved events");
        for (ev, wire) in golden {
            assert_eq!(ev.as_str(), wire, "as_str drift");
            assert_eq!(ev.to_string(), wire, "Display drift");
            assert_eq!(
                serde_json::to_value(ev).unwrap(),
                json!(wire),
                "serde drift"
            );
            let back: EventName = serde_json::from_value(json!(wire)).unwrap();
            assert_eq!(back, ev, "round-trip drift");
        }
    }

    /// The nested payload enums serialize to their frozen wire tokens.
    #[test]
    fn payload_nested_enum_wire_forms() {
        assert_eq!(
            serde_json::to_value(SubagentStopPhaseDto::Gate).unwrap(),
            json!("gate")
        );
        assert_eq!(
            serde_json::to_value(SubagentStopPhaseDto::Observe).unwrap(),
            json!("observe")
        );
        for (v, s) in [
            (BackgroundTaskTypeDto::Shell, "shell"),
            (BackgroundTaskTypeDto::Monitor, "monitor"),
            (BackgroundTaskTypeDto::Subagent, "subagent"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(s));
        }
        for (v, s) in [
            (StopFailureKindDto::RateLimit, "rate_limit"),
            (StopFailureKindDto::AuthenticationFailed, "authentication_failed"),
            (StopFailureKindDto::InvalidRequest, "invalid_request"),
            (StopFailureKindDto::ServerError, "server_error"),
            (StopFailureKindDto::MaxOutputTokens, "max_output_tokens"),
            (StopFailureKindDto::Unknown, "unknown"),
        ] {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(s));
        }
    }

    /// `ProviderRequestPayload`: header tuples serialize as `[k, v]` pairs and
    /// the tool catalog as a string array, both camelCase-keyed.
    #[test]
    fn provider_request_payload_round_trip() {
        round_trip(
            &ProviderRequestPayload {
                endpoint: "chat/completions".into(),
                model: "grok-4.5".into(),
                base_url_alias: "https://api.x.ai/v1".into(),
                agent: "reviewer".into(),
                tools: vec!["read_file".into(), "memory__recall".into()],
                headers: vec![
                    ("accept".into(), "text/event-stream".into()),
                    ("content-type".into(), "application/json".into()),
                ],
                body: json!({ "model": "grok-4.5", "stream": true }),
            },
            json!({
                "endpoint": "chat/completions",
                "model": "grok-4.5",
                "baseUrlAlias": "https://api.x.ai/v1",
                "agent": "reviewer",
                "tools": ["read_file", "memory__recall"],
                "headers": [
                    ["accept", "text/event-stream"],
                    ["content-type", "application/json"],
                ],
                "body": { "model": "grok-4.5", "stream": true },
            }),
        );
    }

    /// `PostToolUsePayload`: every `tool*` field, the optional `durationMs`
    /// number, and `isBackgrounded`, all camelCase on the wire.
    #[test]
    fn post_tool_use_payload_round_trip() {
        round_trip(
            &PostToolUsePayload {
                tool_name: "bash".into(),
                tool_use_id: "call-1".into(),
                tool_input: json!({ "command": "ls" }),
                tool_result: json!({ "stdout": "a\nb" }),
                tool_input_truncated: false,
                tool_result_truncated: true,
                duration_ms: Some(1234),
                is_backgrounded: false,
                subagent_type: Some("explore".into()),
            },
            json!({
                "toolName": "bash",
                "toolUseId": "call-1",
                "toolInput": { "command": "ls" },
                "toolResult": { "stdout": "a\nb" },
                "toolInputTruncated": false,
                "toolResultTruncated": true,
                "durationMs": 1234,
                "isBackgrounded": false,
                "subagentType": "explore",
            }),
        );

        // Absent optionals are omitted (not null), and deserialization is
        // forward-tolerant of unknown fields.
        let p: PostToolUsePayload = serde_json::from_value(json!({
            "toolName": "read_file",
            "toolUseId": "call-2",
            "toolInput": {},
            "toolResult": {},
            "toolInputTruncated": false,
            "toolResultTruncated": false,
            "isBackgrounded": true,
            "future": 1,
        }))
        .unwrap();
        assert_eq!(p.duration_ms, None);
        assert_eq!(p.subagent_type, None);
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("durationMs").is_none());
        assert!(v.get("subagentType").is_none());
    }

    /// `StopPayload`: the nested `backgroundTasks` / `sessionCrons` vecs and
    /// their per-entry camelCase renames (`type`, `agentType`).
    #[test]
    fn stop_payload_round_trip() {
        round_trip(
            &StopPayload {
                reason: "end_turn".into(),
                stop_hook_active: true,
                last_assistant_message: Some("done".into()),
                background_tasks: Some(vec![
                    StopBackgroundTaskDto {
                        id: "task-001".into(),
                        r#type: BackgroundTaskTypeDto::Shell,
                        status: "running".into(),
                        description: None,
                        command: Some("tail -f log".into()),
                        agent_type: None,
                    },
                    StopBackgroundTaskDto {
                        id: "task-002".into(),
                        r#type: BackgroundTaskTypeDto::Subagent,
                        status: "running".into(),
                        description: Some("explore".into()),
                        command: None,
                        agent_type: Some("explore".into()),
                    },
                ]),
                session_crons: Some(vec![StopSessionCronDto {
                    id: "cron-001".into(),
                    schedule: "every 2h".into(),
                    recurring: true,
                    prompt: "check the build".into(),
                }]),
            },
            json!({
                "reason": "end_turn",
                "stopHookActive": true,
                "lastAssistantMessage": "done",
                "backgroundTasks": [
                    { "id": "task-001", "type": "shell", "status": "running", "command": "tail -f log" },
                    {
                        "id": "task-002",
                        "type": "subagent",
                        "status": "running",
                        "description": "explore",
                        "agentType": "explore",
                    },
                ],
                "sessionCrons": [
                    { "id": "cron-001", "schedule": "every 2h", "recurring": true, "prompt": "check the build" },
                ],
            }),
        );
    }

    /// `ProviderResponsePayload`: the `toolCalls` vec of `{ id, name }` entries.
    #[test]
    fn provider_response_payload_round_trip() {
        round_trip(
            &ProviderResponsePayload {
                base_url: "https://provider.example/v1".into(),
                endpoint: "messages".into(),
                tool_calls: vec![
                    ProviderResponseToolCallDto {
                        id: "call_1".into(),
                        name: "masked_a".into(),
                    },
                    ProviderResponseToolCallDto {
                        id: "call_2".into(),
                        name: "masked_b".into(),
                    },
                ],
            },
            json!({
                "baseUrl": "https://provider.example/v1",
                "endpoint": "messages",
                "toolCalls": [
                    { "id": "call_1", "name": "masked_a" },
                    { "id": "call_2", "name": "masked_b" },
                ],
            }),
        );
    }

    /// One credential payload: `resolve_credential` with its `baseUrl` /
    /// `ownerHint` camelCase renames and omit-on-absent behavior.
    #[test]
    fn resolve_credential_payload_round_trip() {
        round_trip(
            &ResolveCredentialPayload {
                reason: "outbound".into(),
                base_url: "https://idp.example/v1".into(),
                owner_hint: Some("primary".into()),
            },
            json!({
                "reason": "outbound",
                "baseUrl": "https://idp.example/v1",
                "ownerHint": "primary",
            }),
        );

        // Absent `ownerHint` is omitted, not null.
        let p: ResolveCredentialPayload = serde_json::from_value(json!({
            "reason": "bootstrap",
            "baseUrl": "",
        }))
        .unwrap();
        assert_eq!(p.owner_hint, None);
        assert!(serde_json::to_value(&p).unwrap().get("ownerHint").is_none());
    }

    /// `StopFailurePayload`: the nested `StopFailureKindDto` and camelCase
    /// `errorDetails`.
    #[test]
    fn stop_failure_payload_round_trip() {
        round_trip(
            &StopFailurePayload {
                error: StopFailureKindDto::RateLimit,
                error_details: Some("429".into()),
                last_assistant_message: None,
            },
            json!({
                "error": "rate_limit",
                "errorDetails": "429",
            }),
        );
    }
}
