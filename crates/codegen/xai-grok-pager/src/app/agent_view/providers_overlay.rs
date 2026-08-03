//! `/providers` overlay: open/close, keyboard and mouse routing.
//!
//! Mirrors `workflows_overlay`: the same Esc/q close, ↑↓/kj selection,
//! Left/Right folding and `handle_modal_mouse` chrome routing, so the panel
//! behaves like every other overlay rather than inventing a vocabulary.

use super::AgentView;
use crate::app::app_view::InputOutcome;
use crate::key;
use crate::views::providers::{ProviderGroup, build_groups, flatten};
use crossterm::event::{Event, KeyCode, KeyEventKind};

impl AgentView {
    /// Open the panel, resolving the post-override catalog once. The resolve
    /// reads config files, which is why it happens here (an explicit user
    /// action) and not per frame.
    pub(crate) fn open_providers_panel(&mut self) {
        self.providers_catalog =
            std::rc::Rc::new(crate::acp::resolved_catalog::ResolvedCatalog::load());
        self.providers_view.reset();
        self.show_providers = true;
    }

    pub(crate) fn close_providers_panel(&mut self) {
        self.show_providers = false;
    }

    /// Provider-grouped rows for the current catalog. Rebuilt per frame from
    /// the live `ModelState`, so a `models/update` broadcast is reflected
    /// without reopening the panel.
    pub(crate) fn providers_groups(&self) -> Vec<ProviderGroup> {
        build_groups(&self.session.models, &self.providers_catalog)
    }

