//! Plugin UI-panel overlay: pager-owned interaction state and rendering for the
//! declarative [`PanelViewModel`]s plugins publish.
//!
//! A plugin re-publishes its whole panel on every change (a status tick, a
//! timer). The pager owns all interaction state — one [`LineEditor`] per Input
//! block, per-table selection/scroll, and the focus target — so re-publishing
//! must *preserve* that state rather than rebuild it (see [`PanelState::merge`]).
//!
//! The AgentView glue (state map, key routing, overlay/sidebar draw, and the
//! action-back Effect) lives in `crate::app::agent_view::plugin_panel`.

use std::collections::{BTreeMap, HashMap};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use xai_grok_plugin_protocol::{PanelBlock, PanelTone, PanelViewModel};

use crate::input::line_editor::{LineEditOutcome, LineEditor};
use crate::scrollback::blocks::markdown_content::MarkdownContent;
use crate::theme::Theme;

/// One focusable element within a panel, in top-to-bottom order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Focusable {
    /// A selectable table, identified by its block index.
    Table(usize),
    /// An input field (block index, input id).
    Input(usize, String),
    /// A button (Actions block index, button index within it).
    Button(usize, usize),
}

/// Outcome of routing a key to the active panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PanelKeyOutcome {
    /// Not handled by the panel.
    Ignored,
    /// Panel state changed; redraw.
    Changed,
    /// Close the overlay.
    Close,
    /// Switch to the previous sibling panel.
    PrevPanel,
    /// Switch to the next sibling panel.
    NextPanel,
    /// Activate a button — routes an action back to the owning plugin.
    Activate { button_id: String },
}

/// Per-table selection + scroll, keyed by the table's block index.
#[derive(Debug, Clone, Copy, Default)]
struct TableSel {
    selected: usize,
    offset: usize,
}

/// Pager-owned interaction state for one plugin panel.
pub(crate) struct PanelState {
    view_model: PanelViewModel,
    /// Editable field state, keyed by Input block `id`. Preserved across a
    /// re-publish so in-progress typing survives (see [`Self::merge`]).
    inputs: HashMap<String, LineEditor>,
    /// Table selection/scroll, keyed by block index.
    tables: HashMap<usize, TableSel>,
    /// Index into [`Self::focusables`] of the focused element.
    focus: usize,
}

impl PanelState {
    /// Build fresh interaction state for a newly published panel: one editor per
    /// Input seeded from the block's `value`.
    pub(crate) fn from_view_model(view_model: PanelViewModel) -> Self {
        let mut inputs = HashMap::new();
        for block in &view_model.blocks {
            if let PanelBlock::Input { id, value, .. } = block {
                let mut editor = LineEditor::default();
                if let Some(v) = value {
                    editor.set_text(v.clone());
                }
                inputs.insert(id.clone(), editor);
            }
        }
        Self {
            view_model,
            inputs,
            tables: HashMap::new(),
            focus: 0,
        }
    }

    /// Apply a re-published view model, PRESERVING in-progress state.
    ///
    /// The single most important correctness property of this layer: for every
    /// Input whose `id` still exists we keep the existing [`LineEditor`]
    /// untouched (text + cursor), discarding the block's `value`. A fresh editor
    /// is built only for genuinely-new input ids. Selection/scroll and focus are
    /// preserved and clamped to the new bounds.
    pub(crate) fn merge(&mut self, view_model: PanelViewModel) {
        let mut next_inputs = HashMap::new();
        for block in &view_model.blocks {
            if let PanelBlock::Input { id, value, .. } = block {
                if let Some(existing) = self.inputs.remove(id) {
                    // Reuse the live editor — this is what saves a half-typed
                    // OAuth code when the plugin re-publishes.
                    next_inputs.insert(id.clone(), existing);
                } else {
                    let mut editor = LineEditor::default();
                    if let Some(v) = value {
                        editor.set_text(v.clone());
                    }
                    next_inputs.insert(id.clone(), editor);
                }
            }
        }
        self.inputs = next_inputs;
        self.view_model = view_model;
        self.clamp_tables();
        let n = self.focusables().len();
        if n == 0 {
            self.focus = 0;
        } else if self.focus >= n {
            self.focus = n - 1;
        }
    }

