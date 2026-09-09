//! Persona detail/edit modal: structured view of a persona with inline editing.
//!
//! Opened by pressing Enter on a persona in the `/config-agents` Personas tab.
//! Renders all persona TOML fields in labeled sections.
//! Editable personas (user/project scope) support inline field editing; bundled personas are read-only.
//!
//! Neither the read nor the write touches a file. The fields arrive as a
//! [`PersonaDocument`] from `x.ai/personas/get` and a committed edit leaves as
//! [`PersonaDetailOutcome::Save`], which the app turns into `x.ai/personas/save`.
//! The `revision` carried through is what makes that safe: it is the hash of
//! the bytes this view was built from, and the shell refuses a save that quotes
//! a revision the file no longer has, so an edit made against what the user saw
//! cannot silently overwrite an edit made since — by another client or by the
//! `$EDITOR` this same view offers on `i`.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::input::line_editor::{LineEditOutcome, LineEditor};
use crate::theme::Theme;
use crate::views::modal_window::{
    self, ModalContentArea, ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut,
};
use xai_grok_shell::extensions::personas::{
    PersonaDocument, PersonaRefusalKind, PersonaScope, PersonaWriteResponse,
};

// ---------------------------------------------------------------------------
// Field enum
// ---------------------------------------------------------------------------

/// Navigable fields in the persona detail view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonaField {
    Name,
    Description,
    Model,
    ReasoningEffort,
    Isolation,
    Instructions,
    InstructionsFile,
}

impl PersonaField {
    const ALL: &[PersonaField] = &[
        PersonaField::Name,
        PersonaField::Description,
        PersonaField::Model,
        PersonaField::ReasoningEffort,
        PersonaField::Isolation,
        PersonaField::Instructions,
        PersonaField::InstructionsFile,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Description => "Description",
            Self::Model => "Model",
            Self::ReasoningEffort => "Effort",
            Self::Isolation => "Isolation",
            Self::Instructions => "Instructions",
            Self::InstructionsFile => "Instr. file",
        }
    }

    fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|&f| f == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    fn prev(self) -> Self {
        let idx = Self::ALL.iter().position(|&f| f == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    /// True for fields that support inline text editing.
    fn is_editable(self) -> bool {
        matches!(
            self,
            Self::Name | Self::Description | Self::Model | Self::ReasoningEffort | Self::Isolation
        )
    }
}

// ---------------------------------------------------------------------------
// Mode state machine
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum PersonaDetailMode {
    Browse,
    Editing {
        field: PersonaField,
        editor: LineEditor,
        original: String,
    },
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// The seven inline-editable fields, as one save carries them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonaFieldEdits {
    pub name: String,
    pub description: String,
    pub model: String,
    pub reasoning_effort: String,
    pub default_isolation: String,
    pub instructions: String,
    pub instructions_file: String,
}

#[derive(Debug)]
pub enum PersonaDetailOutcome {
    /// The event was handled and the modal changed.
    Changed,
    /// Nothing to do.
    Unchanged,
    /// Close the detail modal, return to the list.
    Close,
    /// Open the file in $EDITOR.
    EditInEditor { path: PathBuf },
    /// A field edit was committed. The app sends it as `x.ai/personas/save`;
    /// this view does not write, so that one write is one method for every
    /// client instead of one per client.
    Save {
        name: String,
        scope: PersonaScope,
        /// What this view was built from. A mismatch on disk refuses the save.
        base_revision: String,
        fields: PersonaFieldEdits,
    },
}

// ---------------------------------------------------------------------------
// I/O entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PersonaIOEntry {
    pub name: String,
    pub io_type: String,
    pub required: bool,
    pub description: String,
}

impl PersonaIOEntry {
    fn from_wire(field: &xai_grok_shell::extensions::personas::PersonaIo) -> Self {
        Self {
            name: field.name.clone(),
            io_type: field.io_type.clone(),
            required: field.required,
            description: field.description.clone(),
        }
    }
}

