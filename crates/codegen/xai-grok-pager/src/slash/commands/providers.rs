use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Open the resolved model catalog, grouped by provider.
pub struct ProvidersCommand;

impl SlashCommand for ProvidersCommand {
    fn name(&self) -> &str {
        "providers"
    }

    fn description(&self) -> &str {
        "Show the resolved model catalog by provider"
    }

    fn usage(&self) -> &str {
        "/providers"
    }

    fn visible(&self, _ctx: &crate::slash::command::AppCtx) -> bool {
        // Always offered: an empty or still-loading catalog is itself worth
        // seeing, and the panel says so rather than refusing to open.
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::ToggleProviders)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    static DEFAULT_BUNDLE_STATE: BundleState = BundleState {
        has_cache: false,
        version: String::new(),
        personas: Vec::new(),
        roles: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        persona_details: Vec::new(),
        role_details: Vec::new(),
    };

    #[test]
    fn dispatches_toggle_providers() {
        let models = ModelState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Minimal,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        };
        assert!(matches!(
            ProvidersCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::ToggleProviders)
        ));
    }

    /// The panel is the place you go when the catalog looks wrong, so an empty
    /// catalog must not hide the command that would explain it.
    #[test]
    fn visible_even_with_an_empty_catalog() {
        let models = ModelState::default();
        let ctx = crate::slash::command::AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: false,
            screen_mode: crate::app::ScreenMode::Fullscreen,
        };
        assert!(ProvidersCommand.visible(&ctx));
    }
}
