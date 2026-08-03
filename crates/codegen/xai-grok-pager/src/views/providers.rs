//! `/providers` — the resolved model catalog, grouped by provider.
//!
//! This panel deliberately shows what grok **resolved**, not what config.toml
//! declares. Every row is assembled from the live ACP catalog (the row set and
//! everything its `_meta` carries) enriched with
//! [`crate::acp::resolved_catalog::ResolvedCatalog`] (the post-override wire
//! slug, endpoint, wire format and output ceiling).
//!
//! Two disagreements between declared and resolved are called out as badges,
//! visible without expanding a row, because both have shipped as silent bugs:
//! a wire slug that is not the catalog key's own slug (the key reached the
//! vendor as `model` and 404'd), and a `[model."…"]` table that overrode a
//! `[[provider]]` default.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use agent_client_protocol as acp;

use crate::acp::model_state::ModelState;
use crate::acp::resolved_catalog::ResolvedCatalog;
use crate::render::SafeBuf;
use crate::theme::Theme;
use crate::views::modal_window::ModalWindowState;
use crate::views::picker::{PickerField, PickerRow, render_picker_row};

/// Group label for entries that carry no `[[provider]]` prefix.
pub const BUILTIN_GROUP: &str = "built-in";

/// Where a catalog entry's declaration came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowOrigin {
    /// Synthesized by a `[[provider]]` expansion, no per-model table.
    ProviderTable,
    /// A `[model."<key>"]` table only.
    ModelTable,
    /// A `[[provider]]` expansion refined by a `[model."<key>"]` table.
    Both,
    /// Not declared locally — it came from the server catalog / built-in
    /// defaults, so no post-override facts are available for it.
    ServerCatalog,
}

/// One model in the panel, already reconciled between ACP `_meta` and the
/// locally resolved catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelRow {
    /// Catalog key — what `/model` and the config address the entry by.
    pub key: String,
    /// Picker display name from the ACP catalog.
    pub display_name: String,
    /// Slug actually sent as `model`. `None` when the entry is not declared
    /// locally, in which case the pager cannot see it.
    pub wire_slug: Option<String>,
    pub endpoint: Option<String>,
    pub api_backend: Option<String>,
    pub context_tokens: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub agent_type: Option<String>,
    /// From ACP `meta.firstParty` — whether the resolved base URL is xAI's.
    /// `None` when the shell did not advertise the flag: the ACP-side default
    /// is permissive, so guessing here would label a custom provider
    /// first-party. The panel simply says nothing instead.
    pub first_party: Option<bool>,
    /// Effort menu ids in menu order; the default is flagged.
    pub efforts: Vec<(String, bool)>,
    pub supports_effort: bool,
    pub origin: RowOrigin,
    /// Fields a `[model."<key>"]` table set over a `[[provider]]` default.
    pub overrides: Vec<&'static str>,
    /// The entry carries its own credential. Presence only, never the value.
    pub own_credentials: bool,
    pub is_current: bool,
}

impl ProviderModelRow {
    /// Whether the slug on the wire is neither the catalog key nor the key's
    /// own trailing slug — the shape in which a catalog key once reached a
    /// vendor as a model name that does not exist.
    pub fn wire_slug_differs(&self) -> bool {
        let Some(slug) = self.wire_slug.as_deref() else {
            return false;
        };
        let tail = self.key.rsplit('/').next().unwrap_or(self.key.as_str());
        slug != self.key && slug != tail
    }

    /// Badge text shown on the collapsed row, so both silent-bug shapes are
    /// visible without expanding anything.
    pub fn badge(&self) -> String {
        let mut parts = Vec::new();
        if self.wire_slug_differs() {
            parts.push("⚠ wire ≠ key".to_string());
        }
        if !self.overrides.is_empty() {
            parts.push("⇄ overridden".to_string());
        }
        if self.is_current {
            parts.push("● current".to_string());
        }
        parts.join("  ")
    }

