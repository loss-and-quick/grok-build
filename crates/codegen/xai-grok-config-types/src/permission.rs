//! Permission-policy config value types, extracted from xai-grok-shell so crates the shell depends on can use them.

use serde::{Deserialize, Serialize};

/// Permission policy configuration loaded from `[permission]` section in config.toml.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PermissionConfig {
    pub rules: Vec<PermissionRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub action: RuleAction,
    #[serde(default)]
    pub tool: ToolFilter,
    pub pattern: Option<String>,
    #[serde(default)]
    pub pattern_mode: PatternMode,
    /// Agents this rule is scoped to, matched by subagent type. Empty (the
    /// default) applies the rule to every agent; a non-empty list restricts it to
    /// requests from those agents. Mirrors the workspace policy engine's
    /// `PermissionRule::agents`; the root session matches the reserved name
    /// `"main"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PatternMode {
    #[default]
    Glob,
    /// Match against URL host rather than full string (from `WebFetch(domain:...)`).
    Domain,
}

/// Action to take when a rule matches.
///
/// The default is Deny (CWE-1188): omitting the `action` field in a TOML permission rule must not silently create a catch-all allow rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    Allow,
    #[default]
    Deny,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ToolFilter {
    #[default]
    Any,
    Bash,
    Edit,
    Read,
    Grep,
    Mcp,
    WebFetch,
    #[serde(rename = "agent_message", alias = "agentmessage")]
    AgentMessage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_message_wire_round_trips_and_unknown_is_rejected() {
        let filter: ToolFilter = serde_json::from_str(r#""agent_message""#).unwrap();
        assert_eq!(filter, ToolFilter::AgentMessage);
        assert_eq!(
            serde_json::to_string(&filter).unwrap(),
            r#""agent_message""#
        );
        assert!(serde_json::from_str::<ToolFilter>(r#""future_tool""#).is_err());
    }

    #[test]
    fn rule_without_agents_field_defaults_empty() {
        // Existing configs (no `agents`) parse unchanged with an empty scope.
        let rule: PermissionRule =
            serde_json::from_str(r#"{"action":"deny","tool":"bash","pattern":"rm*"}"#).unwrap();
        assert!(rule.agents.is_empty());
        assert_eq!(rule.action, RuleAction::Deny);
    }

    #[test]
    fn rule_with_agents_round_trips() {
        let rule: PermissionRule = serde_json::from_str(
            r#"{"action":"deny","tool":"bash","pattern":"rm*","agents":["explore","plan"]}"#,
        )
        .unwrap();
        assert_eq!(rule.agents, vec!["explore".to_string(), "plan".to_string()]);
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(json["agents"], serde_json::json!(["explore", "plan"]));
    }

    #[test]
    fn empty_agents_are_omitted_from_serialized_form() {
        let bare: PermissionRule =
            serde_json::from_str(r#"{"action":"allow","tool":"bash"}"#).unwrap();
        let json = serde_json::to_string(&bare).unwrap();
        assert!(
            !json.contains("agents"),
            "empty agents must not serialize: {json}"
        );
    }
}
