//! Post-plan-approval agent switch.
//!
//! After a plan is approved (`exit_plan_mode`), the session can hand off to a different agent
//! (e.g. a small "build" agent) instead of continuing on the planning agent — useful when a big
//! model plans and a small model executes. The behaviour is parameterised by the `[plan]` config
//! section (`switch_agent_on_approval` + `agent`); it never runs when the toggle is off.
//!
//! The switch is orchestrated entirely on the session actor: the `Agent` object is `!Send`, so it
//! cannot be rebuilt from the `MvpAgent`. The durable `post_approval_agent` state on the plan-mode
//! tracker makes the handoff survive restarts/crashes — [`Self::restore_plan_agent_switch`]
//! completes a `Pending` approval (no re-asking) and rebuilds the harness for an `Applied` one.
use super::*;
use xai_grok_agent::config::ModelOverride;

impl SessionActor {
    /// Resolve the target agent definition by name (discovery order: project, user, bundled, plugins).
    fn resolve_target_agent(&self, name: &str) -> Option<xai_grok_agent::AgentDefinition> {
        let cwd = self.tool_context.cwd.as_path();
        let registry = self.plugin_registry.borrow().clone();
        xai_grok_agent::discovery::by_name_in_cwd_with_plugins(name, cwd, registry.as_deref())
    }

    /// The model the target agent should run on: its pinned model when it is still catalog-live,
    /// otherwise the session's current model (`Inherit` pins, or a pin no longer in the catalog).
    fn resolve_target_model(&self, def: &xai_grok_agent::AgentDefinition) -> acp::ModelId {
        match &def.model {
            ModelOverride::Override(id) => {
                let mid = acp::ModelId::new(std::sync::Arc::from(id.as_str()));
                if self.models_manager.models().contains_key(mid.0.as_ref()) {
                    mid
                } else {
                    self.models_manager.current_model_id()
                }
            }
            ModelOverride::Inherit => self.models_manager.current_model_id(),
        }
    }

    /// Preserve the session's current effort when the target model supports it; otherwise fall
    /// back to the target model's default effort.
    async fn resolve_target_effort(
        &self,
        model_id: &acp::ModelId,
    ) -> Option<xai_grok_sampling_types::ReasoningEffort> {
        let current = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .and_then(|c| c.reasoning_effort);
        match current {
            Some(effort)
                if self
                    .models_manager
                    .model_supports_reasoning_effort(model_id.0.as_ref()) =>
            {
                Some(effort)
            }
            _ => self
                .models_manager
                .model_default_reasoning_effort(model_id.0.as_ref()),
        }
    }

    /// The effective `[plan]` config, read from the merged on-disk config. `load_effective_config`
    /// returns the whole config as a `toml::Value`; the `toml` crate here has no `serde` feature, so
    /// the `[plan]` table round-trips through JSON before deserialising into [`PlanConfig`].
    pub(super) fn plan_config() -> crate::config::PlanConfig {
        let Some(plan_value) = crate::config::load_effective_config()
            .ok()
            .and_then(|v| v.get("plan").cloned())
        else {
            return crate::config::PlanConfig::default();
        };
        Self::plan_config_from_value(&plan_value)
    }

    /// Deserialize a `[plan]` table into [`PlanConfig`]. The `toml` crate here has no `serde`
    /// feature, so the table round-trips through JSON before deserialising.
    fn plan_config_from_value(value: &toml::Value) -> crate::config::PlanConfig {
        let json = serde_json::to_value(value).unwrap_or_default();
        serde_json::from_value(json).unwrap_or_default()
    }