    /// One-line summary rendered under a collapsed row.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        parts.push(match self.wire_slug.as_deref() {
            Some(slug) => slug.to_string(),
            None => "wire slug not exposed".to_string(),
        });
        if let Some(backend) = self.api_backend.as_deref() {
            parts.push(backend.to_string());
        }
        if let Some(endpoint) = self.endpoint.as_deref().filter(|e| !e.is_empty()) {
            parts.push(endpoint.to_string());
        }
        if let Some(ctx) = self.context_tokens {
            parts.push(format!("ctx {}", format_thousands(ctx)));
        }
        match self.max_output_tokens {
            Some(max) => parts.push(format!("out {}", format_thousands(u64::from(max)))),
            None => parts.push("out —".to_string()),
        }
        parts.join("  ·  ")
    }

    /// Full field list shown when the row is expanded. Values are resolved,
    /// post-override values throughout; a field the pager cannot see says so
    /// rather than falling back to the declaration.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let unknown = "— not declared locally".to_string();
        let mut fields = Vec::new();

        fields.push((
            "catalog key",
            format!("{}  (addressed by /model and config)", self.key),
        ));
        fields.push((
            "wire slug",
            match self.wire_slug.as_deref() {
                Some(slug) if self.wire_slug_differs() => {
                    format!("{slug}  ⚠ differs from the catalog key")
                }
                Some(slug) => slug.to_string(),
                None => unknown.clone(),
            },
        ));
        fields.push((
            "endpoint",
            match self.endpoint.as_deref().filter(|e| !e.is_empty()) {
                Some(endpoint) => match self.first_party {
                    Some(true) => format!("{endpoint}  (first-party)"),
                    Some(false) => format!("{endpoint}  (third-party)"),
                    None => endpoint.to_string(),
                },
                None => unknown.clone(),
            },
        ));
        fields.push((
            "wire format",
            self.api_backend.clone().unwrap_or_else(|| unknown.clone()),
        ));
        fields.push((
            "ctx window",
            match self.context_tokens {
                Some(ctx) => format!("{} tokens", format_thousands(ctx)),
                None => unknown.clone(),
            },
        ));
        fields.push((
            "max output",
            match self.max_output_tokens {
                Some(max) => format!("{} tokens", format_thousands(u64::from(max))),
                None if matches!(self.api_backend.as_deref(), Some("messages")) => {
                    "none declared  ⚠ required by this wire format".to_string()
                }
                None if self.origin == RowOrigin::ServerCatalog => unknown.clone(),
                None => "none declared (vendor default)".to_string(),
            },
        ));
        fields.push(("effort", self.effort_summary()));
        fields.push((
            "agent type",
            self.agent_type.clone().unwrap_or_else(|| unknown.clone()),
        ));
        fields.push(("source", self.source_summary()));
        if !self.overrides.is_empty() {
            fields.push(("overrides", self.overrides.join(", ")));
        }
        fields.push((
            "credential",
            if self.origin == RowOrigin::ServerCatalog {
                unknown
            } else if self.own_credentials {
                "entry-owned".to_string()
            } else {
                "session / global".to_string()
            },
        ));
        fields
    }

    fn effort_summary(&self) -> String {
        if !self.supports_effort {
            return "not supported".to_string();
        }
        if self.efforts.is_empty() {
            return "supported, no menu advertised".to_string();
        }
        self.efforts
            .iter()
            .map(|(id, is_default)| {
                if *is_default {
                    format!("{id}*")
                } else {
                    id.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
            + "   (* default)"
    }

    fn source_summary(&self) -> String {
        match self.origin {
            RowOrigin::ProviderTable => "[[provider]] expansion".to_string(),
            RowOrigin::ModelTable => format!("[model.\"{}\"]", self.key),
            RowOrigin::Both => {
                format!("[[provider]] expansion + [model.\"{}\"]", self.key)
            }
            RowOrigin::ServerCatalog => "server catalog / built-in defaults".to_string(),
        }
    }
}

/// A provider and the models resolved under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderGroup {
    pub id: String,
    pub rows: Vec<ProviderModelRow>,
}

impl ProviderGroup {
    /// Right-aligned header text: model count plus how many rows carry a
    /// declared-vs-resolved disagreement.
    pub fn header_meta(&self) -> String {
        let flagged = self
            .rows
            .iter()
            .filter(|r| r.wire_slug_differs() || !r.overrides.is_empty())
            .count();
        let models = format!(
            "{} model{}",
            self.rows.len(),
            if self.rows.len() == 1 { "" } else { "s" }
        );
        if flagged == 0 {
            models
        } else {
            format!("{models}  ·  {flagged} flagged")
        }
    }
}

/// Reconcile the live ACP catalog with the locally resolved one into
/// provider-grouped rows, preserving catalog order.
pub fn build_groups(models: &ModelState, resolved: &ResolvedCatalog) -> Vec<ProviderGroup> {
    let mut groups: Vec<ProviderGroup> = Vec::new();
    for (id, info) in &models.available {
        let key = id.0.as_ref().to_string();
        let facts = resolved.get(&key);
        let group_id = resolved
            .provider_for(&key)
            .map(str::to_string)
            .or_else(|| meta_string(info, "provider"))
            .or_else(|| key.split_once('/').map(|(prefix, _)| prefix.to_string()))
            .unwrap_or_else(|| BUILTIN_GROUP.to_string());

        let origin = match facts {
            Some(f) if f.from_provider_table && f.from_model_table => RowOrigin::Both,
            Some(f) if f.from_provider_table => RowOrigin::ProviderTable,
            Some(f) if f.from_model_table => RowOrigin::ModelTable,
            _ => RowOrigin::ServerCatalog,
        };
        let default_effort =
            xai_grok_shell::sampling::types::parse_reasoning_effort_meta(info.meta.as_ref());
        let options = models.reasoning_effort_options_for(id);
        let row = ProviderModelRow {
            key: key.clone(),
            display_name: info.name.clone(),
            wire_slug: facts.map(|f| f.wire_slug.clone()),
            endpoint: facts.map(|f| f.endpoint.clone()),
            api_backend: facts.map(|f| f.api_backend.clone()),
            // The ACP catalog is authoritative for the window the running shell
            // will actually compact against; the local resolve is the fallback.
            context_tokens: meta_u64(info, "totalContextTokens")
                .or_else(|| facts.map(|f| f.context_window)),
            max_output_tokens: facts.and_then(|f| f.max_output_tokens),
            agent_type: meta_string(info, "agentType")
                .or_else(|| facts.map(|f| f.agent_type.clone())),
            first_party: info
                .meta
                .as_ref()
                .and_then(|m| m.get("firstParty"))
                .and_then(serde_json::Value::as_bool),
            supports_effort: !options.is_empty(),
            efforts: options
                .into_iter()
                .map(|opt| {
                    let is_default = default_effort == Some(opt.value) || opt.default;
                    (opt.id, is_default)
                })
                .collect(),
            origin,
            overrides: facts.map(|f| f.overrides.clone()).unwrap_or_default(),
            own_credentials: facts.is_some_and(|f| f.own_credentials),
            is_current: models.current.as_ref() == Some(id),
        };

        match groups.iter_mut().find(|g| g.id == group_id) {
            Some(group) => group.rows.push(row),
            None => groups.push(ProviderGroup {
                id: group_id,
                rows: vec![row],
            }),
        }
    }
    groups
}

fn meta_string(info: &acp::ModelInfo, key: &str) -> Option<String> {
    info.meta
        .as_ref()?
        .get(key)?
        .as_str()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn meta_u64(info: &acp::ModelInfo, key: &str) -> Option<u64> {
    info.meta.as_ref()?.get(key)?.as_u64()
}

/// `1000000` → `1,000,000`. Exact, not abbreviated: a panel that exists to
/// expose a 1M window mistaken for 256k must not round.
pub fn format_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (digits.len() - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

// ---------------------------------------------------------------------------
// Flattened list
// ---------------------------------------------------------------------------

/// One selectable line in the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEntry {
    /// A provider header, at `groups[group]`.
    Group { group: usize },
    /// A model, at `groups[group].rows[row]`.
    Model { group: usize, row: usize },
}

/// Flatten groups into the selectable entry list, honouring collapsed groups.
pub fn flatten(groups: &[ProviderGroup], collapsed: &[String]) -> Vec<PanelEntry> {
    let mut entries = Vec::new();
    for (g_idx, group) in groups.iter().enumerate() {
        entries.push(PanelEntry::Group { group: g_idx });
        if collapsed.iter().any(|id| id == &group.id) {
            continue;
        }
        for r_idx in 0..group.rows.len() {
            entries.push(PanelEntry::Model {
                group: g_idx,
                row: r_idx,
            });
        }
    }
    entries
}

// ---------------------------------------------------------------------------
// View state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ProvidersViewState {
    /// Index into [`flatten`]'s output.
    pub selected: usize,
    /// First visible entry index.
    pub viewport: usize,
    /// Provider ids whose models are hidden.
    pub collapsed: Vec<String>,
    pub window: ModalWindowState,
    /// Hit rects for mouse selection, paired with the entry index.
    pub row_hits: Vec<(Rect, usize)>,
    pub list_area: Option<Rect>,
}

impl ProvidersViewState {
    pub fn reset(&mut self) {
        self.selected = 0;
        self.viewport = 0;
        self.row_hits.clear();
        self.list_area = None;
    }

    pub fn clamp(&mut self, entry_count: usize) {
        if entry_count == 0 {
            self.selected = 0;
            self.viewport = 0;
            return;
        }
        self.selected = self.selected.min(entry_count - 1);
        self.viewport = self.viewport.min(self.selected);
    }

    pub fn select(&mut self, index: usize, entry_count: usize) {
        if entry_count == 0 {
            return;
        }
        self.selected = index.min(entry_count - 1);
    }

    pub fn move_by(&mut self, delta: isize, entry_count: usize) {
        if entry_count == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, entry_count as isize - 1);
        self.selected = next as usize;
    }

    pub fn is_collapsed(&self, group_id: &str) -> bool {
        self.collapsed.iter().any(|id| id == group_id)
    }

    pub fn set_collapsed(&mut self, group_id: &str, collapsed: bool) {
        let present = self.is_collapsed(group_id);
        if collapsed && !present {
            self.collapsed.push(group_id.to_string());
        } else if !collapsed && present {
            self.collapsed.retain(|id| id != group_id);
        }
    }

    pub fn toggle_collapsed(&mut self, group_id: &str) {
        let collapsed = self.is_collapsed(group_id);
        self.set_collapsed(group_id, !collapsed);
    }

    /// Scroll the viewport so the selected entry is on screen. Rows have
    /// variable height, so this is a conservative line-based follow using the
    /// heights measured on the previous frame.
    fn ensure_visible(&mut self, heights: &[u16], visible_rows: u16) {
        if heights.is_empty() || visible_rows == 0 {
            return;
        }
        if self.selected < self.viewport {
            self.viewport = self.selected;
            return;
        }
        loop {
            let used: u16 = heights[self.viewport..=self.selected.min(heights.len() - 1)]
                .iter()
                .copied()
                .sum();
            if used <= visible_rows || self.viewport >= self.selected {
                break;
            }
            self.viewport += 1;
        }
    }

    pub fn handle_scroll(&mut self, lines: i32, entry_count: usize) {
        if entry_count == 0 {
            return;
        }
        let delta = lines.unsigned_abs() as usize;
        if lines > 0 {
            self.viewport = (self.viewport + delta).min(entry_count - 1);
        } else {
            self.viewport = self.viewport.saturating_sub(delta);
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub mod shortcut_ids {
    pub const NAV: usize = 1;
    pub const FOLD: usize = 2;
    pub const CLOSE: usize = 3;
}

pub fn footer_shortcuts() -> Vec<crate::views::modal_window::Shortcut<'static>> {
    use crate::views::modal_window::Shortcut;
    vec![
        Shortcut {
            label: "↑↓ select",
            clickable: false,
            id: shortcut_ids::NAV,
        },
        Shortcut {
            label: "← → fold provider",
            clickable: false,
            id: shortcut_ids::FOLD,
        },
        Shortcut {
            label: "esc close",
            clickable: false,
            id: shortcut_ids::CLOSE,
        },
    ]
}

/// Render the panel. Returns the popup rect (for occlusion bookkeeping), or
/// `None` when the terminal is too small for the modal chrome.
pub fn render_providers(
    buf: &mut Buffer,
    area: Rect,
    groups: &[ProviderGroup],
    state: &mut ProvidersViewState,
) -> Option<Rect> {
    use crate::views::modal_window::{ModalSizing, ModalWindowConfig, render_modal_window};

    let theme = Theme::current();
    state.row_hits.clear();
    state.list_area = None;

    let shortcuts = footer_shortcuts();
    let config = ModalWindowConfig {
        title: "Providers",
        tabs: None,
        shortcuts: &shortcuts,
        sizing: ModalSizing::large(),
        fold_info: None,
    };
    let content = render_modal_window(buf, area, &mut state.window, &config, &theme)?;
    let inner = content.content;

    if groups.is_empty() {
        span_at(
            buf,
            inner.x + 1,
            inner.y + 1,
            "No models resolved yet.",
            Style::default().fg(theme.gray_bright),
            inner.right(),
        );
        span_at(
            buf,
            inner.x + 1,
            inner.y + 3,
            "The catalog arrives with the session; try again once it is connected.",
            Style::default().fg(theme.gray),
            inner.right(),
        );
        return state.window.popup_area;
    }

    let entries = flatten(groups, &state.collapsed);
    state.clamp(entries.len());
    let heights = entry_heights(groups, &entries, state.selected, inner.width);
    state.ensure_visible(&heights, inner.height);

    state.list_area = Some(Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    ));

    let bottom = inner.bottom();
    let mut y = inner.y;
    for (idx, entry) in entries.iter().enumerate().skip(state.viewport) {
        if y >= bottom {
            break;
        }
        let selected = idx == state.selected;
        let rows = match entry {
            PanelEntry::Group { group } => {
                let group = &groups[*group];
                let meta = group.header_meta();
                let row = PickerRow {
                    label: &group.id,
                    right_label: &meta,
                    selected,
                    expanded: !state.is_collapsed(&group.id),
                    fields: &[],
                    description_lines: &[],
                    summary_lines: &[],
                    dimmed: false,
                    indent: 0,
                    badge: "",
                    badge_color: None,
                    collapsible: true,
                    underline_last_desc: false,
                };
                render_picker_row(
                    buf,
                    inner.x + 1,
                    y,
                    inner.width.saturating_sub(2),
                    &theme,
                    &row,
                    false,
                    Some(theme.bg_base),
                    bottom.saturating_sub(y),
                )
                .rows
            }
            PanelEntry::Model { group, row } => {
                let model = &groups[*group].rows[*row];
                let badge = model.badge();
                let summary = model.summary();
                let summary_lines = [summary.as_str()];
                let owned_fields = model.fields();
                let fields: Vec<PickerField<'_>> = owned_fields
                    .iter()
                    .map(|(label, value)| PickerField {
                        label,
                        value: value.as_str(),
                    })
                    .collect();
                let row_view = PickerRow {
                    label: &model.key,
                    right_label: &model.display_name,
                    selected,
                    expanded: selected,
                    fields: if selected { &fields } else { &[] },
                    description_lines: &[],
                    summary_lines: if selected { &[] } else { &summary_lines },
                    dimmed: false,
                    indent: 1,
                    badge: &badge,
                    badge_color: Some(badge_color(model, &theme)),
                    collapsible: false,
                    underline_last_desc: false,
                };
                render_picker_row(
                    buf,
                    inner.x + 1,
                    y,
                    inner.width.saturating_sub(2),
                    &theme,
                    &row_view,
                    false,
                    Some(theme.bg_base),
                    bottom.saturating_sub(y),
                )
                .rows
            }
        };
        let row_h = rows.max(1);
        state.row_hits.push((
            Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), row_h),
            idx,
        ));
        y = y.saturating_add(row_h);
    }

    state.window.popup_area
}

