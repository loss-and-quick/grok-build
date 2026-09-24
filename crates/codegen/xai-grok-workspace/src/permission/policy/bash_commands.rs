//! Configured Bash rules check every command, preserving deny and ask precedence.

use crate::permission::bash_command_splitting::{
    all_commands_from_script, normalize_command_words, try_parse_shell,
};
use crate::permission::bash_permission_script::PermissionScript;
use crate::permission::policy::{
    AllowRuleScope, CompiledPolicy, GateDecision, InlineShellScript, ShellWord,
    combine_gate_decisions, shell_dash_c_script,
};

impl CompiledPolicy {
    /// Recovered command projections and whether allow rules in `scope` cover every one; `None` when nothing parses.
    fn recovered_commands(
        &self,
        cmd: &str,
        agent: Option<&str>,
        scope: AllowRuleScope,
    ) -> Option<(Vec<Vec<String>>, bool)> {
        let tree = try_parse_shell(cmd)?;
        let script = PermissionScript::analyze(&tree, cmd);
        let projections = script.projections();
        let covered = script.is_eligible()
            && projections
                .iter()
                .all(|words| self.bash_words_allowed(words, agent, scope));
        Some((projections, covered))
    }

    pub(crate) fn evaluate_bash_command_segments(
        &self,
        cmd: &str,
        inline_depth_remaining: usize,
        agent: Option<&str>,
    ) -> Option<GateDecision> {
        let Some(segments) = all_commands_from_script(cmd) else {
            let Some((projections, covered)) =
                self.recovered_commands(cmd, agent, AllowRuleScope::Any)
            else {
                return Some(GateDecision::AskFailClosed);
            };
            let mut decision = (!covered).then_some(GateDecision::AskFailClosed);
            for words in &projections {
                decision = combine_gate_decisions(
                    decision,
                    self.evaluate_command_words(words, inline_depth_remaining, agent),
                );
            }
            return decision;
        };
        let mut decision = None;
        for parsed in &segments {
            decision = combine_gate_decisions(
                decision,
                self.evaluate_command_words(parsed.words(), inline_depth_remaining, agent),
            );
        }
        decision
    }

    /// `agent` scopes the allow rules considered, so an agent-scoped allow never
    /// grants a chain segment for a different agent; `scope` narrows *which*
    /// allow rules count (see [`AllowRuleScope`]). The two filters are
    /// independent and both must pass.
    pub(crate) fn bash_chain_fully_allowed(
        &self,
        cmd: &str,
        inline_depth_remaining: usize,
        agent: Option<&str>,
        scope: AllowRuleScope,
    ) -> bool {
        let Some(segments) = all_commands_from_script(cmd) else {
            return self
                .recovered_commands(cmd, agent, scope)
                .is_some_and(|(_, covered)| covered);
        };
        if segments.is_empty() {
            return false;
        }
        for parsed in &segments {
            let norm = normalize_command_words(parsed.words());
            if norm.exhausted
                || norm.ambiguous
                || norm.env_options_uncertain
                || norm.has_split_string
            {
                return false;
            }
            let inner_words = norm.words;
            if !self.bash_words_allowed(inner_words, agent, scope) {
                return false;
            }
            let shell_words: Vec<ShellWord<'_>> = inner_words.iter().map(ShellWord::from).collect();
            match shell_dash_c_script(&shell_words) {
                InlineShellScript::Literal(index) if inline_depth_remaining > 0 => {
                    let Some(inner) = inner_words.get(index) else {
                        return false;
                    };
                    if !self.bash_chain_fully_allowed(
                        inner.as_str(),
                        inline_depth_remaining - 1,
                        agent,
                        scope,
                    ) {
                        return false;
                    }
                }
                InlineShellScript::NotInline => {}
                _ => return false,
            }
        }
        true
    }
}

#[cfg(test)]
#[path = "bash_commands_tests.rs"]
mod tests;
