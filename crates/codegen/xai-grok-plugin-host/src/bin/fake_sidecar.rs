//! Test fixture: a plugin sidecar that speaks the wire protocol over stdio.
//!
//! The supervisor/sidecar integration tests inject this binary via
//! `PluginHost::new_for_test`, so they never need a real bun/node/deno. It
//! deserializes and serializes the real `xai-grok-plugin-protocol` DTOs, so it
//! also cross-checks the host's wire shapes against the contract.
//!
//! Behavior knobs come from env vars (simpler than argv parsing):
//!
//! - `FAKE_PROTOCOL_VERSION`  — reply version at handshake (default 1).
//! - `FAKE_SUBSCRIPTIONS`     — comma-separated event names (default a broad set).
//! - `FAKE_PLUGIN_VERSION`    — informational `plugin_version`.
//! - `FAKE_MODE`:
//!     - `normal`             — reply per gate: Tool→deny(reason), Stop→block, else Observed.
//!     - `replace_payload`    — reply `replace` with a substitute payload (Replace gate).
//!     - `crash_on_invoke`    — exit(1) on the first `hook_invoke`.
//!     - `hang_on_invoke`     — never reply to `hook_invoke`.
//!     - `exit_after_handshake` — reply to initialize, then exit(0).
//!     - `storage_probe`      — on invoke, round-trip through `storage_*`/`log_emit`,
//!                              then reply Observed (exercises the plugin→core path).
//! - `FAKE_TOOL_MODE` (independent of `FAKE_MODE`; governs `tool_invoke`):
//!     - `echo` (default)     — reply with a content string echoing the tool,
//!                              arguments, and per-call context.
//!     - `error`              — reply `{ content, is_error: true }`.
//!     - `storage_probe`      — drive counter storage RPCs mid-`tool_invoke`
//!                              (reentrancy: the reply depends on the host
//!                              serving plugin→core while core→plugin is
//!                              in flight), then echo the stored value.
//!     - `hang`               — never reply to `tool_invoke`.
//!     - `crash`              — exit(1) on the first `tool_invoke`.

use std::io::{BufRead, StdinLock, Write};

use serde_json::{Value, json};
use xai_grok_plugin_protocol::{
    DecisionDto, GateKindDto, HookInvokeParams, HookInvokeResult, InitializeResult,
    PROTOCOL_VERSION, ToolDescriptorDto, ToolInvokeParams, ToolInvokeResult,
};

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn main() {
    let mode = env("FAKE_MODE").unwrap_or_else(|| "normal".to_string());
    let stdin = std::io::stdin();
    // One reader over one buffer for the whole session (no re-locking).
    let mut reader = stdin.lock();
    let mut next_id: i64 = 10_000;
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break; // EOF: parent closed our stdin.
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg): Result<Value, _> = serde_json::from_str(line.trim()) else {
            continue;
        };

        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = msg.get("id").cloned();

        match method.as_deref() {
            Some("initialize") => {
                let version: u32 = env("FAKE_PROTOCOL_VERSION")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(PROTOCOL_VERSION);
                let subscriptions: Vec<String> = env("FAKE_SUBSCRIPTIONS")
                    .unwrap_or_else(|| {
                        "session_start,pre_tool_use,stop,post_tool_use,user_prompt_submit"
                            .to_string()
                    })
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                let result = InitializeResult {
                    protocol_version: version,
                    subscriptions,
                    plugin_version: env("FAKE_PLUGIN_VERSION"),
                    tools: vec![ToolDescriptorDto {
                        name: "echo".to_string(),
                        description: "echo fixture tool".to_string(),
                        input_schema: json!({ "type": "object" }),
                    }],
                };
                reply_ok(&id, serde_json::to_value(result).unwrap());
                if mode == "exit_after_handshake" {
                    std::process::exit(0);
                }
            }
            Some("hook_invoke") => {
                match mode.as_str() {
                    "crash_on_invoke" => std::process::exit(1),
                    "hang_on_invoke" => {
                        // Drain and never reply until the pipe closes.
                        while reader.read_line(&mut line).unwrap_or(0) != 0 {}
                        return;
                    }
                    _ => {}
                }
                let params: HookInvokeParams =
                    serde_json::from_value(msg.get("params").cloned().unwrap_or(Value::Null))
                        .expect("valid hook_invoke params");

                let result = if mode == "storage_probe" {
                    storage_probe(&mut reader, &mut next_id);
                    HookInvokeResult::Observed
                } else if mode == "replace_payload" {
                    // Echo the received payload back under a marker so the test can
                    // confirm the host forwarded it, plus the substitution.
                    HookInvokeResult::Replace {
                        payload: Some(json!({ "replaced": true, "saw": params.payload })),
                    }
                } else {
                    match params.gate {
                        GateKindDto::Tool => HookInvokeResult::Decision {
                            decision: DecisionDto::Deny,
                            reason: Some("fixture-deny".to_string()),
                        },
                        GateKindDto::Stop => HookInvokeResult::Stop {
                            block: true,
                            reason: Some("fixture-block".to_string()),
                            continue_: None,
                            additional_context: Some("fixture-ctx".to_string()),
                        },
                        _ => HookInvokeResult::Observed,
                    }
                };
                reply_ok(&id, serde_json::to_value(result).unwrap());
            }
            Some("tool_invoke") => {
                let tool_mode = env("FAKE_TOOL_MODE").unwrap_or_else(|| "echo".to_string());
                match tool_mode.as_str() {
                    "crash" => std::process::exit(1),
                    "hang" => {
                        // Drain and never reply until the pipe closes.
                        while reader.read_line(&mut line).unwrap_or(0) != 0 {}
                        return;
                    }
                    "hang_record_cancel" => {
                        // Never reply, but keep the main loop running so the
                        // host's `tool_cancel` notification (fired when it
                        // abandons this call) is observed and recorded below.
                        continue;
                    }
                    _ => {}
                }
                let params: ToolInvokeParams =
                    serde_json::from_value(msg.get("params").cloned().unwrap_or(Value::Null))
                        .expect("valid tool_invoke params");

                let result = match tool_mode.as_str() {
                    "error" => ToolInvokeResult {
                        content: format!("tool '{}' failed on purpose", params.tool),
                        is_error: true,
                    },
                    "storage_probe" => {
                        // Counter plugin→core RPCs while this tool_invoke is
                        // still pending: set then read back a value keyed by
                        // the invocation, proving both directions interleave
                        // on one transport.
                        let set_id = alloc(&mut next_id);
                        request(
                            set_id,
                            "storage_set",
                            json!({ "key": params.invocation_id, "value": params.arguments }),
                        );
                        let _ = read_response_for(&mut reader, set_id);

                        let get_id = alloc(&mut next_id);
                        request(get_id, "storage_get", json!({ "key": params.invocation_id }));
                        let got = read_response_for(&mut reader, get_id);
                        ToolInvokeResult {
                            content: format!(
                                "stored-and-loaded: {}",
                                got.get("value").cloned().unwrap_or(Value::Null)
                            ),
                            is_error: false,
                        }
                    }
                    _ => ToolInvokeResult {
                        content: format!(
                            "echo tool={} args={} session={} cwd={} agent={}",
                            params.tool,
                            params.arguments,
                            params.context.session_id,
                            params.context.cwd,
                            params.context.agent,
                        ),
                        is_error: false,
                    },
                };
                reply_ok(&id, serde_json::to_value(result).unwrap());
            }
            Some("tool_cancel") => {
                // The host abandoned an in-flight `tool_invoke` (parent turn
                // aborted). Record the cancelled invocation via a plugin→core
                // storage_set so the test can assert the notification arrived.
                let inv = msg
                    .get("params")
                    .and_then(|p| p.get("invocation_id"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let set_id = alloc(&mut next_id);
                request(
                    set_id,
                    "storage_set",
                    json!({ "key": "tool_cancel_seen", "value": inv }),
                );
                let _ = read_response_for(&mut reader, set_id);
            }
            Some("shutdown") => std::process::exit(0),
            Some(_other) => {
                if let Some(id) = id
                    && !id.is_null()
                {
                    reply_err(&Some(id), -32601, "method not found");
                }
            }
            // A response to one of our own requests (storage_probe consumes those
            // inline), so a stray response here is ignored.
            None => {}
        }
    }
}