fn badge_color(model: &ProviderModelRow, theme: &Theme) -> ratatui::style::Color {
    if model.wire_slug_differs() {
        theme.accent_error
    } else if !model.overrides.is_empty() {
        theme.warning
    } else {
        theme.gray
    }
}

/// Approximate rendered height of each entry, used only for scroll-follow.
/// An expanded model row costs one line per field plus its own line.
fn entry_heights(
    groups: &[ProviderGroup],
    entries: &[PanelEntry],
    selected: usize,
    _width: u16,
) -> Vec<u16> {
    entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| match entry {
            PanelEntry::Group { .. } => 1,
            PanelEntry::Model { group, row } => {
                if idx == selected {
                    let fields = groups[*group].rows[*row].fields().len() as u16;
                    fields.saturating_add(1)
                } else {
                    2
                }
            }
        })
        .collect()
}

fn span_at(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style, max_x: u16) {
    if max_x <= x {
        return;
    }
    buf.set_span_safe(
        x,
        y,
        &ratatui::text::Span::styled(text.to_string(), style),
        max_x - x,
    );
}

/// Bold style for the panel title row; kept here so the overlay and the view
/// agree on emphasis.
pub fn title_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.text_primary)
        .add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::resolved_catalog::ResolvedCatalog;
    use std::sync::Arc;
    use xai_grok_shell::agent::config::Config as AgentConfig;

    fn resolved_from(toml_src: &str) -> ResolvedCatalog {
        let raw: toml::Value = toml::from_str(toml_src).expect("fixture parses");
        let cfg = AgentConfig::new_from_toml_cfg(&raw).expect("fixture builds a config");
        ResolvedCatalog::from_agent_config(&cfg)
    }

    fn model_state(entries: &[(&str, &str, serde_json::Value)]) -> ModelState {
        let mut state = ModelState::default();
        for (key, name, meta) in entries {
            let id = acp::ModelId::new(Arc::from(*key));
            state.available.insert(
                id.clone(),
                acp::ModelInfo::new(id, (*name).to_string()).meta(meta.as_object().cloned()),
            );
        }
        state.current = state.available.keys().next().cloned();
        state
    }

    const PROVIDER_WITH_OVERRIDE: &str = r#"
        [[provider]]
        id = "acme"
        format = "messages"
        base_url = "https://api.example.test/v1"
        models = ["some-model", "other-model"]
        context_window = 256000
        max_completion_tokens = 8000

        [model."acme/some-model"]
        model = "some-model-wire-2"
        context_window = 1000000
        max_completion_tokens = 64000
    "#;

    /// The panel must render the post-override values, never the provider's
    /// declared ones.
    #[test]
    fn row_shows_resolved_values_not_declared_ones() {
        let resolved = resolved_from(PROVIDER_WITH_OVERRIDE);
        let models = model_state(&[
            (
                "acme/some-model",
                "Some Model",
                serde_json::json!({ "totalContextTokens": 1_000_000, "provider": "acme" }),
            ),
            (
                "acme/other-model",
                "Other Model",
                serde_json::json!({ "totalContextTokens": 256_000, "provider": "acme" }),
            ),
        ]);

        let groups = build_groups(&models, &resolved);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id, "acme");

        let overridden = &groups[0].rows[0];
        assert_eq!(overridden.wire_slug.as_deref(), Some("some-model-wire-2"));
        assert_eq!(overridden.context_tokens, Some(1_000_000));
        assert_eq!(overridden.max_output_tokens, Some(64_000));
        assert_eq!(overridden.origin, RowOrigin::Both);
        assert_eq!(
            overridden.overrides,
            vec!["model", "context_window", "max_completion_tokens"]
        );

        // The sibling that no table touched keeps the provider's own values,
        // proving the override is attributed to one entry and not the group.
        let untouched = &groups[0].rows[1];
        assert_eq!(untouched.wire_slug.as_deref(), Some("other-model"));
        assert_eq!(untouched.context_tokens, Some(256_000));
        assert_eq!(untouched.max_output_tokens, Some(8_000));
        assert_eq!(untouched.origin, RowOrigin::ProviderTable);
        assert!(untouched.overrides.is_empty());
    }

    #[test]
    fn overridden_fields_are_listed_in_the_expanded_row() {
        let resolved = resolved_from(PROVIDER_WITH_OVERRIDE);
        let models = model_state(&[(
            "acme/some-model",
            "Some Model",
            serde_json::json!({ "totalContextTokens": 1_000_000 }),
        )]);
        let groups = build_groups(&models, &resolved);
        let fields = groups[0].rows[0].fields();
        let find = |label: &str| {
            fields
                .iter()
                .find(|(l, _)| *l == label)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(
            find("overrides"),
            "model, context_window, max_completion_tokens"
        );
        assert!(find("source").contains("[[provider]] expansion + [model."));
        assert_eq!(find("ctx window"), "1,000,000 tokens");
        assert_eq!(find("max output"), "64,000 tokens");
        assert_eq!(find("wire format"), "messages");
    }

    /// A wire slug that is not the key's own slug is flagged on the collapsed
    /// row and again in the expanded field.
    #[test]
    fn wire_slug_differing_from_key_is_marked() {
        let resolved = resolved_from(PROVIDER_WITH_OVERRIDE);
        let models = model_state(&[
            ("acme/some-model", "Some Model", serde_json::json!({})),
            ("acme/other-model", "Other Model", serde_json::json!({})),
        ]);
        let groups = build_groups(&models, &resolved);

        let flagged = &groups[0].rows[0];
        assert!(flagged.wire_slug_differs());
        assert!(flagged.badge().contains("⚠ wire ≠ key"));
        let wire_field = flagged
            .fields()
            .into_iter()
            .find(|(label, _)| *label == "wire slug")
            .expect("wire slug field");
        assert_eq!(
            wire_field.1,
            "some-model-wire-2  ⚠ differs from the catalog key"
        );

        let plain = &groups[0].rows[1];
        assert!(!plain.wire_slug_differs());
        assert!(!plain.badge().contains("wire ≠ key"));
        assert_eq!(groups[0].header_meta(), "2 models  ·  1 flagged");
    }

    /// A messages-format entry with no declared ceiling is called out — every
    /// request against one of those failed at build time.
    #[test]
    fn messages_provider_without_output_ceiling_is_called_out() {
        let resolved = resolved_from(
            r#"
            [[provider]]
            id = "acme"
            format = "messages"
            base_url = "https://api.example.test/v1"
            models = ["some-model"]
            "#,
        );
        let models = model_state(&[("acme/some-model", "Some Model", serde_json::json!({}))]);
        let groups = build_groups(&models, &resolved);
        let max_output = groups[0].rows[0]
            .fields()
            .into_iter()
            .find(|(label, _)| *label == "max output")
            .expect("max output field")
            .1;
        assert_eq!(max_output, "none declared  ⚠ required by this wire format");
    }

    /// An entry with no local declaration says so instead of inventing values.
    #[test]
    fn prefetch_only_entry_reports_what_is_not_visible() {
        let resolved = resolved_from("");
        let models = model_state(&[(
            "some-model",
            "Some Model",
            serde_json::json!({ "totalContextTokens": 256_000, "agentType": "example-agent" }),
        )]);
        let groups = build_groups(&models, &resolved);
        let row = &groups[0].rows[0];
        assert_eq!(groups[0].id, BUILTIN_GROUP);
        assert_eq!(row.origin, RowOrigin::ServerCatalog);
        assert_eq!(row.wire_slug, None);
        assert!(
            !row.wire_slug_differs(),
            "unknown must not read as a mismatch"
        );
        assert_eq!(row.context_tokens, Some(256_000));
        assert_eq!(row.agent_type.as_deref(), Some("example-agent"));
        let fields = row.fields();
        let wire = fields
            .iter()
            .find(|(l, _)| *l == "wire slug")
            .expect("wire slug field");
        assert_eq!(wire.1, "— not declared locally");
        assert!(row.summary().contains("wire slug not exposed"));
    }

    #[test]
    fn effort_menu_marks_the_default() {
        let resolved = resolved_from("");
        let models = model_state(&[(
            "some-model",
            "Some Model",
            serde_json::json!({
                "supportsReasoningEffort": true,
                "reasoningEffort": "high",
                "reasoningEfforts": [
                    { "id": "deep", "value": "xhigh", "label": "Deep" },
                    { "id": "high", "value": "high", "label": "High" },
                ],
            }),
        )]);
        let groups = build_groups(&models, &resolved);
        let effort = groups[0].rows[0]
            .fields()
            .into_iter()
            .find(|(l, _)| *l == "effort")
            .expect("effort field")
            .1;
        assert_eq!(effort, "deep · high*   (* default)");
    }

    #[test]
    fn models_group_under_their_provider_even_when_the_slug_was_renamed() {
        let resolved = resolved_from(PROVIDER_WITH_OVERRIDE);
        let models = model_state(&[
            // No `provider` in meta: the shell omits it once the key no longer
            // ends with the entry's slug, which is exactly the renamed case.
            ("acme/some-model", "Some Model", serde_json::json!({})),
            ("plain-model", "Plain Model", serde_json::json!({})),
        ]);
        let groups = build_groups(&models, &resolved);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].id, "acme");
        assert_eq!(groups[1].id, BUILTIN_GROUP);
    }

    #[test]
    fn flatten_hides_rows_of_a_collapsed_group() {
        let resolved = resolved_from(PROVIDER_WITH_OVERRIDE);
        let models = model_state(&[
            ("acme/some-model", "Some Model", serde_json::json!({})),
            ("acme/other-model", "Other Model", serde_json::json!({})),
        ]);
        let groups = build_groups(&models, &resolved);
        assert_eq!(flatten(&groups, &[]).len(), 3);
        let collapsed = vec!["acme".to_string()];
        assert_eq!(
            flatten(&groups, &collapsed),
            vec![PanelEntry::Group { group: 0 }]
        );
    }

    #[test]
    fn format_thousands_is_exact() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(256_000), "256,000");
        assert_eq!(format_thousands(1_000_000), "1,000,000");
    }

    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell(ratatui::layout::Position::new(x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    fn render_fixture(width: u16, height: u16) -> String {
        let resolved = resolved_from(PROVIDER_WITH_OVERRIDE);
        let models = model_state(&[
            ("acme/some-model", "Some Model", serde_json::json!({})),
            ("acme/other-model", "Other Model", serde_json::json!({})),
        ]);
        let groups = build_groups(&models, &resolved);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        // Select the overridden model (entry 0 is the provider header).
        let mut state = ProvidersViewState {
            selected: 1,
            ..Default::default()
        };
        render_providers(&mut buf, area, &groups, &mut state);
        buffer_text(&buf)
    }

    /// End-to-end: the rendered panel carries the resolved values and the
    /// mismatch marker, not the declared ones.
    #[test]
    fn rendered_panel_shows_resolved_values_and_mismatch_marker() {
        let text = render_fixture(120, 40);
        assert!(text.contains("Providers"), "title missing: {text}");
        assert!(text.contains("acme"), "provider group missing");
        assert!(
            text.contains("some-model-wire-2"),
            "resolved wire slug missing: {text}"
        );
        assert!(
            text.contains("differs from the catalog key"),
            "mismatch marker missing: {text}"
        );
        assert!(
            text.contains("1,000,000"),
            "resolved context window missing: {text}"
        );
        assert!(text.contains("64,000"), "resolved output ceiling missing");

        // The provider declared 256k / 8k for this key; the per-model table won.
        // The sibling row legitimately shows the provider values, so scope the
        // check to the overridden entry's own field lines.
        let ctx_line = text
            .lines()
            .find(|line| line.contains("ctx window"))
            .expect("ctx window field line");
        assert!(
            ctx_line.contains("1,000,000") && !ctx_line.contains("256,000"),
            "declared value presented as resolved: {ctx_line:?}"
        );
        let out_line = text
            .lines()
            .find(|line| line.contains("max output"))
            .expect("max output field line");
        assert!(
            out_line.contains("64,000") && !out_line.contains("8,000"),
            "declared value presented as resolved: {out_line:?}"
        );
    }

    /// A narrow terminal truncates rows; it must not panic or paint outside
    /// the buffer.
    #[test]
    fn narrow_terminal_renders_without_overflow() {
        for (w, h) in [(40u16, 20u16), (24, 12), (20, 8), (12, 6)] {
            let text = render_fixture(w, h);
            for line in text.lines() {
                assert!(
                    line.chars().count() <= usize::from(w),
                    "line wider than {w}: {line:?}"
                );
            }
        }
    }
}