    /// Run the post-approval agent switch for `requested` (or the configured `[plan] agent`).
    ///
    /// Returns `true` when the harness and/or sampling config were actually switched. Promotes the
    /// durable tracker state `Pending` → `Applied` and publishes the switched values so agent-side
    /// reads (model state, subagent harness) observe them before the next reload.
    pub(crate) async fn apply_plan_agent_switch(self: &Arc<Self>, requested: Option<&str>) -> bool {
        let agent_name = match requested {
            Some(name) => name.to_string(),
            None => match Self::plan_config().resolve_approval_agent() {
                Some(name) => name,
                // Feature disarmed (or armed without a target agent): nothing to do.
                None => return false,
            },
        };
        let def = match self.resolve_target_agent(&agent_name) {
            Some(def) => def,
            None => {
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    agent = %agent_name,
                    "post-approval agent switch: could not resolve agent definition; continuing on current agent"
                );
                return false;
            }
        };
        let target_model = self.resolve_target_model(&def);
        let target_effort = self.resolve_target_effort(&target_model).await;
        let current_agent = self.active_agent_type.lock().clone();
        let need_rebuild = match current_agent.as_deref() {
            Some(active) => !crate::agent::mvp_agent::harnesses_are_compatible(active, &def.name),
            None => true,
        };
        if need_rebuild {
            if let Err(e) = self
                .handle_rebuild_agent_for_definition(def.clone(), false)
                .await
            {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    agent = %def.name,
                    error = %e,
                    "post-approval agent switch: harness rebuild failed"
                );
                return false;
            }
            tracing::info!(
                session_id = %self.session_info.id.0,
                agent = %def.name,
                "post-approval agent switch: harness rebuilt"
            );
        }
        // Point the live sampling config at the target model + effort.
        let is_session_based = self
            .auth_method_id
            .load()
            .as_deref()
            .is_some_and(crate::agent::auth_method::is_session_based_method);
        let origin = self.origin_client.clone();
        let mut sampler_cfg = match self.models_manager.models().get(target_model.0.as_ref()) {
            Some(entry) => {
                self.models_manager
                    .sampling_config_for_entry(is_session_based, entry, origin)
            }
            // Model not in the live catalog: keep the current auth, just move the model id.
            None => {
                let mut cfg = self.models_manager.sampling_config();
                cfg.model = target_model.0.to_string();
                cfg
            }
        };
        sampler_cfg.reasoning_effort = target_effort;
        let threshold = self
            .models_manager
            .auto_compact_threshold_percent_for(&target_model.0, None);
        let _ = self
            .handle_set_session_model(
                sampler_cfg,
                false, // use_concise
                false, // is_family_switch — no inline compaction for a handoff
                false, // apply_prompt_override — the rebuild already installed the new system head
                true,  // skip_prompt_rewrite
                threshold,
            )
            .await;
        // Publish the switched values so agent-side reads see them before reload.
        self.post_approval_switch.store(Some(std::sync::Arc::new(
            crate::session::handle::PostApprovalSwitchInfo {
                model_id: target_model,
                agent_name: self.agent.borrow().definition().name.clone(),
                reasoning_effort: target_effort,
            },
        )));
        // Promote the durable state Pending -> Applied so a restore rebuilds from this agent.
        {
            let mut tracker = self.plan_mode.lock();
            tracker.set_post_approval_agent_applied(&agent_name);
        }
        self.persist_plan_mode_state();
        true
    }

    /// Restore the durable post-approval switch on session start (before any turn runs):
    ///
    /// - `Pending` — the approval landed but the switch never ran (crash/restart); complete it now
    ///   without re-asking the user.
    /// - `Applied` — the session already ran on this agent; rebuild the harness so a restored
    ///   session lives on it rather than on whatever the spawn resolved to, and re-publish the
    ///   switched values.
    pub(crate) async fn restore_plan_agent_switch(self: &Arc<Self>) {
        // Read the durable state under the lock, then drop the guard before awaiting: the handoff
        // rebuilds the harness and republishes the cell, so holding the plan-mode mutex across
        // those awaits would risk contending with the approval path that promotes Pending -> Applied.
        let approval = {
            let plan_guard = self.plan_mode.lock();
            plan_guard.post_approval_agent().cloned()
        };
        let Some(approval) = approval else {
            return;
        };
        match approval {
            crate::session::plan_mode::PostApprovalAgent::Pending { agent } => {
                let _ = self.apply_plan_agent_switch(Some(&agent)).await;
            }
            crate::session::plan_mode::PostApprovalAgent::Applied { agent } => {
                if let Some(def) = self.resolve_target_agent(&agent) {
                    let current_agent = self.active_agent_type.lock().clone();
                    let need_rebuild = match current_agent.as_deref() {
                        Some(active) => {
                            !crate::agent::mvp_agent::harnesses_are_compatible(active, &agent)
                        }
                        None => true,
                    };
                    if need_rebuild
                        && let Err(e) = self.handle_rebuild_agent_for_definition(def, false).await
                    {
                        tracing::warn!(
                            session_id = %self.session_info.id.0,
                            agent = &agent,
                            error = %e,
                            "post-approval agent switch (restore): harness rebuild failed"
                        );
                    }
                    // The model + effort already match the applied agent (persisted sampling
                    // config); just re-publish them so reads reflect the applied harness.
                    let model_id = self.models_manager.current_model_id();
                    let reasoning_effort = self
                        .chat_state_handle
                        .get_sampling_config()
                        .await
                        .and_then(|c| c.reasoning_effort);
                    self.post_approval_switch.store(Some(std::sync::Arc::new(
                        crate::session::handle::PostApprovalSwitchInfo {
                            model_id,
                            agent_name: agent,
                            reasoning_effort,
                        },
                    )));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `[plan]` table the switch reads, deserialised through the same JSON round-trip as
    /// [`SessionActor::plan_config`].
    fn plan_config_from_toml(raw: &str) -> crate::config::PlanConfig {
        let value: toml::Value = toml::from_str(raw).unwrap();
        let Some(plan_value) = value.get("plan").cloned() else {
            return crate::config::PlanConfig::default();
        };
        crate::session::acp_session::SessionActor::plan_config_from_value(&plan_value)
    }

    #[test]
    fn armed_plan_section_resolves_the_approval_agent() {
        let config =
            plan_config_from_toml("[plan]\nswitch_agent_on_approval = true\nagent = \"build\"\n");
        assert_eq!(
            config.resolve_approval_agent().as_deref(),
            Some("build"),
            "an armed toggle with a target agent resolves to that agent",
        );
    }

    #[test]
    fn disarmed_toggle_never_resolves_even_with_a_target() {
        let config =
            plan_config_from_toml("[plan]\nswitch_agent_on_approval = false\nagent = \"build\"\n");
        assert_eq!(
            config.resolve_approval_agent(),
            None,
            "a disabled toggle stays off even when a target agent is set",
        );
    }

    #[test]
    fn armed_toggle_without_agent_disarms() {
        let config = plan_config_from_toml("[plan]\nswitch_agent_on_approval = true\n");
        assert_eq!(
            config.resolve_approval_agent(),
            None,
            "an armed toggle with no target agent logs and stays disabled",
        );
    }

    #[test]
    fn missing_plan_section_defaults_to_disarmed() {
        let config = plan_config_from_toml("[other]\nkey = \"value\"\n");
        assert_eq!(
            config,
            crate::config::PlanConfig::default(),
            "a config with no [plan] section deserialises to the disabled default",
        );
    }
}
