//! AgentView glue for plugin UI panels: the `(plugin, id)`-keyed panel map,
//! merge-on-republish receive path, overlay input routing, the compact
//! status-bar chip, and the full-screen overlay draw. The per-panel
//! interaction state + block rendering lives in
//! [`crate::views::plugin_panel`].

use super::AgentView;
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::theme::Theme;
use crate::views::plugin_panel::{PanelKeyOutcome, PanelState};
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};
use xai_grok_plugin_protocol::PanelViewModel;

impl AgentView {
    /// Store or merge a panel published by `plugin`, keyed by
    /// `(plugin, view_model.id)`. On re-publish of an existing key the live
    /// [`PanelState`] is merged so in-progress input survives; a genuinely new
    /// panel becomes the active one if none is active yet. Does NOT open the
    /// overlay — a fresh panel shows in the sidebar until the user opens it.
    pub(crate) fn apply_plugin_panel(&mut self, plugin: String, view_model: PanelViewModel) -> bool {
        let key = (plugin, view_model.id.clone());
        match self.plugin_panels.get_mut(&key) {
            Some(state) => state.merge(view_model),
            None => {
                self.plugin_panels
                    .insert(key.clone(), PanelState::from_view_model(view_model));
            }
        }
        if self.active_plugin_panel.is_none() {
            self.active_plugin_panel = Some(key);
        }
        true
    }

    /// Remove the panel keyed by `(plugin, id)`. Re-points the active panel and
    /// closes the overlay when the last panel goes away.
    pub(crate) fn remove_plugin_panel(&mut self, plugin: &str, id: &str) -> bool {
        let key = (plugin.to_string(), id.to_string());
        if self.plugin_panels.shift_remove(&key).is_none() {
            return false;
        }
        if self.active_plugin_panel.as_ref() == Some(&key) {
            self.active_plugin_panel = self.plugin_panels.keys().next().cloned();
        }
        if self.plugin_panels.is_empty() {
            self.plugin_panel_overlay_open = false;
        }
        true
    }

    /// Toggle the full-screen overlay. No-op when there are no panels.
    pub(crate) fn toggle_plugin_panel_overlay(&mut self) {
        if self.plugin_panels.is_empty() {
            self.plugin_panel_overlay_open = false;
            return;
        }
        self.plugin_panel_overlay_open = !self.plugin_panel_overlay_open;
        if self.plugin_panel_overlay_open && self.active_plugin_panel.is_none() {
            self.active_plugin_panel = self.plugin_panels.keys().next().cloned();
        }
    }

    /// True when the overlay should own input and render.
    pub(crate) fn plugin_panel_overlay_active(&self) -> bool {
        self.plugin_panel_overlay_open && !self.plugin_panels.is_empty()
    }

    fn switch_active_panel(&mut self, delta: isize) {
        if self.plugin_panels.len() <= 1 {
            return;
        }
        let Some(active) = self.active_plugin_panel.clone() else {
            return;
        };
        if let Some(idx) = self.plugin_panels.get_index_of(&active) {
            let n = self.plugin_panels.len() as isize;
            let next = ((idx as isize + delta) % n + n) % n;
            if let Some((k, _)) = self.plugin_panels.get_index(next as usize) {
                self.active_plugin_panel = Some(k.clone());
            }
        }
    }

    /// Route a key to the open overlay. On button activation, collects the
    /// current text of every Input in the active panel and returns an
    /// [`Action::PluginPanelAction`] that the router turns into the
    /// `x.ai/plugins/panel_action` request back to the owning plugin.
    pub(crate) fn handle_plugin_panel_key(&mut self, key: &KeyEvent) -> InputOutcome {
        let Some(active) = self.active_plugin_panel.clone() else {
            self.plugin_panel_overlay_open = false;
            return InputOutcome::Changed;
        };
        let outcome = match self.plugin_panels.get_mut(&active) {
            Some(state) => state.handle_key(key),
            None => {
                self.plugin_panel_overlay_open = false;
                return InputOutcome::Changed;
            }
        };
        match outcome {
            PanelKeyOutcome::Ignored => InputOutcome::Unchanged,
            PanelKeyOutcome::Changed => InputOutcome::Changed,
            PanelKeyOutcome::Close => {
                self.plugin_panel_overlay_open = false;
                InputOutcome::Changed
            }
            PanelKeyOutcome::PrevPanel => {
                self.switch_active_panel(-1);
                InputOutcome::Changed
            }
            PanelKeyOutcome::NextPanel => {
                self.switch_active_panel(1);
                InputOutcome::Changed
            }
            PanelKeyOutcome::Activate { button_id } => {
                let (plugin, panel_id) = active.clone();
                let inputs = self
                    .plugin_panels
                    .get(&active)
                    .map(|s| s.collect_inputs())
                    .unwrap_or_default();
                InputOutcome::Action(Action::PluginPanelAction {
                    plugin,
                    panel_id,
                    button_id,
                    inputs,
                })
            }
        }
    }

