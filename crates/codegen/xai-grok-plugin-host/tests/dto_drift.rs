//! Auto-exhaustive drift guard: proves the per-event payload DTOs in
//! `xai-grok-plugin-protocol` mirror the source `xai-grok-hooks::event::HookPayload`
//! wire shape byte-for-byte, and *cannot* silently drift.
//!
//! `plugin-host` is the one crate that depends on both the source `HookPayload`
//! and the plugin-protocol DTOs, so the two wire shapes can only be compared
//! here. The guard has two teeth:
//!
//! - **A renamed / missing / wrongly-typed field fails an assert.** Each sample
//!   is serialized from the source `HookPayload`, deserialized into the matching
//!   DTO, re-serialized, and the two JSON values are compared. Every field is
//!   populated (every `Option` is `Some`, every `Vec` non-empty) so no field can
//!   hide behind `skip_serializing_if`.
//! - **A new `HookPayload` variant fails to COMPILE.** The mapping from a sample
//!   to its DTO is an exhaustive, wildcard-free `match` over `&HookPayload`; the
//!   compiler forces a new variant to be handled here (modeled on
//!   `xai-grok-workspace`'s `hook_event_name_wire_covers_all_upstream_variants`).

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use xai_grok_hooks::event::{
    BackgroundTaskType, HookPayload, ProviderResponseToolCall, StopBackgroundTask, StopFailureKind,
    StopSessionCron, SubagentStopPhase,
};
use xai_grok_plugin_protocol as proto;

/// Deserialize `v` into DTO `T`, re-serialize, and return the DTO's JSON. The
/// DTO's re-serialized wire must equal the source `HookPayload`'s wire.
fn reround<T: Serialize + DeserializeOwned>(v: &Value) -> Value {
    let dto: T =
        serde_json::from_value(v.clone()).expect("source payload JSON deserializes into its DTO");
    serde_json::to_value(&dto).expect("DTO re-serializes")
}

/// One fully-populated source sample per `HookPayload` variant. Adding a variant
/// leaves it untested here until it is added to this vec *and* to the exhaustive
/// `match` in [`dto_wire_matches_source_for_every_variant`] (which fails to
/// compile until then).
fn all_payload_samples() -> Vec<HookPayload> {
    vec![
        HookPayload::SessionStart {
            source: "startup".into(),
            model_id: Some("grok-4.5".into()),
            agent_type: Some("main".into()),
        },
        HookPayload::SessionEnd {
            reason: "logout".into(),
            turn_count: Some(12),
            tool_call_count: Some(34),
        },
        HookPayload::Stop {
            reason: "end_turn".into(),
            stop_hook_active: true,
            last_assistant_message: Some("all done".into()),
            background_tasks: Some(vec![
                StopBackgroundTask {
                    id: "task-1".into(),
                    r#type: BackgroundTaskType::Shell,
                    status: "running".into(),
                    description: Some("tail logs".into()),
                    command: Some("tail -f log".into()),
                    agent_type: Some("main".into()),
                },
                StopBackgroundTask {
                    id: "task-2".into(),
                    r#type: BackgroundTaskType::Subagent,
                    status: "running".into(),
                    description: Some("explore".into()),
                    command: Some("cargo test".into()),
                    agent_type: Some("explore".into()),
                },
            ]),
            session_crons: Some(vec![StopSessionCron {
                id: "cron-1".into(),
                schedule: "every 5 minutes".into(),
                recurring: true,
                prompt: "check the build".into(),
            }]),
        },
        HookPayload::StopFailure {
            error: StopFailureKind::RateLimit,
            error_details: Some("429 slow down".into()),
            last_assistant_message: Some("rate limited".into()),
        },
        HookPayload::PreToolUse {
            tool_name: "bash".into(),
            tool_use_id: "call-1".into(),
            tool_input: serde_json::json!({ "command": "ls" }),
            tool_input_truncated: true,
            subagent_type: Some("explore".into()),
        },
        HookPayload::PostToolUse {
            tool_name: "bash".into(),
            tool_use_id: "call-2".into(),
            tool_input: serde_json::json!({ "command": "ls" }),
            tool_result: serde_json::json!({ "stdout": "a" }),
            tool_input_truncated: false,
            tool_result_truncated: true,
            duration_ms: Some(4321),
            is_backgrounded: true,
            subagent_type: Some("explore".into()),
        },
        HookPayload::PostToolUseFailure {
            tool_name: "bash".into(),
            tool_use_id: "call-3".into(),
            tool_input: serde_json::json!({ "command": "boom" }),
            tool_input_truncated: false,
            error: "exit 1".into(),
            subagent_type: Some("explore".into()),
        },
        HookPayload::PermissionDenied {
            tool_name: "bash".into(),
            tool_use_id: "call-4".into(),
            tool_input: serde_json::json!({ "command": "rm -rf /" }),
            tool_input_truncated: false,
        },
        HookPayload::UserPromptSubmit {
            prompt: Some("hello".into()),
        },
        HookPayload::Notification {
            notification_type: "info".into(),
            message: Some("heads up".into()),
            title: Some("Notice".into()),
            level: Some("warn".into()),
        },
        HookPayload::SubagentStart {
            subagent_id: "sub-1".into(),
            subagent_type: "explore".into(),
            description: Some("scan the repo".into()),
        },
        HookPayload::SubagentStop {
            phase: SubagentStopPhase::Gate,
            subagent_id: "sub-1".into(),
            subagent_type: "explore".into(),
            stop_hook_active: Some(true),
            last_assistant_message: Some("subagent done".into()),
        },
        HookPayload::PreCompact {
            source: "auto".into(),
        },
        HookPayload::PostCompact {
            source: "manual".into(),
        },
        HookPayload::ProviderRequest {
            endpoint: "chat/completions".into(),
            model: "grok-4.5".into(),
            base_url_alias: "https://api.x.ai/v1".into(),
            agent: "reviewer".into(),
            tools: vec!["read_file".into(), "memory__recall".into()],
            headers: vec![
                ("accept".into(), "text/event-stream".into()),
                ("content-type".into(), "application/json".into()),
            ],
            body: serde_json::json!({ "model": "grok-4.5", "stream": true }),
        },
        HookPayload::ProviderResponse {
            base_url: "https://provider.example/v1".into(),
            endpoint: "messages".into(),
            tool_calls: vec![
                ProviderResponseToolCall {
                    id: "call_1".into(),
                    name: "masked_a".into(),
                },
                ProviderResponseToolCall {
                    id: "call_2".into(),
                    name: "masked_b".into(),
                },
            ],
        },
        HookPayload::ProviderError {
            error_class: "5xx".into(),
            model: "grok-4.5".into(),
            attempt: 3,
            base_url_alias: "https://api.x.ai/v1".into(),
        },
        HookPayload::SubagentResolve {
            subagent_id: "sub-1".into(),
            subagent_type: "explore".into(),
            description: "scan the repo".into(),
            prompt_preview: "find all callers of foo".into(),
            model: Some("grok-code-fast-1".into()),
            parent_model: "grok-4.5".into(),
        },
        HookPayload::ResolveCredential {
            reason: "outbound".into(),
            base_url: "https://idp.example/v1".into(),
            owner_hint: Some("primary".into()),
        },
        HookPayload::RefreshCredential {
            reason: "unauthorized".into(),
            base_url: "https://api.x.ai/v1".into(),
            owner_id: Some("acct-1".into()),
        },
        HookPayload::StartOauthFlow {
            reason: "missing_credential".into(),
            owner_hint: Some("primary".into()),
        },
    ]
}

