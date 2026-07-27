//! Ratatui layout and input mapping for the login-method picker.
//!
//! Everything here is presentation *mechanics*: where rows land on screen and
//! which key or click maps to which outcome. What a row says — its badge, its
//! right-hand label, which row starts selected — is decided in
//! [`super::state`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use crate::theme::Theme;
use crate::views::auth_method_modal::state::{AuthMethodPickerOutcome, AuthMethodPickerState};
use crate::views::modal_window::{
    ModalSizing, ModalWindowConfig, ModalWindowOutcome, Shortcut, handle_modal_key,
    handle_modal_mouse, render_modal_window,
};
use crate::views::picker::{
    PickerEntry, PickerField, PickerHitAreas, PickerRow, render_picker_content_with_scrollbar_x,
};

const TITLE: &str = "Choose login method";

/// Footer hints. None are clickable, so the ids are never dispatched.
fn shortcuts() -> [Shortcut<'static>; 3] {
    [
        Shortcut {
            label: "↑/↓ move",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Enter sign in",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Esc cancel",
            clickable: false,
            id: 0,
        },
    ]
}

fn chrome_config<'a>(shortcuts: &'a [Shortcut<'a>], compact: bool) -> ModalWindowConfig<'a> {
    ModalWindowConfig {
        title: TITLE,
        tabs: None,
        shortcuts,
        sizing: ModalSizing::medium().with_compact(compact),
        // No collapsible rows: Left/Right stay unhandled.
        fold_info: None,
    }
}

impl AuthMethodPickerState {
    /// Map a key press onto an outcome. Callers filter key *releases*.
    pub fn handle_key(&mut self, key: &KeyEvent) -> AuthMethodPickerOutcome {
        let hints = shortcuts();
        match handle_modal_key(&mut self.window, key, &chrome_config(&hints, false)) {
            ModalWindowOutcome::CloseRequested => return AuthMethodPickerOutcome::Cancelled,
            ModalWindowOutcome::Handled => return AuthMethodPickerOutcome::Changed,
            _ => {}
        }
        if (key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT))
            && !crate::input::key::is_altgr(key.modifiers)
        {
            return AuthMethodPickerOutcome::Unchanged;
        }
        match key.code {
            KeyCode::Enter => self.choose(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
                AuthMethodPickerOutcome::Changed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                AuthMethodPickerOutcome::Changed
            }
            _ => AuthMethodPickerOutcome::Unchanged,
        }
    }

    /// Map a mouse event onto an outcome. Clicking a row selects it and signs
    /// in with it; the hit areas come from the last render.
    pub fn handle_mouse(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
    ) -> AuthMethodPickerOutcome {
        match handle_modal_mouse(&mut self.window, kind, column, row) {
            ModalWindowOutcome::CloseRequested => return AuthMethodPickerOutcome::Cancelled,
            ModalWindowOutcome::Handled => return AuthMethodPickerOutcome::Changed,
            _ => {}
        }
        let target = self.entry_at(column, row);
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match target {
                Some(idx) => {
                    self.set_selected(idx);
                    self.choose()
                }
                None => AuthMethodPickerOutcome::Unchanged,
            },
            MouseEventKind::Moved => {
                if self.picker.hovered == target {
                    return AuthMethodPickerOutcome::Unchanged;
                }
                self.picker.hovered = target;
                AuthMethodPickerOutcome::Changed
            }
            _ => AuthMethodPickerOutcome::Unchanged,
        }
    }

    fn choose(&mut self) -> AuthMethodPickerOutcome {
        match self.selected_method_id() {
            Some(method_id) => AuthMethodPickerOutcome::Chosen(method_id),
            None => AuthMethodPickerOutcome::Unchanged,
        }
    }

    /// Entry index under the given screen cell, from the last render's hit
    /// areas. `None` before the first draw or when the cell is not a row.
    fn entry_at(&self, column: u16, row: u16) -> Option<usize> {
        let hit = self.picker.hit_areas.as_ref()?;
        let pos = Position::new(column, row);
        hit.item_rects
            .iter()
            .position(|rect| rect.contains(pos))
            .and_then(|visual| hit.entry_indices.get(visual).copied())
    }
}

