//! Shared reasoning-effort dropdown levels for `/model` and `/effort`.

use xai_grok_shell::sampling::types::{ReasoningEffort, ReasoningEffortOption};

use crate::slash::command::ArgItem;

/// Effort levels in the built-in fallback menu (strongest first).
/// `none`/`minimal` are still accepted by `ReasoningEffort::from_str` for power users.
pub(crate) const EFFORT_LEVELS: &[ReasoningEffort] = &[
    ReasoningEffort::Xhigh,
    ReasoningEffort::High,
    ReasoningEffort::Medium,
    ReasoningEffort::Low,
];

pub(crate) fn effort_description(level: ReasoningEffort) -> &'static str {
    match level {
        ReasoningEffort::None => "No reasoning",
        ReasoningEffort::Minimal => "Minimal reasoning",
        ReasoningEffort::Low => "Faster, lighter reasoning",
        ReasoningEffort::Medium => "Balanced reasoning",
        ReasoningEffort::High => "Heavy reasoning",
        ReasoningEffort::Xhigh => "Extended reasoning",
        ReasoningEffort::Max => "Maximum reasoning",
    }
}

/// The built-in menu used when the server sends no `reasoningEfforts`.
/// Reproduces the historical rows: labels are the lowercase level (via `Display`), descriptions from `effort_description`.
/// The active row is matched by value against the session effort at render time, so `default` is left unset here.
pub(crate) fn legacy_effort_options() -> Vec<ReasoningEffortOption> {
    EFFORT_LEVELS
        .iter()
        .map(|&level| ReasoningEffortOption {
            id: level.as_str().to_string(),
            value: level,
            label: level.to_string(),
            description: Some(effort_description(level).to_string()),
            default: false,
        })
        .collect()
}

/// Build effort rows for autocomplete from a per-model option list.
///
/// - `mark_active` and `current_effort` mark the current session effort with `(active)`.
/// - `insert_text_for` controls what is inserted on select:
///   - `/effort`: the option id (`"deep"`)
///   - `/model` chained phase: `"ModelName deep"`
///
/// Rows come out in `options` order, and both dropdowns keep that order on equal-scoring rows,
/// so `match_text` carries only text the user can see on the row or type into the prompt.
pub(crate) fn build_effort_arg_items(
    options: &[ReasoningEffortOption],
    current_effort: Option<ReasoningEffort>,
    mark_active: bool,
    insert_text_for: impl Fn(&ReasoningEffortOption) -> String,
) -> Vec<ArgItem> {
    options
        .iter()
        .map(|option| {
            let active = mark_active && current_effort == Some(option.value);
            let active_suffix = if active { " (active)" } else { "" };
            let insert_text = insert_text_for(option);
            ArgItem {
                display: format!("{}{active_suffix}", option.label),
                match_text: insert_text.clone(),
                insert_text,
                description: option.description.clone().unwrap_or_default(),
            }
        })
        .collect()
}

/// Whether `query` keeps `item` in the `ArgPicker` modal (`app/modals.rs` filters rows
/// by substring over `match_text`, `display` and `description`) while neither text the
/// row draws contains it. Such a row answers a query with nothing on it that explains
/// the hit, so a test asserts no effort row ever does.
#[cfg(test)]
pub(crate) fn matches_only_on_text_the_row_never_draws(item: &ArgItem, query: &str) -> bool {
    let q = query.to_lowercase();
    item.match_text.to_lowercase().contains(&q)
        && !item.display.to_lowercase().contains(&q)
        && !item.description.to_lowercase().contains(&q)
}