    /// Route a bracketed paste to the open overlay's focused input.
    pub(crate) fn handle_plugin_panel_paste(&mut self, text: &str) -> InputOutcome {
        let Some(active) = self.active_plugin_panel.clone() else {
            return InputOutcome::Unchanged;
        };
        match self.plugin_panels.get_mut(&active).map(|s| s.handle_paste(text)) {
            Some(PanelKeyOutcome::Changed) => InputOutcome::Changed,
            _ => InputOutcome::Unchanged,
        }
    }

    /// Compact status-bar chip summarising active panels. `None` (and thus no
    /// change to the shared status bar) when there are no panels.
    pub(crate) fn plugin_panel_status_chip(&self, theme: &Theme) -> Option<Line<'static>> {
        if self.plugin_panels.is_empty() {
            return None;
        }
        let count = self.plugin_panels.len();
        let title = self
            .active_plugin_panel
            .as_ref()
            .and_then(|k| self.plugin_panels.get(k))
            .map(|s| s.title().to_string())
            .unwrap_or_default();
        let title: String = title.chars().take(20).collect();
        let label = if count > 1 {
            format!("\u{25a4} {title} (+{})", count - 1)
        } else {
            format!("\u{25a4} {title}")
        };
        let style = Style::default().fg(theme.accent_skill).bg(theme.bg_base);
        Some(Line::from(Span::styled(label, style)))
    }

    /// Draw the full-screen plugin-panel overlay. Returns the popup rect so the
    /// caller can register it as a link occluder. Assumes
    /// [`Self::plugin_panel_overlay_active`] is true.
    pub(crate) fn draw_plugin_panel_overlay(&mut self, area: Rect, buf: &mut Buffer) -> Rect {
        let theme = Theme::current();
        let width = area.width.saturating_sub(6).clamp(20, 96).min(area.width);
        let height = area.height.saturating_sub(4).clamp(8, 30).min(area.height);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        Clear.render(rect, buf);

        let active = self.active_plugin_panel.clone();
        let active_title = active
            .as_ref()
            .and_then(|k| self.plugin_panels.get(k))
            .map(|s| s.title().to_string())
            .unwrap_or_default();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent_skill))
            .title(Span::styled(
                format!(" Plugin panel: {active_title} "),
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.bg_dark));
        let inner = block.inner(rect);
        block.render(rect, buf);
        if inner.width == 0 || inner.height == 0 {
            return rect;
        }

        let mut content = inner;

        // Switcher row when more than one panel exists.
        if self.plugin_panels.len() > 1 {
            let mut spans = Vec::new();
            for (k, state) in self.plugin_panels.iter() {
                let is_active = Some(k) == active.as_ref();
                let style = if is_active {
                    Style::default()
                        .fg(theme.bg_base)
                        .bg(theme.accent_skill)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_secondary).bg(theme.bg_dark)
                };
                spans.push(Span::styled(format!(" {} ", state.title()), style));
                spans.push(Span::raw(" "));
            }
            buf.set_line(content.x, content.y, &Line::from(spans), content.width);
            content.y = content.y.saturating_add(2);
            content.height = content.height.saturating_sub(2);
        }

        // Footer hint row.
        if content.height > 1 {
            let hint_y = content.y + content.height - 1;
            let hint = Line::from(Span::styled(
                "Esc/q close  \u{2022}  Tab focus  \u{2022}  \u{2190}/\u{2192} panel  \u{2022}  Enter activate",
                Style::default().fg(theme.gray_dim),
            ));
            buf.set_line(content.x, hint_y, &hint, content.width);
            content.height = content.height.saturating_sub(2);
        }

        if let Some(key) = active
            && content.height > 0
            && let Some(state) = self.plugin_panels.get_mut(&key)
        {
            state.render_content(content, buf, &theme);
        }
        rect
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_fixtures::make_agent;
    use super::AgentView;
    use crate::actions::{ActionId, ActionRegistry, When};
    use crate::app::actions::Action;
    use crate::app::app_view::InputOutcome;
    use crate::key;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use xai_grok_plugin_protocol::{
        PanelBlock, PanelButton, PanelStatusItem, PanelTone, PanelViewModel,
    };

    fn ev(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn ch(c: char) -> Event {
        ev(KeyCode::Char(c))
    }

    fn vm(id: &str, title: &str, blocks: Vec<PanelBlock>) -> PanelViewModel {
        PanelViewModel {
            id: id.into(),
            title: title.into(),
            blocks,
        }
    }

    /// Status + selectable Table + plain Input + secret Input + Actions.
    fn sample_blocks() -> Vec<PanelBlock> {
        vec![
            PanelBlock::Status {
                items: vec![PanelStatusItem {
                    label: "state".into(),
                    value: "idle".into(),
                    tone: PanelTone::Success,
                }],
            },
            PanelBlock::Table {
                columns: vec!["name".into(), "val".into()],
                rows: vec![
                    vec!["alpha".into(), "1".into()],
                    vec!["bravo".into(), "2".into()],
                    vec!["carol".into(), "3".into()],
                ],
                selectable: true,
            },
            PanelBlock::Input {
                id: "code".into(),
                label: "Code".into(),
                placeholder: Some("enter".into()),
                value: None,
                secret: false,
            },
            PanelBlock::Input {
                id: "pass".into(),
                label: "Pass".into(),
                placeholder: None,
                value: None,
                secret: true,
            },
            PanelBlock::Actions {
                buttons: vec![PanelButton {
                    id: "submit".into(),
                    label: "Submit".into(),
                    key: None,
                }],
            },
        ]
    }

    fn render_overlay_text(agent: &mut AgentView) -> String {
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        agent.draw_plugin_panel_overlay(area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[test]
    fn publish_stores_under_plugin_key_latest_wins() {
        let mut agent = make_agent();
        assert!(agent.apply_plugin_panel("council".into(), vm("p1", "First", vec![])));
        let key = ("council".to_string(), "p1".to_string());
        assert!(agent.plugin_panels.contains_key(&key));
        assert_eq!(agent.plugin_panels.len(), 1);

        // Same (plugin, id): latest-wins in place, not a second entry.
        agent.apply_plugin_panel("council".into(), vm("p1", "Second", vec![]));
        assert_eq!(agent.plugin_panels.len(), 1);
        assert_eq!(agent.plugin_panels.get(&key).unwrap().title(), "Second");

        // Same id under a DIFFERENT plugin is a distinct panel.
        agent.apply_plugin_panel("other".into(), vm("p1", "Other", vec![]));
        assert_eq!(agent.plugin_panels.len(), 2);
    }

    #[test]
    fn panel_closed_removes_only_that_key() {
        let mut agent = make_agent();
        agent.apply_plugin_panel("a".into(), vm("p", "A", vec![]));
        agent.apply_plugin_panel("b".into(), vm("p", "B", vec![]));
        assert!(agent.remove_plugin_panel("a", "p"));
        assert_eq!(agent.plugin_panels.len(), 1);
        assert!(agent.plugin_panels.contains_key(&("b".to_string(), "p".to_string())));
    }

    #[test]
    fn typed_input_survives_republish_through_agent() {
        let mut agent = make_agent();
        agent.apply_plugin_panel("council".into(), vm("p", "T", sample_blocks()));
        agent.toggle_plugin_panel_overlay();
        assert!(agent.plugin_panel_overlay_open);
        let reg = ActionRegistry::non_vscode_for_test();
        // Focus order: Table(0), Input code(1), Input pass(2), Button(3).
        agent.handle_input(&ev(KeyCode::Tab), &reg); // -> code
        for c in "hello".chars() {
            agent.handle_input(&ch(c), &reg);
        }
        // Re-publish with a changed status chip; input ids unchanged.
        let mut blocks = sample_blocks();
        if let PanelBlock::Status { items } = &mut blocks[0] {
            items[0].value = "busy".into();
        }
        agent.apply_plugin_panel("council".into(), vm("p", "T", blocks));
        let text = render_overlay_text(&mut agent);
        assert!(
            text.contains("hello"),
            "typed input must survive a wholesale re-publish:\n{text}"
        );
    }

    #[test]
    fn render_masks_secret_and_marks_selected_row() {
        let mut agent = make_agent();
        agent.apply_plugin_panel("council".into(), vm("p", "T", sample_blocks()));
        agent.toggle_plugin_panel_overlay();
        let reg = ActionRegistry::non_vscode_for_test();
        agent.handle_input(&ev(KeyCode::Tab), &reg); // code
        agent.handle_input(&ev(KeyCode::Tab), &reg); // pass (secret)
        for c in "abcd".chars() {
            agent.handle_input(&ch(c), &reg);
        }
        let text = render_overlay_text(&mut agent);
        assert!(text.contains('\u{2022}'), "secret must render masked:\n{text}");
        assert!(
            !text.contains("abcd"),
            "secret raw value must never render:\n{text}"
        );
        assert!(
            text.contains('\u{25b8}'),
            "selected table row must show the marker:\n{text}"
        );
    }

    #[test]
    fn jk_moves_table_selection_marker() {
        let mut agent = make_agent();
        agent.apply_plugin_panel("council".into(), vm("p", "T", sample_blocks()));
        agent.toggle_plugin_panel_overlay();
        let reg = ActionRegistry::non_vscode_for_test();
        // Focus defaults to the table; the marker starts on row `alpha`.
        let before = render_overlay_text(&mut agent);
        let marked_before = before.lines().find(|l| l.contains('\u{25b8}')).unwrap_or("");
        assert!(marked_before.contains("alpha"));
        agent.handle_input(&ch('j'), &reg);
        let after = render_overlay_text(&mut agent);
        let marked_after = after.lines().find(|l| l.contains('\u{25b8}')).unwrap_or("");
        assert!(
            marked_after.contains("bravo"),
            "j must move the selection to the next row:\n{after}"
        );
    }

    #[test]
    fn f6_bound_and_ctrl_p_untouched() {
        let reg = ActionRegistry::non_vscode_for_test();
        assert_eq!(
            reg.lookup(&key!(F(6)).to_key_event(), When::AgentScreen),
            Some(ActionId::TogglePluginPanels)
        );
        assert_eq!(
            reg.lookup(&key!(F(6)).to_key_event(), When::Always),
            None,
            "F6 must be free at When::Always too"
        );
        // Ctrl+P is still the command palette — not stolen.
        assert_eq!(
            reg.lookup(&key!('p', CONTROL).to_key_event(), When::AgentScreen),
            Some(ActionId::CommandPalette)
        );
    }

    #[test]
    fn f6_action_toggles_and_esc_closes() {
        let mut agent = make_agent();
        agent.apply_plugin_panel("council".into(), vm("p", "T", sample_blocks()));
        assert!(matches!(
            agent.handle_agent_action(ActionId::TogglePluginPanels),
            InputOutcome::Action(Action::TogglePluginPanels)
        ));
        agent.toggle_plugin_panel_overlay();
        assert!(agent.plugin_panel_overlay_open);
        let reg = ActionRegistry::non_vscode_for_test();
        agent.handle_input(&ev(KeyCode::Esc), &reg);
        assert!(
            !agent.plugin_panel_overlay_open,
            "Esc must close the overlay"
        );
    }

    #[test]
    fn button_activation_routes_action_with_typed_inputs() {
        let mut agent = make_agent();
        let blocks = vec![
            PanelBlock::Input {
                id: "code".into(),
                label: "Code".into(),
                placeholder: None,
                value: None,
                secret: false,
            },
            PanelBlock::Actions {
                buttons: vec![PanelButton {
                    id: "submit".into(),
                    label: "Submit".into(),
                    key: None,
                }],
            },
        ];
        agent.apply_plugin_panel("council".into(), vm("panel-1", "T", blocks));
        agent.toggle_plugin_panel_overlay();
        let reg = ActionRegistry::non_vscode_for_test();
        // Focus 0 = Input(code).
        for c in "xyz".chars() {
            agent.handle_input(&ch(c), &reg);
        }
        agent.handle_input(&ev(KeyCode::Tab), &reg); // -> button
        let out = agent.handle_input(&ev(KeyCode::Enter), &reg);
        match out {
            InputOutcome::Action(Action::PluginPanelAction {
                plugin,
                panel_id,
                button_id,
                inputs,
            }) => {
                assert_eq!(plugin, "council");
                assert_eq!(panel_id, "panel-1");
                assert_eq!(button_id, "submit");
                assert_eq!(inputs.get("code").map(String::as_str), Some("xyz"));
            }
            other => panic!("expected PluginPanelAction, got {other:?}"),
        }
    }
}