/// Draw the picker over the welcome screen.
pub fn render_auth_method_picker(
    buf: &mut Buffer,
    area: Rect,
    state: &mut AuthMethodPickerState,
    compact: bool,
    theme: &Theme,
) {
    let hints = shortcuts();
    let config = chrome_config(&hints, compact);
    let Some(modal) = render_modal_window(buf, area, &mut state.window, &config, theme) else {
        // Too small to draw: drop stale hit areas so a click cannot land on a
        // row that is no longer on screen.
        state.picker.hit_areas = None;
        return;
    };

    // Split the borrow: the row slices borrow `entries` while the list widget
    // needs `&mut picker`.
    let AuthMethodPickerState {
        entries,
        selected,
        picker,
        ..
    } = state;

    let fields: Vec<Vec<PickerField<'_>>> = entries
        .iter()
        .map(|entry| {
            entry
                .details
                .iter()
                .map(|detail| PickerField {
                    label: detail.label.as_str(),
                    value: detail.value.as_str(),
                })
                .collect()
        })
        .collect();

    let rows: Vec<PickerEntry<'_>> = entries
        .iter()
        .zip(fields.iter())
        .enumerate()
        .map(|(idx, (entry, fields))| {
            PickerEntry::Row(PickerRow {
                label: entry.provider_label.as_str(),
                right_label: entry.right_label(),
                selected: idx == *selected,
                expanded: idx == *selected,
                fields,
                description_lines: &[],
                summary_lines: &[],
                dimmed: false,
                indent: 0,
                badge: entry.badge(),
                badge_color: None,
                collapsible: false,
                underline_last_desc: false,
            })
        })
        .collect();

    let non_selectable = vec![false; rows.len()];
    let content_hit = render_picker_content_with_scrollbar_x(
        buf,
        modal.content,
        theme,
        picker,
        &rows,
        &non_selectable,
        &[],
        Some(theme.bg_base),
        false,
        modal.inner_x + modal.inner_width.saturating_sub(1),
    );
    // Persist the hit areas: the next click is tested against them.
    picker.hit_areas = Some(PickerHitAreas {
        close_button: Rect::default(),
        search_bar: Rect::default(),
        item_rects: content_hit.item_rects,
        entry_indices: content_hit.entry_indices,
        tab_rects: Vec::new(),
        filter_rect: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol as acp;

    fn method(id: &str, name: &str) -> acp::AuthMethod {
        acp::AuthMethod::Agent(acp::AuthMethodAgent::new(
            acp::AuthMethodId::new(id),
            name.to_string(),
        ))
    }

    fn picker() -> AuthMethodPickerState {
        AuthMethodPickerState::new(
            &[
                method("grok.com", "xAI"),
                method("plugin-oauth:example-auth", "Acme OAuth"),
            ],
            None,
            None,
            false,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_chooses_the_selected_method() {
        let mut state = picker();
        state.set_selected(1);
        assert_eq!(
            state.handle_key(&key(KeyCode::Enter)),
            AuthMethodPickerOutcome::Chosen(acp::AuthMethodId::new("plugin-oauth:example-auth"))
        );
    }

    #[test]
    fn esc_cancels() {
        let mut state = picker();
        assert_eq!(
            state.handle_key(&key(KeyCode::Esc)),
            AuthMethodPickerOutcome::Cancelled
        );
    }

    #[test]
    fn vim_and_arrow_keys_both_navigate() {
        let mut state = picker();
        assert_eq!(
            state.handle_key(&key(KeyCode::Char('j'))),
            AuthMethodPickerOutcome::Changed
        );
        assert_eq!(state.selected, 1);
        assert_eq!(
            state.handle_key(&key(KeyCode::Up)),
            AuthMethodPickerOutcome::Changed
        );
        assert_eq!(state.selected, 0);
        assert_eq!(
            state.handle_key(&key(KeyCode::Char('k'))),
            AuthMethodPickerOutcome::Changed
        );
        assert_eq!(state.selected, 1);
    }

    /// A click can only work if the hit areas recorded by the renderer are
    /// still there on the next event — i.e. the picker state is persisted
    /// rather than rebuilt per frame.
    #[test]
    fn click_on_a_row_selects_and_chooses_it() {
        let mut state = picker();
        let theme = Theme::current();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        render_auth_method_picker(&mut buf, area, &mut state, false, &theme);

        let hit = state
            .picker
            .hit_areas
            .as_ref()
            .expect("render must record row hit areas");
        let visual = hit
            .entry_indices
            .iter()
            .position(|&idx| idx == 1)
            .expect("second entry must be clickable");
        let rect = hit.item_rects[visual];

        let outcome =
            state.handle_mouse(MouseEventKind::Down(MouseButton::Left), rect.x + 1, rect.y);
        assert_eq!(state.selected, 1);
        assert_eq!(
            outcome,
            AuthMethodPickerOutcome::Chosen(acp::AuthMethodId::new("plugin-oauth:example-auth"))
        );
    }

    #[test]
    fn click_outside_the_popup_cancels() {
        let mut state = picker();
        let theme = Theme::current();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        render_auth_method_picker(&mut buf, area, &mut state, false, &theme);

        assert_eq!(
            state.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
            AuthMethodPickerOutcome::Cancelled
        );
    }
}