/// The word shown next to a persona name. The scope is the wire enum; this is
/// the one place it becomes prose.
pub fn scope_label(scope: PersonaScope) -> &'static str {
    match scope {
        PersonaScope::Bundled => "bundled",
        PersonaScope::User => "user",
        PersonaScope::Project => "project",
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct PersonaDetailState {
    pub window: ModalWindowState,
    /// The catalog name, i.e. the file stem. What a save and a delete address,
    /// as against `name`, which is the editable `name` key inside the file.
    pub catalog_name: String,
    pub name: String,
    pub description: String,
    pub model: String,
    pub reasoning_effort: String,
    pub default_isolation: String,
    pub instructions: String,
    pub instructions_file: String,
    pub inputs: Vec<PersonaIOEntry>,
    pub outputs: Vec<PersonaIOEntry>,
    pub source_path: Option<PathBuf>,
    pub editable: bool,
    pub scope: PersonaScope,
    /// Hash of the bytes this view was built from. Quoted back on save so a
    /// file that changed underneath refuses the write instead of losing it.
    pub revision: String,
    pub selected_field: PersonaField,
    pub scroll_offset: usize,
    mode: PersonaDetailMode,
    pub dirty: bool,
    pub instructions_expanded: bool,
    /// Scroll offset within expanded instructions (line index of first visible line).
    pub instructions_scroll: usize,
    pub message: Option<String>,
}

impl PersonaDetailState {
    /// Build the view from one `x.ai/personas/get` answer.
    ///
    /// `name` shows the persona's own `name` key when it declares one and the
    /// catalog name otherwise, which is what a reader expects to see; the
    /// catalog name is kept separately because that, not the displayed one, is
    /// what a save addresses.
    pub fn from_document(doc: &PersonaDocument) -> Self {
        Self {
            window: ModalWindowState::new(),
            catalog_name: doc.name.clone(),
            name: if doc.declared_name.is_empty() {
                doc.name.clone()
            } else {
                doc.declared_name.clone()
            },
            description: doc.description.clone(),
            model: doc.model.clone(),
            reasoning_effort: doc.reasoning_effort.clone(),
            default_isolation: doc.default_isolation.clone(),
            instructions: doc.instructions.clone(),
            instructions_file: doc.instructions_file.clone(),
            inputs: doc.inputs.iter().map(PersonaIOEntry::from_wire).collect(),
            outputs: doc.outputs.iter().map(PersonaIOEntry::from_wire).collect(),
            source_path: (!doc.source_path.is_empty()).then(|| PathBuf::from(&doc.source_path)),
            editable: doc.editable,
            scope: doc.scope,
            revision: doc.revision.clone(),
            selected_field: PersonaField::Name,
            scroll_offset: 0,
            mode: PersonaDetailMode::Browse,
            dirty: false,
            instructions_expanded: false,
            instructions_scroll: 0,
            message: None,
        }
    }

    fn field_value(&self, field: PersonaField) -> &str {
        match field {
            PersonaField::Name => &self.name,
            PersonaField::Description => &self.description,
            PersonaField::Model => &self.model,
            PersonaField::ReasoningEffort => &self.reasoning_effort,
            PersonaField::Isolation => &self.default_isolation,
            PersonaField::Instructions => &self.instructions,
            PersonaField::InstructionsFile => &self.instructions_file,
        }
    }

    fn set_field_value(&mut self, field: PersonaField, value: String) {
        match field {
            PersonaField::Name => self.name = value,
            PersonaField::Description => self.description = value,
            PersonaField::Model => self.model = value,
            PersonaField::ReasoningEffort => self.reasoning_effort = value,
            PersonaField::Isolation => self.default_isolation = value,
            PersonaField::Instructions => self.instructions = value,
            PersonaField::InstructionsFile => self.instructions_file = value,
        }
    }

    pub fn is_editing(&self) -> bool {
        matches!(&self.mode, PersonaDetailMode::Editing { .. })
    }

    #[cfg(test)]
    fn editing_editor(&self) -> Option<&LineEditor> {
        match &self.mode {
            PersonaDetailMode::Editing { editor, .. } => Some(editor),
            PersonaDetailMode::Browse => None,
        }
    }

    #[cfg(test)]
    fn editing_viewport(&self, width: usize) -> Option<xai_ratatui_textarea::SingleLineViewport> {
        self.editing_editor().map(|editor| editor.viewport(width))
    }

    #[cfg(test)]
    fn editing_text(&self) -> Option<&str> {
        self.editing_editor().map(LineEditor::text)
    }

    #[cfg(test)]
    fn set_editing_text(&mut self, text: impl Into<String>) {
        if let PersonaDetailMode::Editing { editor, .. } = &mut self.mode {
            editor.set_text(text);
        }
    }

    #[cfg(test)]
    fn set_editing_cursor_byte(&mut self, cursor_byte: usize) -> LineEditOutcome {
        match &mut self.mode {
            PersonaDetailMode::Editing { editor, .. } => editor.set_cursor_byte(cursor_byte),
            PersonaDetailMode::Browse => LineEditOutcome::Unhandled,
        }
    }

    /// Fold one `x.ai/personas/save` answer back into the view.
    ///
    /// A refused save keeps the stale revision on purpose. Adopting the one the
    /// refusal carries would make the very next keystroke overwrite the write
    /// that beat us, which is the lost edit this whole path exists to prevent;
    /// leaving it stale means every save from this view keeps refusing until
    /// the user reopens the persona and sees what is actually in the file.
    pub fn apply_write_result(&mut self, response: &PersonaWriteResponse) {
        match &response.refusal {
            None => {
                if let Some(ref revision) = response.revision {
                    self.revision = revision.clone();
                }
                self.dirty = false;
                self.message = Some("Saved".to_string());
            }
            Some(refusal) if refusal.kind == PersonaRefusalKind::Conflict => {
                self.message = Some(format!(
                    "{} — reopen it to see the current file",
                    refusal.message
                ));
            }
            Some(refusal) => {
                self.message = Some(refusal.message.clone());
            }
        }
    }

    /// The seven editable fields as the save method wants them.
    ///
    /// All seven go every time, empty included, because empty is how a key is
    /// removed. The tables this view does not show — `inputs`, `outputs` — are
    /// not in the request at all, so the shell's document edit leaves them, and
    /// any comments, exactly where the user put them.
    fn field_edits(&self) -> PersonaFieldEdits {
        PersonaFieldEdits {
            name: self.name.clone(),
            description: self.description.clone(),
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            default_isolation: self.default_isolation.clone(),
            instructions: self.instructions.clone(),
            instructions_file: self.instructions_file.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_detail_editor(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: usize,
    editor: &LineEditor,
    style: Style,
    theme: &Theme,
) {
    let viewport = editor.viewport(width);
    let visible = &editor.text()[viewport.visible_byte_range];
    buf.set_string(x, y, visible, style);
    if width > 0 {
        let cursor_x = x + viewport.cursor_display_column as u16;
        if let Some(cell) = buf.cell_mut((cursor_x, y)) {
            cell.set_style(Style::default().fg(theme.bg_base).bg(theme.text_primary));
        }
    }
}

/// Render the persona detail modal.
pub fn render_persona_detail(
    buf: &mut Buffer,
    area: Rect,
    state: &mut PersonaDetailState,
    theme: &Theme,
    compact: bool,
) {
    let title = format!("persona: {}", state.name);
    let shortcuts = build_shortcuts(state);
    let config = ModalWindowConfig {
        title: &title,
        tabs: None,
        shortcuts: &shortcuts,
        sizing: persona_detail_sizing(compact),
        fold_info: None,
    };
    let Some(ModalContentArea {
        content: content_area,
        ..
    }) = modal_window::render_modal_window(buf, area, &mut state.window, &config, theme)
    else {
        return;
    };

    let w = content_area.width as usize;
    let mut y = content_area.y;
    let max_y = content_area.y + content_area.height;
    let label_w = 14u16; // column width for field labels

    // Message line
    if let Some(ref msg) = state.message
        && y < max_y
    {
        buf.set_string(
            content_area.x,
            y,
            msg,
            Style::default().fg(theme.accent_error),
        );
        y += 2;
    }

    // Render each field row.
    for &field in PersonaField::ALL {
        if y >= max_y {
            break;
        }

        let is_selected = state.selected_field == field;
        let label = field.label();
        let value = state.field_value(field);

        // Background highlight for selected row.
        let row_bg = if is_selected {
            Some(theme.bg_highlight)
        } else {
            None
        };

        // Label
        let label_style = if is_selected {
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray)
        };
        if let Some(bg) = row_bg {
            // Fill the row background.
            let blank: String = " ".repeat(w);
            buf.set_string(content_area.x, y, &blank, Style::default().bg(bg));
        }
        buf.set_string(content_area.x, y, label, label_style);

        let value_x = content_area.x + label_w;
        let value_w = w.saturating_sub(label_w as usize);

        if is_selected
            && let PersonaDetailMode::Editing {
                field: editing_field,
                editor,
                ..
            } = &state.mode
            && *editing_field == field
        {
            let field_style = if let Some(bg) = row_bg {
                Style::default().fg(theme.text_primary).bg(bg)
            } else {
                Style::default().fg(theme.text_primary)
            };
            render_detail_editor(buf, value_x, y, value_w, editor, field_style, theme);
        } else if field == PersonaField::Instructions {
            // Multi-line instructions with expand/collapse and scroll.
            if value.is_empty() {
                let empty_style = if let Some(bg) = row_bg {
                    Style::default().fg(theme.gray_dim).bg(bg)
                } else {
                    Style::default().fg(theme.gray_dim)
                };
                buf.set_string(value_x, y, "(empty)", empty_style);
            } else {
                let lines = word_wrap_lines(value, value_w);
                let total = lines.len();
                let max_collapsed = 8usize;
                let is_long = total > max_collapsed;

                // Reserve 1 line for the hint at the bottom.
                let avail_lines = (max_y.saturating_sub(y)) as usize;
                let viewport_h = if is_long {
                    avail_lines.saturating_sub(1) // room for hint
                } else {
                    avail_lines
                };

                let val_style = if let Some(bg) = row_bg {
                    Style::default().fg(theme.text_secondary).bg(bg)
                } else {
                    Style::default().fg(theme.text_secondary)
                };

                if !state.instructions_expanded {
                    // Collapsed: show first max_collapsed lines (no scroll).
                    let show = total.min(max_collapsed).min(viewport_h);
                    for (i, line) in lines.iter().enumerate().take(show) {
                        let x_pos = if i == 0 { value_x } else { content_area.x + 2 };
                        buf.set_string(x_pos, y + i as u16, line, val_style);
                    }
                    y += show.saturating_sub(1) as u16;
                    if is_long {
                        y += 1;
                        if y < max_y {
                            let hint = format!(
                                "  ... ({} more lines: e to expand, j/k to scroll)",
                                total - max_collapsed
                            );
                            buf.set_string(
                                content_area.x + 2,
                                y,
                                hint,
                                Style::default().fg(theme.gray_dim),
                            );
                        }
                    }
                } else {
                    // Expanded: viewport with scroll offset.
                    let scroll = state
                        .instructions_scroll
                        .min(total.saturating_sub(viewport_h));
                    state.instructions_scroll = scroll;
                    let visible = &lines[scroll..total.min(scroll + viewport_h)];
                    for (i, line) in visible.iter().enumerate() {
                        let x_pos = if i == 0 && scroll == 0 {
                            value_x
                        } else {
                            content_area.x + 2
                        };
                        buf.set_string(x_pos, y + i as u16, line, val_style);
                    }
                    y += visible.len().saturating_sub(1) as u16;
                    // Hint line.
                    y += 1;
                    if y < max_y {
                        let pos_hint = if total > viewport_h {
                            format!(
                                " [{}\u{2013}{}/ {}]",
                                scroll + 1,
                                (scroll + viewport_h).min(total),
                                total
                            )
                        } else {
                            String::new()
                        };
                        let hint = format!("  (e to collapse, j/k to scroll{})", pos_hint);
                        buf.set_string(
                            content_area.x + 2,
                            y,
                            hint,
                            Style::default().fg(theme.gray_dim),
                        );
                    }
                }
            }
        } else if value.is_empty() {
            let empty_style = if let Some(bg) = row_bg {
                Style::default().fg(theme.gray_dim).bg(bg)
            } else {
                Style::default().fg(theme.gray_dim)
            };
            buf.set_string(value_x, y, "-", empty_style);
        } else if value.width() <= value_w {
            // Fits on one line.
            let val_style = if let Some(bg) = row_bg {
                Style::default().fg(theme.text_primary).bg(bg)
            } else {
                Style::default().fg(theme.text_primary)
            };
            buf.set_string(value_x, y, value, val_style);
        } else {
            // Word-wrap long values.
            let val_style = if let Some(bg) = row_bg {
                Style::default().fg(theme.text_primary).bg(bg)
            } else {
                Style::default().fg(theme.text_primary)
            };
            let lines = word_wrap_lines(value, value_w);
            for (i, line) in lines.iter().enumerate() {
                if y + i as u16 >= max_y {
                    break;
                }
                let x_pos = if i == 0 {
                    value_x
                } else {
                    content_area.x + label_w
                };
                buf.set_string(x_pos, y + i as u16, line, val_style);
            }
            y += lines.len().saturating_sub(1) as u16;
        }

        y += 2; // spacing between fields
    }

    // I/O sections
    for (section, items) in [("Inputs", &state.inputs), ("Outputs", &state.outputs)] {
        if items.is_empty() || y >= max_y {
            continue;
        }
        buf.set_string(
            content_area.x,
            y,
            section,
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        );
        y += 1;
        for entry in items {
            if y >= max_y {
                break;
            }
            let req = if entry.required { ", required" } else { "" };
            let header = format!("  \u{2022} {} ({}{})", entry.name, entry.io_type, req);
            buf.set_string(
                content_area.x,
                y,
                &header,
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            );
            if !entry.description.is_empty() {
                // Wrap the description across multiple lines below the header.
                let indent = 4usize;
                let desc_w = w.saturating_sub(indent);
                if desc_w > 0 {
                    y += 1;
                    for desc_line in word_wrap_lines(&entry.description, desc_w) {
                        if y >= max_y {
                            break;
                        }
                        let padded = format!("{:indent$}{desc_line}", "", indent = indent);
                        buf.set_string(
                            content_area.x,
                            y,
                            &padded,
                            Style::default().fg(theme.text_secondary),
                        );
                        y += 1;
                    }
                } else {
                    y += 1;
                }
            } else {
                y += 1;
            }
        }
        y += 1;
    }

    // Source path
    if y < max_y
        && let Some(ref path) = state.source_path
    {
        let src = format!("Source: {}", path.display());
        let truncated: String = src.chars().take(w).collect();
        buf.set_string(
            content_area.x,
            y,
            &truncated,
            Style::default().fg(theme.gray_dim),
        );
    }
}

fn persona_detail_sizing(compact: bool) -> ModalSizing {
    ModalSizing {
        width_pct: 0.70,
        max_width: 100,
        min_width: 44,
        v_margin: 4,
        h_pad: 2,
        v_pad: 1,
        footer_lines: 2,
    }
    .with_compact(compact)
}

fn build_shortcuts(state: &PersonaDetailState) -> Vec<Shortcut<'static>> {
    if state.is_editing() {
        vec![
            Shortcut {
                label: "Enter save",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "Esc cancel",
                clickable: false,
                id: 0,
            },
        ]
    } else {
        let mut shortcuts = vec![Shortcut {
            label: "j/k nav",
            clickable: false,
            id: 0,
        }];
        if state.editable {
            shortcuts.push(Shortcut {
                label: "e edit field",
                clickable: false,
                id: 0,
            });
        }
        if state.source_path.is_some() && state.editable {
            shortcuts.push(Shortcut {
                label: "i $EDITOR",
                clickable: false,
                id: 0,
            });
        }
        shortcuts.push(Shortcut {
            label: "Esc back",
            clickable: false,
            id: 0,
        });
        shortcuts
    }
}

// ---------------------------------------------------------------------------
// Input handling
// ---------------------------------------------------------------------------

pub fn handle_persona_detail_key(
    state: &mut PersonaDetailState,
    key: &KeyEvent,
) -> PersonaDetailOutcome {
    state.message = None;

    if state.is_editing() {
        handle_editing_key(state, key)
    } else {
        handle_browse_key(state, key)
    }
}

pub fn handle_persona_detail_paste(
    state: &mut PersonaDetailState,
    text: &str,
) -> PersonaDetailOutcome {
    if !state.is_editing() {
        return PersonaDetailOutcome::Unchanged;
    }
    state.message = None;
    let outcome = match &mut state.mode {
        PersonaDetailMode::Editing { editor, .. } => editor.insert_paste(text),
        PersonaDetailMode::Browse => unreachable!("editing mode changed before paste"),
    };
    finish_edit(outcome)
}

fn handle_browse_key(state: &mut PersonaDetailState, key: &KeyEvent) -> PersonaDetailOutcome {
    // When instructions are expanded and selected, j/k scrolls within them.
    let instr_scrolling =
        state.selected_field == PersonaField::Instructions && state.instructions_expanded;

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            // If instructions are expanded, collapse first instead of closing.
            if instr_scrolling {
                state.instructions_expanded = false;
                state.instructions_scroll = 0;
                return PersonaDetailOutcome::Changed;
            }
            PersonaDetailOutcome::Close
        }
        KeyCode::Char('j') | KeyCode::Down if instr_scrolling => {
            state.instructions_scroll = state.instructions_scroll.saturating_add(1);
            PersonaDetailOutcome::Changed
        }
        KeyCode::Char('k') | KeyCode::Up if instr_scrolling => {
            state.instructions_scroll = state.instructions_scroll.saturating_sub(1);
            PersonaDetailOutcome::Changed
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.selected_field = state.selected_field.next();
            PersonaDetailOutcome::Changed
        }
        // Instructions: e/Enter toggles expand/collapse.
        KeyCode::Char('e') | KeyCode::Enter
            if state.selected_field == PersonaField::Instructions =>
        {
            state.instructions_expanded = !state.instructions_expanded;
            state.instructions_scroll = 0;
            PersonaDetailOutcome::Changed
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.selected_field = state.selected_field.prev();
            PersonaDetailOutcome::Changed
        }
        // Other fields: e/Enter opens inline editor.
        KeyCode::Char('e') | KeyCode::Enter => {
            if !state.editable {
                state.message = Some("Bundled personas are read-only".to_string());
                return PersonaDetailOutcome::Changed;
            }
            let field = state.selected_field;
            if !field.is_editable() {
                state.message = Some("This field cannot be edited inline".to_string());
                return PersonaDetailOutcome::Changed;
            }
            let current = state.field_value(field).to_owned();
            if current.contains(['\n', '\r']) {
                state.message =
                    Some("Multiline values must be edited in the source file".to_string());
                return PersonaDetailOutcome::Changed;
            }
            let mut editor = LineEditor::default();
            editor.set_text(&current);
            let original = current;
            state.mode = PersonaDetailMode::Editing {
                field,
                editor,
                original,
            };
            PersonaDetailOutcome::Changed
        }
        KeyCode::Char('i') => {
            if let Some(ref path) = state.source_path {
                if state.editable {
                    return PersonaDetailOutcome::EditInEditor { path: path.clone() };
                }
                state.message = Some("Bundled personas are read-only".to_string());
            } else {
                state.message = Some("No source file".to_string());
            }
            PersonaDetailOutcome::Changed
        }
        _ => PersonaDetailOutcome::Unchanged,
    }
}

