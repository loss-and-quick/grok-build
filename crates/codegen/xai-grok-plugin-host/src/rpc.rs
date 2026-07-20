//! Minimal JSON-RPC 2.0 framing shared by the sidecar client loop and the
//! capability server.
//!
//! We hand-roll rather than pull an RPC library: the contract is a handful of
//! methods over newline-delimited compact JSON, and both directions send
//! requests *and* notifications on the same pipe, which a client-only crate
//! wouldn't model cleanly. Ids are numeric with independent spaces per direction
//! (we allocate ours; a plugin allocates its own and we echo them back).

use serde_json::{Value, json};

/// A JSON-RPC error object returned by a capability handler (plugin→core) or
/// carried in a response we receive (core→plugin).
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    /// `-32601`: the requested method isn't served. Used for reserved
    /// `agent_*` methods and anything unrecognized.
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
            data: None,
        }
    }

    /// `-32602`: params were missing or malformed.
    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: detail.into(),
            data: None,
        }
    }

    /// `-32603`: the handler failed internally (e.g. storage IO).
    pub fn internal(detail: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: detail.into(),
            data: None,
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rpc error {}: {}", self.code, self.message)
    }
}

/// Compact-serialize an outbound request frame.
pub fn request_frame(id: i64, method: &str, params: &Value) -> String {
    compact(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
}

/// Compact-serialize an outbound notification frame (no id, no reply).
pub fn notification_frame(method: &str, params: &Value) -> String {
    compact(&json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }))
}

/// Compact-serialize a successful response to a plugin request. `id` is the
/// plugin's original id value, echoed verbatim (number or string).
pub fn response_ok_frame(id: &Value, result: Value) -> String {
    compact(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
}

/// Compact-serialize an error response to a plugin request.
pub fn response_err_frame(id: &Value, err: &RpcError) -> String {
    let mut error = json!({ "code": err.code, "message": err.message });
    if let Some(data) = &err.data {
        error["data"] = data.clone();
    }
    compact(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    }))
}

/// Serialize to compact single-line JSON (mirrors `xai-acp-lib::common::compact_json`).
fn compact<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| {
        // A DTO that can't serialize is a programming error; emit a valid frame
        // that the peer can at least decode rather than a torn line.
        tracing::error!("failed to serialize JSON-RPC frame: {e}");
        json!({ "jsonrpc": "2.0", "error": { "code": -32603, "message": "serialize failed" } })
            .to_string()
    })
}

/// A decoded inbound frame from the plugin. Responses are correlated by id;
/// requests and notifications are dispatched to the capability server.
pub enum Inbound {
    /// A reply to one of *our* requests.
    Response {
        id: i64,
        result: Result<Value, RpcError>,
    },
    /// A request from the plugin (expects a reply keyed by `id`).
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// A fire-and-forget notification from the plugin.
    Notification { method: String, params: Value },
}

/// Classify a parsed JSON value as one of the three inbound frame shapes.
///
/// Returns `None` for frames we can't route (missing method on a non-response,
/// non-integer response id): the caller logs and skips.
pub fn classify(value: Value) -> Option<Inbound> {
    let obj = value.as_object()?;

    // A `method` present => it's an inbound request or notification.
    if let Some(method) = obj.get("method").and_then(Value::as_str) {
        let method = method.to_string();
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        return match obj.get("id") {
            Some(id) if !id.is_null() => Some(Inbound::Request {
                id: id.clone(),
                method,
                params,
            }),
            _ => Some(Inbound::Notification { method, params }),
        };
    }

    // Otherwise it's a response to a request we sent; correlate by our numeric id.
    let id = obj.get("id").and_then(Value::as_i64)?;
    if let Some(error) = obj.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32603);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string();
        let data = error.get("data").cloned();
        return Some(Inbound::Response {
            id,
            result: Err(RpcError {
                code,
                message,
                data,
            }),
        });
    }
    let result = obj.get("result").cloned().unwrap_or(Value::Null);
    Some(Inbound::Response {
        id,
        result: Ok(result),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_response_ok() {
        let v = serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"result":{"a":1}}"#).unwrap();
        match classify(v).unwrap() {
            Inbound::Response {
                id,
                result: Ok(val),
            } => {
                assert_eq!(id, 7);
                assert_eq!(val, json!({"a": 1}));
            }
            _ => panic!("expected ok response"),
        }
    }

    #[test]
    fn classify_response_err() {
        let v = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"nope"}}"#,
        )
        .unwrap();
        match classify(v).unwrap() {
            Inbound::Response { id, result: Err(e) } => {
                assert_eq!(id, 3);
                assert_eq!(e.code, -32601);
                assert_eq!(e.message, "nope");
            }
            _ => panic!("expected err response"),
        }
    }

    #[test]
    fn classify_request_and_notification() {
        let req = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"method":"storage_get","params":{"key":"k"}}"#,
        )
        .unwrap();
        assert!(matches!(classify(req), Some(Inbound::Request { .. })));

        let note = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"log_emit","params":{"level":"info","message":"hi"}}"#,
        )
        .unwrap();
        assert!(matches!(classify(note), Some(Inbound::Notification { .. })));
    }

    #[test]
    fn frames_round_trip_through_classify() {
        let req = request_frame(42, "hook_invoke", &json!({"event": "stop"}));
        let parsed: Value = serde_json::from_str(&req).unwrap();
        assert_eq!(parsed["id"], 42);
        assert_eq!(parsed["method"], "hook_invoke");

        let ok = response_ok_frame(&json!(9), json!({"value": null}));
        let parsed: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(parsed["id"], 9);
        assert!(parsed.get("error").is_none());

        let err = response_err_frame(&json!("str-id"), &RpcError::method_not_found("agent_spawn"));
        let parsed: Value = serde_json::from_str(&err).unwrap();
        assert_eq!(parsed["id"], "str-id");
        assert_eq!(parsed["error"]["code"], -32601);
    }
}