/// For every `HookPayload` variant, the source wire JSON must survive a
/// round-trip through the matching plugin-protocol DTO unchanged.
///
/// The `match` is exhaustive and wildcard-free on purpose: a new `HookPayload`
/// variant fails to compile here (not merely at runtime), forcing its DTO and
/// this mapping to be added. A renamed or wrongly-typed field instead fails the
/// `assert_eq!` below.
#[test]
fn dto_wire_matches_source_for_every_variant() {
    for payload in all_payload_samples() {
        let source = serde_json::to_value(&payload).expect("source HookPayload serializes");
        let via_dto = match &payload {
            HookPayload::SessionStart { .. } => reround::<proto::SessionStartPayload>(&source),
            HookPayload::SessionEnd { .. } => reround::<proto::SessionEndPayload>(&source),
            HookPayload::Stop { .. } => reround::<proto::StopPayload>(&source),
            HookPayload::StopFailure { .. } => reround::<proto::StopFailurePayload>(&source),
            HookPayload::PreToolUse { .. } => reround::<proto::PreToolUsePayload>(&source),
            HookPayload::PostToolUse { .. } => reround::<proto::PostToolUsePayload>(&source),
            HookPayload::PostToolUseFailure { .. } => {
                reround::<proto::PostToolUseFailurePayload>(&source)
            }
            HookPayload::PermissionDenied { .. } => {
                reround::<proto::PermissionDeniedPayload>(&source)
            }
            HookPayload::UserPromptSubmit { .. } => {
                reround::<proto::UserPromptSubmitPayload>(&source)
            }
            HookPayload::Notification { .. } => reround::<proto::NotificationPayload>(&source),
            HookPayload::SubagentStart { .. } => reround::<proto::SubagentStartPayload>(&source),
            HookPayload::SubagentStop { .. } => reround::<proto::SubagentStopPayload>(&source),
            HookPayload::PreCompact { .. } => reround::<proto::PreCompactPayload>(&source),
            HookPayload::PostCompact { .. } => reround::<proto::PostCompactPayload>(&source),
            HookPayload::ProviderRequest { .. } => reround::<proto::ProviderRequestPayload>(&source),
            HookPayload::ProviderResponse { .. } => {
                reround::<proto::ProviderResponsePayload>(&source)
            }
            HookPayload::ProviderError { .. } => reround::<proto::ProviderErrorPayload>(&source),
            HookPayload::SubagentResolve { .. } => reround::<proto::SubagentResolvePayload>(&source),
            HookPayload::ResolveCredential { .. } => {
                reround::<proto::ResolveCredentialPayload>(&source)
            }
            HookPayload::RefreshCredential { .. } => {
                reround::<proto::RefreshCredentialPayload>(&source)
            }
            HookPayload::StartOauthFlow { .. } => reround::<proto::StartOauthFlowPayload>(&source),
        };
        assert_eq!(
            source, via_dto,
            "wire drift between HookPayload and its DTO for {payload:?}"
        );
    }
}
