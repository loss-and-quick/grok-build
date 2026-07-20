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
// initialize (core→plugin request, handshake)
// ─────────────────────────────────────────────────────────────────────────────

/// Host abilities advertised to the plugin at handshake. Part of `initialize`,
/// core→plugin. `leader_socket` is reserved (currently always `None`).
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
/// names from the dictionary; `plugin_version` is informational.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "../../../../sdk/plugin/src/generated/", optional_fields = nullable)]
pub struct InitializeResult {
    pub protocol_version: u32,
    #[serde(default)]
    pub subscriptions: Vec<String>,
    #[serde(default)]
    pub plugin_version: Option<String>,
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
            HostCapabilities,
            InitializeParams,
            InitializeResult,
            HookInvokeParams,
            HookInvokeResult,
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
            },
            json!({
                "protocol_version": 1,
                "subscriptions": ["session_start", "pre_tool_use"],
                "plugin_version": "0.2.0",
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
    /// exactly the 15 live + 7 reserved events.
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
            (EventName::ProviderError, "provider_error"),
            (EventName::SubagentResolve, "subagent_resolve"),
            (EventName::PermissionAsk, "permission_ask"),
            (EventName::ResolveCredential, "resolve_credential"),
            (EventName::RefreshCredential, "refresh_credential"),
            (EventName::StartOauthFlow, "start_oauth_flow"),
        ];
        assert_eq!(golden.len(), 22, "15 live + 7 reserved events");
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
}