fn handle_editing_key(state: &mut PersonaDetailState, key: &KeyEvent) -> PersonaDetailOutcome {
    if key.code == KeyCode::Esc {
        state.mode = PersonaDetailMode::Browse;
        return PersonaDetailOutcome::Changed;
    }
    if key.code == KeyCode::Enter {
        let mode = std::mem::replace(&mut state.mode, PersonaDetailMode::Browse);
        let PersonaDetailMode::Editing {
            field,
            editor,
            original,
        } = mode
        else {
            return PersonaDetailOutcome::Unchanged;
        };
        let new_value = editor.text().to_owned();
        let changed = new_value != original;
        if changed {
            state.set_field_value(field, new_value);
            state.dirty = true;
            state.message = Some("Saving…".to_string());
            return PersonaDetailOutcome::Save {
                name: state.catalog_name.clone(),
                scope: state.scope,
                base_revision: state.revision.clone(),
                fields: state.field_edits(),
            };
        }
        return PersonaDetailOutcome::Changed;
    }

    let outcome = match &mut state.mode {
        PersonaDetailMode::Editing { editor, .. } => editor.handle_key(key),
        PersonaDetailMode::Browse => return PersonaDetailOutcome::Unchanged,
    };
    finish_edit(outcome)
}

fn finish_edit(outcome: LineEditOutcome) -> PersonaDetailOutcome {
    match outcome {
        LineEditOutcome::TextChanged
        | LineEditOutcome::CursorChanged
        | LineEditOutcome::HandledNoChange => PersonaDetailOutcome::Changed,
        LineEditOutcome::Unhandled => PersonaDetailOutcome::Unchanged,
    }
}

pub fn handle_persona_detail_mouse(
    state: &mut PersonaDetailState,
    mouse: &MouseEvent,
) -> PersonaDetailOutcome {
    let chrome =
        modal_window::handle_modal_mouse(&mut state.window, mouse.kind, mouse.column, mouse.row);
    match chrome {
        modal_window::ModalWindowOutcome::CloseRequested => PersonaDetailOutcome::Close,
        modal_window::ModalWindowOutcome::Handled => PersonaDetailOutcome::Changed,
        _ => PersonaDetailOutcome::Unchanged,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn word_wrap_lines(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        if raw_line.width() <= max_width {
            lines.push(raw_line.to_string());
        } else {
            let mut current = String::new();
            for word in raw_line.split_whitespace() {
                if current.is_empty() {
                    current = word.to_string();
                } else if current.width() + 1 + word.width() <= max_width {
                    current.push(' ');
                    current.push_str(word);
                } else {
                    lines.push(current);
                    current = word.to_string();
                }
            }
            if !current.is_empty() {
                lines.push(current);
            }
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests;