/// Exercise the plugin→core capability surface: log, then set/get/list/delete,
/// reading each reply inline from `reader`.
fn storage_probe(reader: &mut StdinLock<'_>, next_id: &mut i64) {
    notify(
        "log_emit",
        json!({ "level": "info", "message": "probe start", "fields": { "n": 1 } }),
    );

    let set_id = alloc(next_id);
    request(
        set_id,
        "storage_set",
        json!({ "key": "probe", "value": { "ok": true } }),
    );
    let _ = read_response_for(reader, set_id);

    let get_id = alloc(next_id);
    request(get_id, "storage_get", json!({ "key": "probe" }));
    let got = read_response_for(reader, get_id);
    assert_eq!(
        got.get("value"),
        Some(&json!({ "ok": true })),
        "storage_get should return what storage_set wrote"
    );

    let list_id = alloc(next_id);
    request(list_id, "storage_list", json!({ "prefix": "pro" }));
    let listed = read_response_for(reader, list_id);
    assert_eq!(listed.get("keys"), Some(&json!(["probe"])));

    let del_id = alloc(next_id);
    request(del_id, "storage_delete", json!({ "key": "probe" }));
    let deleted = read_response_for(reader, del_id);
    assert_eq!(deleted.get("existed"), Some(&json!(true)));
}

fn alloc(next_id: &mut i64) -> i64 {
    let id = *next_id;
    *next_id += 1;
    id
}

fn write_line(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn reply_ok(id: &Option<Value>, result: Value) {
    let id = id.clone().unwrap_or(Value::Null);
    write_line(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn reply_err(id: &Option<Value>, code: i64, message: &str) {
    let id = id.clone().unwrap_or(Value::Null);
    write_line(
        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    );
}

fn request(id: i64, method: &str, params: Value) {
    write_line(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
}

fn notify(method: &str, params: Value) {
    write_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
}

/// Block-read from `reader` until the response for `id` arrives, returning its
/// `result` object.
fn read_response_for(reader: &mut StdinLock<'_>, id: i64) -> Value {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return Value::Null; // EOF
        }
        let Ok(msg): Result<Value, _> = serde_json::from_str(line.trim()) else {
            continue;
        };
        if msg.get("id").and_then(Value::as_i64) == Some(id) {
            return msg.get("result").cloned().unwrap_or(Value::Null);
        }
    }
}
