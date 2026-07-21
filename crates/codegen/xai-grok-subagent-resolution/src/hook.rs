//! The `subagent_resolve` Replace-hook directive.
//!
//! A plugin subscribed to the `subagent_resolve` event receives the requested
//! spawn spec (agent type, prompt preview, model) and may reply with this
//! directive to substitute parts of the resolution before the spawn proceeds.
//! Every field is optional; an omitted field leaves that part of the request
//! unchanged. An unparseable reply fails open (the requested spec is kept),
//! mirroring the provider seams' tolerant deserialization.

use serde::{Deserialize, Serialize};

/// What a `subagent_resolve` hook may substitute. Wire shape is snake_case
/// with every field optional, tolerant of unknown fields (additive evolution).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentResolveDirective {
    /// Replacement agent type. Still subject to the same validation and
    /// allow-list gating as the originally requested type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    /// Replacement model override (validated against the model catalog like a
    /// tool-provided `Task.model` argument).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Extra system-prompt text appended to the child's role instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_system_prompt: Option<String>,
}

impl SubagentResolveDirective {
    /// Whether the directive substitutes anything at all.
    pub fn is_noop(&self) -> bool {
        self.subagent_type.is_none() && self.model.is_none() && self.extra_system_prompt.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn directive_round_trips() {
        let full = SubagentResolveDirective {
            subagent_type: Some("explore".into()),
            model: Some("grok-4.5".into()),
            extra_system_prompt: Some("Prefer read-only commands.".into()),
        };
        let value = serde_json::to_value(&full).unwrap();
        assert_eq!(
            value,
            json!({
                "subagent_type": "explore",
                "model": "grok-4.5",
                "extra_system_prompt": "Prefer read-only commands.",
            })
        );
        let back: SubagentResolveDirective = serde_json::from_value(value).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn empty_and_partial_replies_parse() {
        // Empty object -> no-op directive (passthrough semantics).
        let empty: SubagentResolveDirective = serde_json::from_value(json!({})).unwrap();
        assert!(empty.is_noop());
        assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));

        // Partial reply keeps unset fields None; unknown fields are ignored
        // (forward compat, no deny_unknown_fields).
        let partial: SubagentResolveDirective =
            serde_json::from_value(json!({ "model": "backup", "future_field": 1 })).unwrap();
        assert_eq!(partial.model.as_deref(), Some("backup"));
        assert!(partial.subagent_type.is_none());
        assert!(partial.extra_system_prompt.is_none());
        assert!(!partial.is_noop());
    }
}