    pub(crate) fn title(&self) -> &str {
        &self.view_model.title
    }

    /// Current text of every Input block, keyed by the field's `id`. Sent as the
    /// action `inputs` when a button is activated.
    pub(crate) fn collect_inputs(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for block in &self.view_model.blocks {
            if let PanelBlock::Input { id, value, .. } = block {
                let text = self
                    .inputs
                    .get(id)
                    .map(|e| e.text().to_string())
                    .or_else(|| value.clone())
                    .unwrap_or_default();
                out.insert(id.clone(), text);
            }
        }
        out
    }

    /// Focusable elements in top-to-bottom order.
    fn focusables(&self) -> Vec<Focusable> {
        let mut out = Vec::new();
        for (bi, block) in self.view_model.blocks.iter().enumerate() {
            match block {
                PanelBlock::Table { selectable, .. } if *selectable => {
                    out.push(Focusable::Table(bi));
                }
                PanelBlock::Input { id, .. } => out.push(Focusable::Input(bi, id.clone())),
                PanelBlock::Actions { buttons } => {
                    for i in 0..buttons.len() {
                        out.push(Focusable::Button(bi, i));
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn focus_next(&mut self, n: usize) {
        if n > 0 {
            self.focus = (self.focus + 1) % n;
        }
    }

    fn focus_prev(&mut self, n: usize) {
        if n > 0 {
            self.focus = (self.focus + n - 1) % n;
        }
    }

    fn rows_len(&self, block_idx: usize) -> usize {
        match self.view_model.blocks.get(block_idx) {
            Some(PanelBlock::Table { rows, .. }) => rows.len(),
            _ => 0,
        }
    }

    fn table_move(&mut self, block_idx: usize, delta: isize) {
        let rows = self.rows_len(block_idx);
        if rows == 0 {
            return;
        }
        let sel = self.tables.entry(block_idx).or_default();
        let cur = sel.selected as isize;
        let next = (cur + delta).clamp(0, rows as isize - 1);
        sel.selected = next as usize;
    }

    fn clamp_tables(&mut self) {
        let blocks = &self.view_model.blocks;
        self.tables.retain(|bi, sel| match blocks.get(*bi) {
            Some(PanelBlock::Table { rows, .. }) if !rows.is_empty() => {
                sel.selected = sel.selected.min(rows.len() - 1);
                sel.offset = sel.offset.min(rows.len() - 1);
                true
            }
            _ => false,
        });
    }

    fn button_id(&self, block_idx: usize, button_idx: usize) -> Option<String> {
        match self.view_model.blocks.get(block_idx) {
            Some(PanelBlock::Actions { buttons }) => buttons.get(button_idx).map(|b| b.id.clone()),
            _ => None,
        }
    }

    /// The id of a button whose `key` letter equals `c` (case-insensitive).
    fn button_by_key(&self, c: char) -> Option<String> {
        let lc = c.to_ascii_lowercase();
        for block in &self.view_model.blocks {
            if let PanelBlock::Actions { buttons } = block {
                for b in buttons {
                    if let Some(k) = &b.key
                        && k.chars().next().map(|kc| kc.to_ascii_lowercase()) == Some(lc)
                    {
                        return Some(b.id.clone());
                    }
                }
            }
        }
        None
    }

    /// Route a key to the panel. Printable keys reach a focused Input's editor
    /// BEFORE button `key`-letter shortcuts, so typing `a` in a field inserts
    /// `a` rather than firing a button.
    pub(crate) fn handle_key(&mut self, key: &KeyEvent) -> PanelKeyOutcome {
        if key.kind == KeyEventKind::Release {
            return PanelKeyOutcome::Ignored;
        }
        if key.code == KeyCode::Esc {
            return PanelKeyOutcome::Close;
        }
        let focusables = self.focusables();
        let n = focusables.len();
        let focused = focusables.get(self.focus).cloned();

        // A focused Input owns printable/editing keys first.
        if let Some(Focusable::Input(_, id)) = &focused {
            match key.code {
                KeyCode::Tab | KeyCode::Down => {
                    self.focus_next(n);
                    return PanelKeyOutcome::Changed;
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.focus_prev(n);
                    return PanelKeyOutcome::Changed;
                }
                _ => {
                    if let Some(editor) = self.inputs.get_mut(id) {
                        return match editor.handle_key(key) {
                            LineEditOutcome::Unhandled => PanelKeyOutcome::Ignored,
                            _ => PanelKeyOutcome::Changed,
                        };
                    }
                    return PanelKeyOutcome::Ignored;
                }
            }
        }

        // Non-input focus (table / button / none).
        match key.code {
            KeyCode::Tab => {
                self.focus_next(n);
                PanelKeyOutcome::Changed
            }
            KeyCode::BackTab => {
                self.focus_prev(n);
                PanelKeyOutcome::Changed
            }
            KeyCode::Enter => {
                if let Some(Focusable::Button(bi, idx)) = &focused
                    && let Some(id) = self.button_id(*bi, *idx)
                {
                    return PanelKeyOutcome::Activate { button_id: id };
                }
                PanelKeyOutcome::Ignored
            }
            KeyCode::Left => PanelKeyOutcome::PrevPanel,
            KeyCode::Right => PanelKeyOutcome::NextPanel,
            KeyCode::Up => {
                if let Some(Focusable::Table(bi)) = &focused {
                    self.table_move(*bi, -1);
                } else {
                    self.focus_prev(n);
                }
                PanelKeyOutcome::Changed
            }
            KeyCode::Down => {
                if let Some(Focusable::Table(bi)) = &focused {
                    self.table_move(*bi, 1);
                } else {
                    self.focus_next(n);
                }
                PanelKeyOutcome::Changed
            }
            KeyCode::Char(c) => {
                // Button key-letter shortcut wins over navigation letters.
                if let Some(id) = self.button_by_key(c) {
                    return PanelKeyOutcome::Activate { button_id: id };
                }
                match c {
                    'q' => PanelKeyOutcome::Close,
                    ' ' => {
                        if let Some(Focusable::Button(bi, idx)) = &focused
                            && let Some(id) = self.button_id(*bi, *idx)
                        {
                            return PanelKeyOutcome::Activate { button_id: id };
                        }
                        PanelKeyOutcome::Ignored
                    }
                    'h' => PanelKeyOutcome::PrevPanel,
                    'l' => PanelKeyOutcome::NextPanel,
                    'k' => {
                        if let Some(Focusable::Table(bi)) = &focused {
                            self.table_move(*bi, -1);
                        } else {
                            self.focus_prev(n);
                        }
                        PanelKeyOutcome::Changed
                    }
                    'j' => {
                        if let Some(Focusable::Table(bi)) = &focused {
                            self.table_move(*bi, 1);
                        } else {
                            self.focus_next(n);
                        }
                        PanelKeyOutcome::Changed
                    }
                    _ => PanelKeyOutcome::Ignored,
                }
            }
            _ => PanelKeyOutcome::Ignored,
        }
    }

    /// Route a bracketed paste to the focused Input, if any.
    pub(crate) fn handle_paste(&mut self, text: &str) -> PanelKeyOutcome {
        let focusables = self.focusables();
        if let Some(Focusable::Input(_, id)) = focusables.get(self.focus)
            && let Some(editor) = self.inputs.get_mut(id)
        {
            editor.insert_paste(text);
            return PanelKeyOutcome::Changed;
        }
        PanelKeyOutcome::Ignored
    }

    /// Render the panel's blocks top-to-bottom into `area`. Takes `&mut self`
    /// so a selectable table can scroll its window to keep the selection
    /// visible for the current viewport height.
    pub(crate) fn render_content(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        let bottom = area.y.saturating_add(area.height);
        let focusables = self.focusables();
        let focused = focusables.get(self.focus).cloned();
        let blocks = self.view_model.blocks.clone();
        let mut y = area.y;

        for (bi, block) in blocks.iter().enumerate() {
            if y >= bottom {
                break;
            }
            match block {
                PanelBlock::Status { items } => {
                    let mut spans = Vec::new();
                    for item in items {
                        let bg = tone_color(item.tone, theme);
                        spans.push(Span::styled(
                            format!(" {}: {} ", item.label, item.value),
                            Style::default().fg(theme.bg_base).bg(bg),
                        ));
                        spans.push(Span::raw(" "));
                    }
                    buf.set_line(area.x, y, &Line::from(spans), area.width);
                    y += 1;
                }
                PanelBlock::Markdown { text } => {
                    let md = MarkdownContent::new(text.clone());
                    let lines = md.with_wrapped_lines(width, |w| w.lines.to_vec());
                    for line in lines {
                        if y >= bottom {
                            break;
                        }
                        buf.set_line(area.x, y, &line, area.width);
                        y += 1;
                    }
                }
                PanelBlock::Table {
                    columns,
                    rows,
                    selectable,
                } => {
                    let header_style = Style::default()
                        .fg(theme.text_secondary)
                        .add_modifier(Modifier::BOLD);
                    let header = Line::from(format!("  {}", columns.join("   ")));
                    buf.set_line(area.x, y, &header.style(header_style), area.width);
                    y += 1;

                    let is_focused_table =
                        matches!(&focused, Some(Focusable::Table(fb)) if *fb == bi);
                    let avail = bottom.saturating_sub(y) as usize;
                    let max_rows = avail.min(rows.len());
                    let sel = self.tables.entry(bi).or_default();
                    if *selectable && max_rows > 0 {
                        if sel.selected < sel.offset {
                            sel.offset = sel.selected;
                        } else if sel.selected >= sel.offset + max_rows {
                            sel.offset = sel.selected + 1 - max_rows;
                        }
                    }
                    let offset = sel.offset.min(rows.len().saturating_sub(1));
                    let selected = sel.selected;
                    for (ri, row) in rows.iter().enumerate().skip(offset).take(max_rows) {
                        if y >= bottom {
                            break;
                        }
                        let is_sel = *selectable && ri == selected;
                        let marker = if is_sel { "\u{25b8} " } else { "  " };
                        let mut style = Style::default().fg(theme.text_primary);
                        if is_sel && is_focused_table {
                            style = style.add_modifier(Modifier::REVERSED);
                        } else if is_sel {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        let line = Line::from(format!("{marker}{}", row.join("   "))).style(style);
                        buf.set_line(area.x, y, &line, area.width);
                        y += 1;
                    }
                }
                PanelBlock::Input {
                    id,
                    label,
                    placeholder,
                    secret,
                    ..
                } => {
                    let is_focused = matches!(&focused, Some(Focusable::Input(_, fid)) if fid == id);
                    let text = self
                        .inputs
                        .get(id)
                        .map(|e| e.text().to_string())
                        .unwrap_or_default();
                    let (shown, dim) = if text.is_empty() {
                        (placeholder.clone().unwrap_or_default(), true)
                    } else if *secret {
                        ("\u{2022}".repeat(text.chars().count()), false)
                    } else {
                        (text, false)
                    };
                    let mut value_style = Style::default().bg(theme.bg_base);
                    value_style = if dim {
                        value_style.fg(theme.gray_dim)
                    } else {
                        value_style.fg(theme.text_primary)
                    };
                    if is_focused {
                        value_style = value_style.add_modifier(Modifier::REVERSED);
                    }
                    let marker = if is_focused { "\u{203a} " } else { "  " };
                    let line = Line::from(vec![
                        Span::styled(
                            format!("{marker}{label}: "),
                            Style::default().fg(theme.text_secondary),
                        ),
                        Span::styled(format!("{shown} "), value_style),
                    ]);
                    buf.set_line(area.x, y, &line, area.width);
                    y += 1;
                }
                PanelBlock::Actions { buttons } => {
                    let mut spans = Vec::new();
                    for (idx, btn) in buttons.iter().enumerate() {
                        let is_focused =
                            matches!(&focused, Some(Focusable::Button(fb, fi)) if *fb == bi && *fi == idx);
                        let label = match &btn.key {
                            Some(k) => format!(" {} ({}) ", btn.label, k),
                            None => format!(" {} ", btn.label),
                        };
                        let style = if is_focused {
                            Style::default()
                                .fg(theme.bg_base)
                                .bg(theme.accent_user)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme.text_primary).bg(theme.bg_dark)
                        };
                        spans.push(Span::styled(label, style));
                        spans.push(Span::raw(" "));
                    }
                    buf.set_line(area.x, y, &Line::from(spans), area.width);
                    y += 1;
                }
            }
            // Blank separator between blocks.
            if y < bottom {
                y += 1;
            }
        }
    }
}

fn tone_color(tone: PanelTone, theme: &Theme) -> Color {
    match tone {
        PanelTone::Neutral => theme.text_secondary,
        PanelTone::Success => theme.accent_success,
        PanelTone::Warning => theme.warning,
        PanelTone::Error => theme.accent_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn input_block(id: &str, value: Option<&str>) -> PanelBlock {
        PanelBlock::Input {
            id: id.into(),
            label: id.into(),
            placeholder: None,
            value: value.map(Into::into),
            secret: false,
        }
    }

    fn ch(state: &mut PanelState, c: char) -> PanelKeyOutcome {
        state.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    /// The headline property: a re-publish keeps the live editor and DISCARDS
    /// the block's `value`, while seeding a fresh editor for a new input id.
    #[test]
    fn merge_reuses_editor_and_discards_new_value() {
        let vm1 = PanelViewModel {
            id: "p".into(),
            title: "t".into(),
            blocks: vec![input_block("code", None)],
        };
        let mut state = PanelState::from_view_model(vm1);
        for c in "abc".chars() {
            assert_eq!(ch(&mut state, c), PanelKeyOutcome::Changed);
        }
        assert_eq!(state.collect_inputs().get("code").unwrap(), "abc");

        // Re-publish with a server-set value for the SAME id plus a new id.
        let vm2 = PanelViewModel {
            id: "p".into(),
            title: "t2".into(),
            blocks: vec![
                input_block("code", Some("server-value")),
                input_block("extra", Some("x")),
            ],
        };
        state.merge(vm2);
        assert_eq!(
            state.collect_inputs().get("code").unwrap(),
            "abc",
            "existing editor must survive and ignore the re-published value"
        );
        assert_eq!(
            state.collect_inputs().get("extra").unwrap(),
            "x",
            "a genuinely new input id is seeded from its value"
        );
        assert_eq!(state.title(), "t2");
    }

    #[test]
    fn dropped_input_id_removes_its_editor() {
        let vm1 = PanelViewModel {
            id: "p".into(),
            title: "t".into(),
            blocks: vec![input_block("a", None), input_block("b", None)],
        };
        let mut state = PanelState::from_view_model(vm1);
        let vm2 = PanelViewModel {
            id: "p".into(),
            title: "t".into(),
            blocks: vec![input_block("a", None)],
        };
        state.merge(vm2);
        let inputs = state.collect_inputs();
        assert!(inputs.contains_key("a"));
        assert!(!inputs.contains_key("b"), "dropped input must not linger");
    }
}