    pub(super) fn handle_providers_overlay_input(&mut self, ev: &Event) -> Option<InputOutcome> {
        if !self.show_providers {
            return None;
        }
        let groups = self.providers_groups();

        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
        {
            // Ctrl+Q keeps its global meaning; any other modified chord is
            // swallowed so it cannot leak to the composer behind the panel.
            if key!('q', CONTROL).matches(key) {
                return Some(InputOutcome::Unchanged);
            }
            if !key.modifiers.is_empty() {
                return Some(InputOutcome::Changed);
            }
            let entries = flatten(&groups, &self.providers_view.collapsed);
            let count = entries.len();
            let outcome = match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.close_providers_panel();
                    InputOutcome::Changed
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.providers_view.move_by(-1, count);
                    InputOutcome::Changed
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.providers_view.move_by(1, count);
                    InputOutcome::Changed
                }
                KeyCode::PageUp => {
                    self.providers_view.move_by(-10, count);
                    InputOutcome::Changed
                }
                KeyCode::PageDown => {
                    self.providers_view.move_by(10, count);
                    InputOutcome::Changed
                }
                KeyCode::Home => {
                    self.providers_view.select(0, count);
                    InputOutcome::Changed
                }
                KeyCode::End => {
                    self.providers_view.select(count.saturating_sub(1), count);
                    InputOutcome::Changed
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    self.fold_selected_provider(&groups, true);
                    InputOutcome::Changed
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.fold_selected_provider(&groups, false);
                    InputOutcome::Changed
                }
                KeyCode::Enter => {
                    self.toggle_selected_provider(&groups);
                    InputOutcome::Changed
                }
                _ => InputOutcome::Changed,
            };
            return Some(outcome);
        }

        if let Event::Mouse(mouse) = ev {
            use crate::views::modal_window::{ModalWindowOutcome, handle_modal_mouse};

            let outcome = handle_modal_mouse(
                &mut self.providers_view.window,
                mouse.kind,
                mouse.column,
                mouse.row,
            );
            match outcome {
                ModalWindowOutcome::CloseRequested => {
                    self.close_providers_panel();
                    return Some(InputOutcome::Changed);
                }
                ModalWindowOutcome::Unhandled => {}
                _ => return Some(InputOutcome::Changed),
            }

            let entries = flatten(&groups, &self.providers_view.collapsed);
            if let Some(index) = self
                .providers_view
                .row_hits
                .iter()
                .find(|(rect, _)| {
                    mouse.column >= rect.x
                        && mouse.column < rect.x.saturating_add(rect.width)
                        && mouse.row >= rect.y
                        && mouse.row < rect.y.saturating_add(rect.height)
                })
                .map(|(_, index)| *index)
            {
                use crossterm::event::MouseEventKind;
                if matches!(mouse.kind, MouseEventKind::Down(_)) {
                    self.providers_view.select(index, entries.len());
                    return Some(InputOutcome::Changed);
                }
            }
            return Some(InputOutcome::Changed);
        }

        None
    }

    /// The provider group owning the selected entry, whether the selection is
    /// on the header itself or on one of its models.
    fn selected_group_id(&self, groups: &[ProviderGroup]) -> Option<String> {
        use crate::views::providers::PanelEntry;
        let entries = flatten(groups, &self.providers_view.collapsed);
        match entries.get(self.providers_view.selected)? {
            PanelEntry::Group { group } | PanelEntry::Model { group, .. } => {
                groups.get(*group).map(|g| g.id.clone())
            }
        }
    }

    fn fold_selected_provider(&mut self, groups: &[ProviderGroup], collapsed: bool) {
        if let Some(id) = self.selected_group_id(groups) {
            self.providers_view.set_collapsed(&id, collapsed);
            // Collapsing can shorten the entry list under the cursor.
            let entries = flatten(groups, &self.providers_view.collapsed);
            self.providers_view.clamp(entries.len());
        }
    }

    fn toggle_selected_provider(&mut self, groups: &[ProviderGroup]) {
        if let Some(id) = self.selected_group_id(groups) {
            self.providers_view.toggle_collapsed(&id);
            let entries = flatten(groups, &self.providers_view.collapsed);
            self.providers_view.clamp(entries.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::agent_view::test_agent_view;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn key_event(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn agent_with_two_providers() -> AgentView {
        let mut agent = test_agent_view(None, std::path::PathBuf::from("."));
        let raw: toml::Value = toml::from_str(
            r#"
            [[provider]]
            id = "acme"
            base_url = "https://api.example.test/v1"
            models = ["some-model", "other-model"]

            [[provider]]
            id = "example-provider"
            base_url = "https://api.example.test/v1"
            models = ["some-model"]
            "#,
        )
        .expect("fixture parses");
        let cfg = xai_grok_shell::agent::config::Config::new_from_toml_cfg(&raw)
            .expect("fixture builds a config");
        agent.providers_catalog = std::rc::Rc::new(
            crate::acp::resolved_catalog::ResolvedCatalog::from_agent_config(&cfg),
        );
        for key in [
            "acme/some-model",
            "acme/other-model",
            "example-provider/some-model",
        ] {
            let id = agent_client_protocol::ModelId::new(std::sync::Arc::from(key));
            agent.session.models.available.insert(
                id.clone(),
                agent_client_protocol::ModelInfo::new(id, key.to_string()),
            );
        }
        agent.show_providers = true;
        agent
    }

    #[test]
    fn esc_and_q_close_the_panel() {
        for code in [KeyCode::Esc, KeyCode::Char('q')] {
            let mut agent = agent_with_two_providers();
            let outcome = agent.handle_providers_overlay_input(&key_event(code));
            assert!(matches!(outcome, Some(InputOutcome::Changed)));
            assert!(!agent.show_providers, "{code:?} should close the panel");
        }
    }

    #[test]
    fn arrows_move_the_selection_and_stop_at_the_ends() {
        let mut agent = agent_with_two_providers();
        // 2 group headers + 3 models.
        assert_eq!(agent.providers_groups().len(), 2);
        assert_eq!(agent.providers_view.selected, 0);
        for _ in 0..10 {
            agent.handle_providers_overlay_input(&key_event(KeyCode::Down));
        }
        assert_eq!(
            agent.providers_view.selected, 4,
            "clamped to the last entry"
        );
        for _ in 0..10 {
            agent.handle_providers_overlay_input(&key_event(KeyCode::Up));
        }
        assert_eq!(agent.providers_view.selected, 0);
    }

    #[test]
    fn left_folds_the_owning_provider_and_right_unfolds_it() {
        let mut agent = agent_with_two_providers();
        // Select a model row, then fold from it — the group, not the row, folds.
        agent.handle_providers_overlay_input(&key_event(KeyCode::Down));
        agent.handle_providers_overlay_input(&key_event(KeyCode::Left));
        assert!(agent.providers_view.is_collapsed("acme"));
        let groups = agent.providers_groups();
        assert_eq!(flatten(&groups, &agent.providers_view.collapsed).len(), 3);
        // The cursor was inside the folded group; it must stay in range.
        assert!(agent.providers_view.selected < 3);

        agent.handle_providers_overlay_input(&key_event(KeyCode::Home));
        agent.handle_providers_overlay_input(&key_event(KeyCode::Right));
        assert!(!agent.providers_view.is_collapsed("acme"));
        assert_eq!(flatten(&groups, &agent.providers_view.collapsed).len(), 5);
    }

    #[test]
    fn input_is_ignored_when_the_panel_is_closed() {
        let mut agent = agent_with_two_providers();
        agent.show_providers = false;
        assert!(
            agent
                .handle_providers_overlay_input(&key_event(KeyCode::Down))
                .is_none()
        );
    }
}
