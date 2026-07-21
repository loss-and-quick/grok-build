//! Permission-policy config value types, extracted from xai-grok-shell
//! (config dependency inversion).

use serde::{Deserialize, Serialize};

/// Permission policy configuration loaded from `[permission]` section in config.toml.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PermissionConfig {
    pub rules: Vec<PermissionRule>,
}

/// A single permission rule.
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

/// Action to take when rule matches.
///
/// CWE-1188: Default changed from Allow to Deny so that omitting the
/// `action` field in a TOML permission rule does not silently create a
/// catch-all allow rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    Allow,
    #[default]
    Deny,
    Ask,
}

/// Tool filter for permission rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolFilter {
    #[default]
    Any,
    Bash,
    Edit,
    Read,
    Grep,
    Mcp,
    WebFetch,
}

#[cfg(test)]
mod tests {
    use super::*;

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
