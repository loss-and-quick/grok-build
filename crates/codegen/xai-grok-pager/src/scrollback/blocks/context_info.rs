//! ContextInfoBlock — typed `/context` display rendered in scrollback.
//!
//! Holds the [`ContextFacts`] resolved when the block was created, plus the
//! active model name, and rebuilds the styled output on every `output()` call.
//! This is the same pattern as [`super::SessionEventBlock`]: keep typed data,
//! format at render time. The payoff is theme-reactivity — `Theme::current()`
//! is re-resolved on every redraw, so switching themes after running
//! `/context` updates the colors immediately instead of leaving stale
//! baked-in values.
//!
//! The split is deliberate: every number is decided once, in
//! [`crate::acp::context_facts`], and this module only chooses glyphs, colors
//! and column widths for them. Arithmetic added back into `build_lines` would
//! be recomputed on each redraw and unreachable from a test.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::acp::context_facts::{BarPartition, ContextFacts, ContributorKind};
use crate::acp::tracker::CompactionRecord;
use crate::appearance::AppearanceConfig;
use crate::render::wrapping::word_wrap_lines;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{AccentStyle, BlockContext, BlockLine, BlockOutput};
use crate::theme::{Theme, quantize};
use xai_grok_shell::session::ContextInfo;

/// Block that renders a `/context` snapshot in scrollback.
///
/// Layout (all left-aligned to column 0):
///
/// ```text
/// Context
///
/// 36.7k / 1.0m tokens (3.67%)
/// grok-4
///
/// ◆ ◆ ◆ ◆ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
/// ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
/// ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
/// ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
/// ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
///
/// ◆ System prompt     1.2k tokens  (0.1%)   (gray)
/// ◆ Messages         29.9k tokens    (3%)
/// ◆ Tool schemas      5.6k tokens  (0.6%) · 12 tools   (teal)
/// ◆ Unattributed      3.3k tokens  (0.3%)              (violet)
/// ◇ Free              963k tokens   (96%)
///
/// Unattributed: reasoning, per-request scaffolding, and the gap between
/// this estimate and the provider's tokenizer.
///
/// ◈ Skills            2.4k tokens  (0.2%) · 21 skills
/// ◈ MCP servers        320 tokens  (0.1%) ·  4 servers
///
/// Auto-compact at 85% · ~812k tokens remaining
///
/// Compaction
/// 2 compactions · 1.6m tokens recovered · 1.2s spent
///  1  858k → 43.0k tokens  ·  815k recovered  (0.5s)
///  2  900k → 60.0k tokens  ·  840k recovered  (0.7s)
///
/// Turns: 5 · Tool calls: 12 · Compactions: 0
/// ```
///
/// The bar is a categorical breakdown: each cell uses its category's glyph
/// and color. System (gray ◆), messages (primary ◆), tool schemas (teal ◆)
/// and the unattributed remainder (violet ◆) fill left-to-right in legend
/// order, and the rest renders as muted ◇ outlines for free capacity.
///
/// Every ◆ band is a partition of `used`; the ◈ rows below are informational
/// — their tokens are already counted inside one of the bands above, so they
/// never enter the bar. The glyph is the whole distinction: ◆ adds up to the
/// window, ◈ does not.
#[derive(Debug, Clone)]
pub struct ContextInfoBlock {
    /// Facts resolved when the block was created. Held rather than the raw
    /// snapshot so redraws only restyle — every number is already decided.
    pub facts: ContextFacts,
    /// Active model name at the time of capture (display-only).
    pub model: String,
}

/// Shape of the categorical bar — how the 100 cells are laid out.
///
/// Two layouts ship today:
///
/// - `WIDE`: 5 rows × 20 cells = 100 cells, ~39 columns wide. The
///   default when the terminal has room.
/// - `NARROW`: 10 rows × 10 cells = 100 cells, ~19 columns wide. Used
///   when terminal width drops below [`BarLayout::NARROW_BREAKPOINT`]
///   so the bar still fits on narrow terminals (tmux split panes,
///   small terminal windows, embedded shells).
///
/// Both shapes hold the same 100 cells, so the visual breakdown
/// (which categories occupy which share of the bar) is identical —
/// only the aspect ratio changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BarLayout {
    /// Cells per row.
    row_len: usize,
    /// Number of rows.
    rows: usize,
}

impl BarLayout {
    /// 5 rows × 20 cells. Renders in ~39 columns (20 glyphs + 19
    /// separator spaces).
    const WIDE: Self = Self {
        row_len: 20,
        rows: 5,
    };

    /// 10 rows × 10 cells. Renders in ~19 columns (10 glyphs + 9
    /// separator spaces).
    const NARROW: Self = Self {
        row_len: 10,
        rows: 10,
    };

    /// Terminal width (in columns) at which the bar switches from
    /// [`Self::WIDE`] to [`Self::NARROW`]. The wide layout needs 39
    /// columns just for the bar; 50 leaves ~11 columns of margin and
    /// is also roughly where the legend rows
    /// (e.g. `◆ Tool schemas  5.6k tokens   (0.6%) · 12 tools`)
    /// start to word-wrap, so the breakpoint is consistent with the
    /// rest of the block's responsive behavior.
    const NARROW_BREAKPOINT: u16 = 50;

    /// Choose a layout that fits the available terminal width.
    fn for_width(width: u16) -> Self {
        if width < Self::NARROW_BREAKPOINT {
            Self::NARROW
        } else {
            Self::WIDE
        }
    }

    /// Total cells in the bar. Always 100 for both shipped layouts;
    /// kept as a method so future layouts with a different total cell
    /// count don't silently misalign with the legend percentages.
    const fn total(self) -> usize {
        self.row_len * self.rows
    }
}

/// What the unattributed row is actually made of, printed under the legend.
///
/// Pre-split into short lines rather than word-wrapped at render time: the
/// usage modal deliberately does not wrap (one row per logical line is what
/// keeps its scroll clamp exact), so a single long line would be clipped
/// there. Each line stays under [`BarLayout::NARROW_BREAKPOINT`] columns so
/// the note survives the narrowest layout the block draws.
const UNATTRIBUTED_NOTE: [&str; 4] = [
    "Unattributed = used minus what can be measured",
    "here: reasoning, per-request scaffolding, and the",
    "drift between this client's bytes/4 estimate and",
    "the provider's tokenizer.",
];

/// Footer for the injected-context view: what it does and does not cover.
///
/// Printed after the blocks rather than before them so the first thing on the
/// screen is the largest block, not a paragraph about the screen.
///
/// Kept as paragraphs rather than pre-split lines because this view knows the
/// content width and wraps everything it emits — unlike the legend footnote,
/// which is measured against a bar whose layout is chosen for it.
const INJECTION_NOTE: [&str; 3] = [
    "These blocks ride in every request and are already counted in the bands \
     on the Context usage tab, so they are listed here rather than added to \
     them.",
    "",
    "Not listed: the system prompt and the tool schemas, both sized on the \
     Context usage tab; and the <user_info> prefix these blocks hang off, \
     which cannot be re-rendered without re-running the git status it \
     carries.",
];

/// One legend or informational row, before column formatting.
struct LegendRow {
    glyph: &'static str,
    color: Color,
    label: String,
    tokens: u64,
    detail: Option<String>,
}

/// Column widths for the legend and informational rows, measured from the
/// rows that actually render so token counts, percentages, and detail
/// counts stay aligned no matter the labels or magnitudes.
struct RowLayout {
    label_width: usize,
    tokens_width: usize,
    percent_width: usize,
    count_width: usize,
}

impl RowLayout {
    /// Measure column widths over every row that will render. Widths are
    /// in codepoints, not bytes.
    fn measure<'a>(rows: impl Iterator<Item = &'a LegendRow> + Clone, total: u64) -> Self {
        Self {
            label_width: rows
                .clone()
                .map(|r| r.label.chars().count())
                .max()
                .unwrap_or(0)
                + 1,
            tokens_width: rows
                .clone()
                .map(|r| fmt_tok(r.tokens).chars().count())
                .max()
                .unwrap_or(0),
            percent_width: rows
                .clone()
                .map(|r| Self::percent(r.tokens, total).chars().count())
                .max()
                .unwrap_or(0),
            count_width: rows
                .filter_map(|r| r.detail.as_deref())
                .filter_map(|d| d.split(' ').next().map(|n| n.chars().count()))
                .max()
                .unwrap_or(0),
        }
    }

    /// The parenthesized share of the window, e.g. `"(0.6%)"`.
    fn percent(tokens: u64, total: u64) -> String {
        format!("({})", percent_of_window(tokens, total))
    }

    /// The row's numeric cells: tokens and percent, each right-aligned.
    fn cells(&self, tokens: u64, total: u64) -> String {
        format!(
            "{:>tokens_width$} tokens   {:>percent_width$}",
            fmt_tok(tokens),
            Self::percent(tokens, total),
            tokens_width = self.tokens_width,
            percent_width = self.percent_width,
        )
    }

    /// The detail suffix with the leading count right-aligned so the nouns
    /// line up: `" · 25 tools"` over `" ·  1 server"`. Details follow the
    /// `TokenUsageCategory::detail` count-then-noun convention; text with
    /// no space renders unaligned.
    fn detail_suffix(&self, detail: &str) -> String {
        match detail.split_once(' ') {
            Some((count, rest)) => {
                format!(
                    " \u{00b7} {count:>count_width$} {rest}",
                    count_width = self.count_width
                )
            }
            None => format!(" \u{00b7} {detail}"),
        }
    }

    /// Render one row. WIDE: a single column-aligned line. NARROW: glyph
    /// and label on the first line, numeric data indented by one space on
    /// the second so it clusters under the label.
    fn render(
        &self,
        row: &LegendRow,
        bar: BarLayout,
        total: u64,
        label_style: Style,
        muted: Style,
    ) -> Vec<Line<'static>> {
        let glyph = Span::styled(format!("{} ", row.glyph), Style::default().fg(row.color));
        let suffix = row.detail.as_deref().map(|d| self.detail_suffix(d));
        if bar == BarLayout::NARROW {
            let first = Line::from(vec![glyph, Span::styled(row.label.clone(), label_style)]);
            let mut second = vec![
                Span::raw(" "),
                Span::styled(
                    format!(
                        "{} tokens   {}",
                        fmt_tok(row.tokens),
                        Self::percent(row.tokens, total)
                    ),
                    muted,
                ),
            ];
            if let Some(extra) = suffix {
                second.push(Span::styled(extra, muted));
            }
            vec![first, Line::from(second)]
        } else {
            let mut spans = vec![
                glyph,
                Span::styled(
                    format!(
                        "{:<label_width$}",
                        row.label,
                        label_width = self.label_width
                    ),
                    label_style,
                ),
                Span::raw(" "),
                Span::styled(self.cells(row.tokens, total), muted),
            ];
            if let Some(extra) = suffix {
                spans.push(Span::styled(extra, muted));
            }
            vec![Line::from(spans)]
        }
    }
}

impl ContextInfoBlock {
    /// Create a new context-info block, resolving the facts up front.
    pub fn new(
        snapshot: ContextInfo,
        history: &[CompactionRecord],
        model: impl Into<String>,
    ) -> Self {
        Self {
            facts: ContextFacts::resolve(&snapshot, history),
            model: model.into(),
        }
    }

    /// Build the "Injected context" view: one section per injected block,
    /// each headed by its measured size and followed by the text that size
    /// was measured over.
    ///
    /// Lives here rather than in the modal for the same reason `build_lines`
    /// does — the facts are already resolved, and the modal's own renderer
    /// deliberately does not wrap, so wrapping has to happen where the width
    /// is known and the content is owned.
    pub(crate) fn injection_lines(&self, theme: &Theme, width: u16) -> Vec<Line<'static>> {
        let muted = theme.muted();
        let primary = Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD);
        let label_style = Style::default().fg(theme.text_secondary);
        let mut lines = vec![Line::from(Span::styled("Injected context", primary))];
        lines.push(Line::from(""));

        // No early return for the empty case: every line this view emits has
        // to leave through the wrap below, or it is the one line that gets
        // clipped.
        if self.facts.itemized.is_empty() {
            lines.push(Line::from(Span::styled(
                "This session injects nothing beyond the system prompt.",
                muted,
            )));
        } else {
            for row in &self.facts.itemized {
                let mut head = vec![
                    Span::styled(format!("{} ", crate::glyphs::diamond_dotted()), label_style),
                    Span::styled(row.label.clone(), label_style),
                    Span::styled(
                        format!(
                            "  {} tokens ({})",
                            fmt_tok(row.tokens),
                            percent_of_window(row.tokens, self.facts.total)
                        ),
                        muted,
                    ),
                ];
                if let Some(detail) = &row.detail {
                    head.push(Span::styled(format!(" \u{00b7} {detail}"), muted));
                }
                lines.push(Line::from(head));
                match row.text.as_deref() {
                    Some(text) => lines.extend(
                        text.lines()
                            .map(|l| Line::from(Span::styled(l.to_string(), muted))),
                    ),
                    // An older shell sends the size without the text. Say which of
                    // the two is missing rather than rendering an empty section
                    // that reads as "this block is empty".
                    None => lines.push(Line::from(Span::styled(
                        "  (this agent reports the size but not the text)",
                        muted,
                    ))),
                }
                lines.push(Line::from(""));
            }
            lines.extend(
                INJECTION_NOTE
                    .iter()
                    .map(|l| Line::from(Span::styled(*l, muted))),
            );
        }
        // Wrap everything once, at the end: the usage modal renders one row
        // per logical line and clips the rest, so any line this view emits
        // past the right edge would simply be lost — including the injected
        // text the view exists to show. `.max(20)` keeps a pathologically
        // narrow pane from wrapping to nothing.
        word_wrap_lines(lines, (width as usize).max(20))
    }

    /// Build the styled lines for an arbitrary content width. Reused by the
    /// usage modal's "Context usage" tab so the modal and the minimal-mode
    /// scrollback block render the same breakdown.
    pub(crate) fn lines_for_width(&self, theme: &Theme, width: u16) -> Vec<Line<'static>> {
        self.build_lines(theme, BarLayout::for_width(width))
    }

    /// Build the styled lines using the supplied theme and bar layout.
    ///
    /// Called from `output()` on every redraw so theme switches take effect
    /// without re-running `/context`. The theme is passed in (rather than
    /// re-resolved here) so a single `Theme::current()` lookup in `output()`
    /// is shared with the `max_lines` truncation branch.
    ///
    /// `bar` controls the shape of the categorical bar — the wide layout
    /// (5×20) is the default; `output()` switches to the narrow layout
    /// (10×10) when terminal width drops below `BarLayout::NARROW_BREAKPOINT`
    /// so the bar still fits on column-constrained terminals.
    fn build_lines(&self, theme: &Theme, bar: BarLayout) -> Vec<Line<'static>> {
        let facts = &self.facts;
        let model = &self.model;

        let used = facts.used;
        let total = facts.total;

        let muted = theme.muted();
        let primary = Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD);

        // Per-category colors used in both the bar and the legend so the
        // two visualizations are scannable side-by-side. Messages get the
        // brightest treatment (primary) — they dominate the breakdown and
        // are the conversation the user is actually steering. System
        // prompt uses the same diamond glyph as messages but in muted
        // gray so it reads as a quiet base layer.
        let system_color = quantize(theme.gray_bright); // gray
        let tools_color = quantize(theme.accent_skill); // teal / skill accent
        let messages_color = quantize(theme.text_primary); // primary (white)
        let empty_color = quantize(theme.gray_dim); // free / outline
        let overhead_color = quantize(theme.accent_verify);

        // Categorical bar: 100 cells laid out as `bar.rows` rows of
        // `bar.row_len` cells with one space between cells. Each category
        // gets its own glyph + color so the bar reads as a stacked
        // breakdown at a glance.
        // Routed through `glyphs` so the diamonds degrade to CP437-safe
        // stand-ins (`◆`→`♦`, `◇`→`○`) on legacy Windows consoles that
        // can't render the U+25Cx diamonds.
        let system_glyph = crate::glyphs::diamond_filled(); // ◆ (gray)
        let messages_glyph = crate::glyphs::diamond_filled(); // ◆ (primary)
        let schemas_glyph = crate::glyphs::diamond_filled(); // ◆ (teal)
        let free_glyph = crate::glyphs::diamond_hollow(); // ◇
        let overhead_glyph = crate::glyphs::diamond_filled(); // ◆ (violet)
        // ◈ is reserved for rows that do NOT partition the window, so a reader
        // can tell at a glance which rows sum to `used` and which are already
        // counted inside one of them.
        let info_glyph = crate::glyphs::diamond_dotted(); // ◈

        // The partition is resolved against `BarPartition::CELLS`; the layout
        // only chooses how those cells are wrapped into rows. `LAYOUTS_HOLD_A
        // _FULL_PARTITION` fails the build if a future shape stops agreeing.
        let total_cells = bar.total();
        let partition = facts.bar;
        let mut cells: Vec<(&'static str, Color)> = Vec::with_capacity(total_cells);
        for _ in 0..partition.system {
            cells.push((system_glyph, system_color));
        }
        for _ in 0..partition.messages {
            cells.push((messages_glyph, messages_color));
        }
        for _ in 0..partition.tools {
            cells.push((schemas_glyph, tools_color));
        }
        for _ in 0..partition.unattributed {
            cells.push((overhead_glyph, overhead_color));
        }
        for _ in 0..partition.free {
            cells.push((free_glyph, empty_color));
        }
        debug_assert_eq!(cells.len(), total_cells);

        let mut bar_lines: Vec<Line<'static>> = Vec::with_capacity(bar.rows);
        for row_idx in 0..bar.rows {
            let start = row_idx * bar.row_len;
            let end = (start + bar.row_len).min(cells.len());
            let mut spans = Vec::with_capacity(bar.row_len * 2);
            for (i, (glyph, color)) in cells[start..end].iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    (*glyph).to_string(),
                    Style::default().fg(*color),
                ));
            }
            bar_lines.push(Line::from(spans));
        }

        // Legend rows fill the bar; informational rows sit below it because
        // their tokens are already counted in one of the bands — the injected
        // blocks land in the first user turn, so they overlap Messages.
        // Kinds carry no styling of their own — the resolver never names a
        // glyph or a color — so the mapping to chrome lives here.
        let chrome = |kind: ContributorKind| -> (&'static str, Color) {
            match kind {
                ContributorKind::SystemPrompt => (system_glyph, system_color),
                ContributorKind::Messages => (messages_glyph, messages_color),
                ContributorKind::ToolSchemas => (schemas_glyph, tools_color),
                ContributorKind::Unattributed => (overhead_glyph, overhead_color),
                ContributorKind::Free => (free_glyph, empty_color),
                ContributorKind::Itemized => (info_glyph, tools_color),
            }
        };
        let to_row = |c: &crate::acp::context_facts::Contributor| {
            let (glyph, color) = chrome(c.kind);
            LegendRow {
                glyph,
                color,
                label: c.label.clone(),
                tokens: c.tokens,
                detail: c.detail.clone(),
            }
        };
        let legend_rows: Vec<LegendRow> = facts.contributors.iter().map(to_row).collect();
        let info_rows: Vec<LegendRow> = facts.itemized.iter().map(to_row).collect();
        let layout = RowLayout::measure(legend_rows.iter().chain(info_rows.iter()), total);
        let label_style = Style::default().fg(theme.text_secondary);

        let mut lines: Vec<Line<'static>> = vec![
            // Header: bold white "Context"
            Line::from(Span::styled("Context", primary)),
            // Blank row between header and the at-a-glance summary
            Line::from(""),
            // Sub-header: token totals + percent. Uses `text_secondary` for
            // a touch more contrast than `muted` so the at-a-glance numbers
            // stand apart from the breakdown/footer rows. Switches to "m"
            // with one decimal place once a value reaches a million so wide
            // context windows (e.g. 1m / 2m / 4m) read naturally. The
            // percentage is recomputed from `used / total` so we get two
            // decimal places of precision (the `usage_pct: u8` field on
            // `ContextInfo` is pre-rounded to an integer).
            Line::from(Span::styled(
                format!(
                    "{} / {} tokens ({:.2}%)",
                    fmt_tok_big(used),
                    fmt_tok_big(total),
                    facts.usage_pct,
                ),
                Style::default().fg(theme.text_secondary),
            )),
            // Model name (one step dimmer than the tokens line so it reads
            // as a supporting caption rather than the primary number).
            Line::from(Span::styled(
                model.to_string(),
                Style::default().fg(theme.gray_bright),
            )),
            // Blank space before the bar
            Line::from(""),
        ];
        lines.extend(bar_lines);
        lines.push(Line::from(""));
        for row in &legend_rows {
            lines.extend(layout.render(row, bar, total, label_style, muted));
        }
        // Say what the remainder is made of rather than letting a one-word
        // label imply the client knows. Only when the row is actually there:
        // a footnote to an absent row is noise.
        if facts
            .contributors
            .iter()
            .any(|c| c.kind == ContributorKind::Unattributed)
        {
            lines.push(Line::from(""));
            lines.extend(
                UNATTRIBUTED_NOTE
                    .iter()
                    .map(|l| Line::from(Span::styled(*l, muted))),
            );
        }
        if !info_rows.is_empty() {
            lines.push(Line::from(""));
            for row in &info_rows {
                lines.extend(layout.render(row, bar, total, label_style, muted));
            }
        }
        lines.push(Line::from(""));

        // Auto-compact estimate: where the trigger sits and how far off it is.
        // Both figures come from the resolved facts, which carry the threshold
        // the shell resolved for this model (remote settings / user TOML / env)
        // rather than a default assumed here.
        if total > 0 {
            let auto = facts.auto_compact;
            let threshold_percent = auto.threshold_percent;
            let (text, style) = if auto.imminent {
                (
                    format!("Auto-compact triggers next turn (at {threshold_percent}%)"),
                    Style::default().fg(quantize(theme.warning)),
                )
            } else {
                // Use `fmt_tok_big` (same as the header) so the remaining
                // count rolls over to `m` for wide context windows — a 4m
                // window at 60% reads `~1.0m tokens remaining`, not
                // `~1000k tokens remaining`.
                (
                    format!(
                        "Auto-compact at {threshold_percent}% \u{00b7} ~{} tokens remaining",
                        fmt_tok_big(auto.remaining_tokens)
                    ),
                    muted,
                )
            };
            lines.push(Line::from(Span::styled(text, style)));
            lines.push(Line::from(""));
        }

        lines.extend(self.compaction_lines(theme, muted, label_style));

        // Footer stats
        lines.push(Line::from(Span::styled(
            format!(
                "Turns: {} \u{00b7} Tool calls: {} \u{00b7} Compactions: {}",
                facts.turn_count, facts.tool_call_count, facts.compaction.reported_count
            ),
            muted,
        )));

        // Approaching-auto-compact tip: only in the gap below the trigger.
        // Past it the "Auto-compact triggers next turn" line already says so
        // in warning style, and a second warning-styled tip advising a manual
        // `/compact` would contradict it.
        if facts.auto_compact.approaching {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Tip: run /compact to free up context space.".to_string(),
                Style::default().fg(quantize(theme.warning)),
            )));
        }

        lines
    }

    /// The compaction section: a totals line, then one row per recorded
    /// compaction. Absent entirely for a session that has never compacted.
    ///
    /// The section reports the shell's count and this client's recorded events
    /// as two separate things. They diverge on a session resumed without a
    /// full replay, and letting the row count stand in for the total would
    /// under-report what compaction has actually done to the conversation.
    fn compaction_lines(
        &self,
        theme: &Theme,
        muted: Style,
        label_style: Style,
    ) -> Vec<Line<'static>> {
        let c = &self.facts.compaction;
        if c.is_empty() {
            return Vec::new();
        }

        let mut lines = vec![Line::from(Span::styled(
            "Compaction",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        ))];

        let mut summary = vec![count_noun(c.reported_count, "compaction")];
        if c.recovered_tokens > 0 {
            let mut recovered = format!("{} tokens recovered", fmt_tok_big(c.recovered_tokens));
            if c.records_without_recovery > 0 {
                // The total omits these, so say so rather than letting it read
                // as the whole story.
                recovered.push_str(&format!(
                    " (excludes {})",
                    count_noun(c.records_without_recovery as u64, "event")
                ));
            }
            summary.push(recovered);
        }
        if c.elapsed_ms > 0 {
            summary.push(format!("{:.1}s spent", c.elapsed_ms as f64 / 1000.0));
        }
        lines.push(Line::from(Span::styled(summary.join(" \u{00b7} "), muted)));

        let ordinal_width = c
            .records
            .iter()
            .map(|r| r.ordinal.to_string().chars().count())
            .max()
            .unwrap_or(1);
        for record in &c.records {
            lines.push(Line::from(vec![
                Span::styled(format!(" {:>ordinal_width$} ", record.ordinal), label_style),
                Span::styled(compaction_row(record), muted),
            ]));
        }

        if c.undetailed() > 0 {
            lines.push(Line::from(Span::styled(
                format!(
                    "{} ran before this session was opened",
                    count_noun(c.undetailed(), "compaction")
                ),
                muted,
            )));
        }

        lines.push(Line::from(""));
        lines
    }
}

/// One compaction's numbers: what it collapsed, what it gave back, how long
/// it took. Fields the shell did not report are omitted, never guessed.
fn compaction_row(record: &CompactionRecord) -> String {
    let mut row = match record.tokens_before {
        Some(before) => format!(
            "{} \u{2192} {} tokens",
            fmt_tok_big(before),
            fmt_tok_big(record.tokens_after)
        ),
        None => format!("\u{2192} {} tokens", fmt_tok_big(record.tokens_after)),
    };
    if let Some(recovered) = record.recovered() {
        row.push_str(&format!("  \u{00b7}  {} recovered", fmt_tok_big(recovered)));
    }
    if let Some(ms) = record.elapsed_ms {
        row.push_str(&format!("  ({:.1}s)", ms as f64 / 1000.0));
    }
    row
}

/// `"1 compaction"` / `"2 compactions"` — the plural rule the summary and the
/// undetailed note share.
fn count_noun(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Format a token count compactly (`123`, `1.2k`, `999k`).
///
/// The cutover from `{:.1}k` to plain `{}k` happens at 99_500 (not 100_000)
/// to avoid a precision discontinuity: `{:.1}k` rounds `99.999` (n=99_999)
/// up to `"100.0k"` (6 chars), and the next bucket then emits `"100k"` (4
/// chars). The mismatch makes the value visually identical but two
/// characters wider, which knocks the column-aligned `tw` width in
/// `build_lines` off by one. Switching to integer-rounded `Nk` at 99_500
/// keeps the output stable: 99_499 → `"99.5k"`, 99_500 → `"100k"`.
fn fmt_tok(n: u64) -> String {
    if n >= 99_500 {
        // Round half-up to the nearest 1k; equivalent to `(n + 500) / 1000`
        // for u64, which avoids the f64 rounding artifact described above.
        format!("{}k", (n + 500) / 1000)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{n}")
    }
}

// Every shipped bar shape must hold exactly the partition the resolver
// produces, or the legend percentages would describe a different bar than the
// one drawn. A new shape with a different cell count fails the build here.
const _: () = {
    assert!(BarLayout::WIDE.total() == BarPartition::CELLS);
    assert!(BarLayout::NARROW.total() == BarPartition::CELLS);
};

/// Like [`fmt_tok`] but rolls over to `1.0m` at one million.
///
/// Used for the at-a-glance totals line so a 1M / 2M / 4M context window
/// reads naturally as `1.0m` rather than `1000k`. Per-category legend rows
/// stay on [`fmt_tok`] so a fractional-million breakdown still shows the
/// finer-grained `k` resolution.
fn fmt_tok_big(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    } else {
        fmt_tok(n)
    }
}

/// Format a category's share of the total context window as a percentage.
fn percent_of_window(part: u64, total: u64) -> String {
    if total == 0 {
        return "-".to_string();
    }
    // Tiny nonzero shares floor at 0.1% so the column stays clean.
    let p = ((part as f64 / total as f64) * 100.0).max(if part > 0 { 0.1 } else { 0.0 });
    if p < 10.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

impl BlockContent for ContextInfoBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        // Responsive bar shape: narrow terminals get a 10×10 bar so the
        // visualization doesn't get clipped or pushed under the legend.
        let bar = BarLayout::for_width(ctx.width);
        let styled_lines = self.build_lines(&theme, bar);
        let wrapped = word_wrap_lines(styled_lines, ctx.width as usize);
        let all_lines: Vec<BlockLine> = wrapped
            .into_iter()
            .map(|line| BlockLine::styled(line).with_selection_range(Some(0)))
            .collect();

        // Apply max_lines budget if set
        let lines = if let Some(max) = ctx.max_lines {
            let max = max as usize;
            if all_lines.len() > max && max > 0 {
                let take_count = if max > 1 { max - 1 } else { 1 };
                let mut truncated: Vec<BlockLine> =
                    all_lines.into_iter().take(take_count).collect();
                if let Some(last) = truncated.last_mut() {
                    let content_end = last.content.spans.len();
                    last.content
                        .spans
                        .push(Span::styled(" \u{2026}".to_string(), theme.muted()));
                    last.selectable = crate::scrollback::types::Selectable::Spans(0..content_end);
                }
                truncated
            } else {
                all_lines
            }
        } else {
            all_lines
        };

        if lines.is_empty() {
            BlockOutput {
                lines: vec![BlockLine::styled(Line::from("")).with_selection_range(Some(0))],
            }
        } else {
            BlockOutput { lines }
        }
    }

    fn accent(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        None
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false // Compact like SystemMessageBlock
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        false
    }

    fn is_selectable(&self) -> bool {
        false
    }

    fn is_groupable(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_shell::session::TokenUsageCategory;

    pub(super) fn snapshot() -> ContextInfo {
        ContextInfo {
            used: 36_700,
            total: 1_000_000,
            system_prompt_tokens: 1_200,
            tool_definitions_count: 12,
            tool_definitions_tokens: 5_600,
            compaction_count: 0,
            turn_count: 5,
            tool_call_count: 12,
            message_count: 8,
            message_tokens: 29_900,
            free_tokens: 963_300,
            usage_pct: 4,
            auto_compact_threshold_percent: 85,
            usage_categories: vec![],
        }
    }

    /// Theme handle used by the unit tests. `Theme::current()` is the same
    /// resolver `output()` calls; the active theme doesn't matter for these
    /// tests since they assert on text content / span counts, not colors.
    pub(super) fn test_theme() -> Theme {
        Theme::current()
    }

    /// Render a block and collapse a single line's spans into a flat string.
    fn line_text(lines: &[Line<'static>], idx: usize) -> String {
        lines[idx]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// Render a block and collapse all spans into a flat newline-joined
    /// string, useful for `contains` assertions.
    pub(super) fn all_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()).chain(["\n"]))
            .collect()
    }

    fn record(ordinal: usize, before: Option<u64>, after: u64) -> CompactionRecord {
        CompactionRecord {
            ordinal,
            tokens_before: before,
            tokens_after: after,
            elapsed_ms: Some(500),
            summary_preview: None,
        }
    }

    // ── compaction section ─────────────────────────────────────────────

    #[test]
    fn compaction_section_is_absent_until_something_compacts() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let lines = block.build_lines(&test_theme(), BarLayout::WIDE);
        assert!(
            !all_text(&lines).contains("Compaction\n"),
            "a session that never compacted gets no section"
        );
    }

    #[test]
    fn compaction_section_lists_each_recorded_event() {
        let mut snap = snapshot();
        snap.compaction_count = 2;
        let history = [
            record(1, Some(858_000), 43_000),
            record(2, Some(900_000), 60_000),
        ];
        let block = ContextInfoBlock::new(snap, &history, "grok-4");
        let all = all_text(&block.build_lines(&test_theme(), BarLayout::WIDE));

        assert!(all.contains("2 compactions"), "{all}");
        assert!(all.contains("1.7m tokens recovered"), "{all}");
        assert!(all.contains("1.0s spent"), "{all}");
        // Per-event rows carry before → after, what came back, and how long.
        assert!(all.contains("858k \u{2192} 43.0k tokens"), "{all}");
        assert!(all.contains("815k recovered"), "{all}");
        assert!(all.contains("900k \u{2192} 60.0k tokens"), "{all}");
        assert!(all.contains("(0.5s)"), "{all}");
    }

    #[test]
    fn compaction_row_omits_what_the_shell_did_not_report() {
        let mut snap = snapshot();
        snap.compaction_count = 1;
        let history = [CompactionRecord {
            ordinal: 1,
            tokens_before: None,
            tokens_after: 20_000,
            elapsed_ms: None,
            summary_preview: None,
        }];
        let block = ContextInfoBlock::new(snap, &history, "grok-4");
        let all = all_text(&block.build_lines(&test_theme(), BarLayout::WIDE));
        assert!(all.contains("\u{2192} 20.0k tokens"), "{all}");
        assert!(
            !all.contains("recovered"),
            "no before count means no recovery figure to show: {all}"
        );
        assert!(!all.contains("0.0s)"), "no timing means no timing: {all}");
    }

    #[test]
    fn compaction_section_says_when_the_client_missed_events() {
        // A resumed session: the shell counted five, this client saw two.
        let mut snap = snapshot();
        snap.compaction_count = 5;
        let history = [
            record(1, Some(858_000), 43_000),
            record(2, Some(900_000), 60_000),
        ];
        let block = ContextInfoBlock::new(snap, &history, "grok-4");
        let all = all_text(&block.build_lines(&test_theme(), BarLayout::WIDE));
        assert!(
            all.contains("5 compactions"),
            "the summary reports the shell's count, not the row count: {all}"
        );
        assert!(
            all.contains("3 compactions ran before this session was opened"),
            "{all}"
        );
    }

    #[test]
    fn compaction_summary_flags_a_recovery_total_that_is_short() {
        let mut snap = snapshot();
        snap.compaction_count = 2;
        let history = [record(1, Some(858_000), 43_000), record(2, None, 60_000)];
        let block = ContextInfoBlock::new(snap, &history, "grok-4");
        let all = all_text(&block.build_lines(&test_theme(), BarLayout::WIDE));
        assert!(all.contains("815k tokens recovered"), "{all}");
        assert!(
            all.contains("(excludes 1 event)"),
            "the total must not read as covering both events: {all}"
        );
    }

    #[test]
    fn footer_compaction_count_is_the_shell_figure() {
        let mut snap = snapshot();
        snap.compaction_count = 5;
        let block = ContextInfoBlock::new(snap, &[record(1, Some(90_000), 20_000)], "grok-4");
        let all = all_text(&block.build_lines(&test_theme(), BarLayout::WIDE));
        assert!(all.contains("Compactions: 5"), "{all}");
    }

    #[test]
    fn build_lines_contains_header_tokens_and_model() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        // Layout: Context / <blank> / tokens / model.
        assert_eq!(line_text(&lines, 0), "Context");
        assert_eq!(line_text(&lines, 1), "");
        let l2 = line_text(&lines, 2);
        assert!(l2.contains("tokens"));
        // Percent now shows 2 decimal places (36.7k / 1m = 3.67%).
        assert!(l2.contains("(3.67%)"), "got: {l2:?}");
        assert_eq!(line_text(&lines, 3), "grok-4");
    }

    #[test]
    fn build_lines_contains_tokens_summary() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let l2 = line_text(&lines, 2);
        assert!(l2.contains("tokens"));
        assert!(l2.contains("(3.67%)"));
    }

    #[test]
    fn build_lines_includes_compaction_tip_in_warning_band() {
        // 80..85 is the band where the tip appears: auto-compact is close
        // enough to mention but not so close that the "triggers next turn"
        // line is also showing.
        let mut snap = snapshot();
        snap.usage_pct = 80;
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let last = line_text(&lines, lines.len() - 1);
        assert!(last.contains("/compact"), "expected tip line, got {last:?}");
    }

    #[test]
    fn build_lines_omits_tip_below_threshold() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        assert!(!all_text(&lines).contains("/compact"));
    }

    #[test]
    fn build_lines_omits_tip_at_or_above_threshold() {
        // At/above the auto-compact threshold the "triggers next turn" line
        // already says everything the tip would; the tip is suppressed to
        // avoid stacking two warning-styled lines that contradict each
        // other (manual /compact vs. auto-compact about to fire).
        let mut snap = snapshot();
        snap.usage_pct = 85; // the historical default (and value in snapshot() helper)
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        assert!(!all_text(&lines).contains("/compact"));
    }

    #[test]
    fn build_lines_shows_auto_compact_estimate_below_threshold() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("Auto-compact at 85%") && all.contains("tokens remaining"),
            "expected `Auto-compact at 85% · ~X tokens remaining` line, got:\n{all}"
        );
    }

    #[test]
    fn build_lines_auto_compact_eta_uses_millions_for_wide_windows() {
        // 4M window at 0% used: remaining = ceil(4_000_000 * 85 / 100) = 3_400_000.
        // Should render via fmt_tok_big as "3.4m", not "3400k".
        let mut snap = snapshot();
        snap.total = 4_000_000;
        snap.used = 0;
        snap.usage_pct = 0;
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("~3.4m tokens remaining"),
            "expected ETA to use millions, got:\n{all}"
        );
    }

    #[test]
    fn build_lines_auto_compact_eta_arithmetic_at_known_snapshot() {
        // 1M window, 36_700 used: ceil(850_000) - 36_700 = 813_300 → "813k".
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("~813k tokens remaining"),
            "expected `~813k tokens remaining`, got:\n{all}"
        );
    }

    #[test]
    fn fmt_tok_big_switches_to_millions_at_one_million() {
        assert_eq!(fmt_tok_big(999_999), fmt_tok(999_999)); // delegates below 1m
        assert_eq!(fmt_tok_big(1_000_000), "1.0m");
        assert_eq!(fmt_tok_big(1_500_000), "1.5m");
        assert_eq!(fmt_tok_big(2_345_678), "2.3m");
        assert_eq!(fmt_tok_big(10_000_000), "10.0m");
    }

    #[test]
    fn fmt_tok_boundaries() {
        assert_eq!(fmt_tok(0), "0");
        assert_eq!(fmt_tok(999), "999");
        assert_eq!(fmt_tok(1_000), "1.0k");
        // 99_499 is the last value below the `{:.1}k` cutover; it formats
        // with one decimal. 99_500 crosses into the integer-rounded branch
        // and emits `100k` (4 chars) rather than `{:.1}k`'s round-up of
        // `99.5` to `100.0k` (6 chars). See `fmt_tok` doc comment.
        assert_eq!(fmt_tok(99_499), "99.5k");
        assert_eq!(fmt_tok(99_500), "100k");
        assert_eq!(fmt_tok(99_999), "100k");
        assert_eq!(fmt_tok(100_000), "100k");
        assert_eq!(fmt_tok(999_999), "1000k");
    }

    #[test]
    fn build_lines_summary_uses_millions_for_total() {
        let mut snap = snapshot();
        snap.total = 2_000_000;
        snap.used = 36_700;
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let l2 = line_text(&lines, 2);
        assert!(
            l2.contains("/ 2.0m tokens"),
            "expected `/ 2.0m tokens` in summary line, got {l2:?}"
        );
    }

    #[test]
    fn build_lines_shows_imminent_auto_compact_at_threshold() {
        let mut snap = snapshot();
        snap.usage_pct = 85;
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("Auto-compact triggers next turn"),
            "expected `Auto-compact triggers next turn` line, got:\n{all}"
        );
    }

    // -------------------------------------------------------------------
    // Bar partition tests
    //
    // The bar lives at line indices 5..(5+layout.rows) (after header /
    // blank / tokens / model / blank). Each row is rendered as `glyph`
    // spans separated by raw-space spans. To count cells per category,
    // we walk the bar lines for the given layout and count spans whose
    // content matches each category's glyph.
    // -------------------------------------------------------------------

    const SYSTEM_GLYPH_TEST: &str = "\u{25C6}";
    const TOOLS_GLYPH_TEST: &str = "\u{25C8}";
    const MESSAGES_GLYPH_TEST: &str = "\u{25C6}"; // same as system; distinguished by color in real render
    const FREE_GLYPH_TEST: &str = "\u{25C7}";

    /// `layout` tells the function how many bar rows to slice (5 for
    /// WIDE, 10 for NARROW); without it the slice would be wrong for
    /// the narrow layout and the assertions would fail spuriously.
    fn count_bar_glyphs(
        lines: &[Line<'static>],
        layout: BarLayout,
    ) -> (usize, usize, usize, usize) {
        let mut diamonds = 0usize;
        let mut dotted = 0usize;
        let mut free = 0usize;
        let bar_start = 5; // header / blank / tokens / model / blank
        let bar_end = bar_start + layout.rows;
        for line in &lines[bar_start..bar_end] {
            for span in &line.spans {
                let c = span.content.as_ref();
                if c == SYSTEM_GLYPH_TEST || c == MESSAGES_GLYPH_TEST {
                    // Every band that partitions `used` draws the filled
                    // diamond and is separated only by color, so the bar-cell
                    // counts here are of the used band as a whole. The
                    // per-band split is asserted on `facts.bar` instead.
                    diamonds += 1;
                } else if c == TOOLS_GLYPH_TEST {
                    // ◈ — reserved for rows that do not partition the window,
                    // so finding one inside the bar is itself the failure.
                    dotted += 1;
                } else if c == FREE_GLYPH_TEST {
                    free += 1;
                }
            }
        }
        (diamonds, dotted, free, diamonds + dotted + free)
    }

    #[test]
    fn bar_used_band_does_not_overshoot_when_estimates_exceed_used() {
        let mut snap = snapshot();
        snap.total = 100_000;
        snap.used = 10_000;
        snap.system_prompt_tokens = 8_000;
        snap.message_tokens = 5_000;
        snap.tool_definitions_tokens = 0;
        snap.free_tokens = 90_000;
        snap.usage_pct = 10;
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (diamonds, tools, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(tools, 0);
        assert_eq!(
            diamonds, 10,
            "used band must equal cells_for(used)=10 even when estimates (13k) exceed used (10k)"
        );
        assert_eq!(free, 90);
    }

    #[test]
    fn bar_total_cells_always_sum_to_one_hundred() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (_, _, _, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100, "bar must always render exactly 100 cells");
    }

    #[test]
    fn bar_all_free_when_total_is_zero() {
        let mut snap = snapshot();
        snap.total = 0;
        snap.used = 0;
        snap.system_prompt_tokens = 0;
        snap.tool_definitions_tokens = 0;
        snap.message_tokens = 0;
        snap.free_tokens = 0;
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (diamonds, tools, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(diamonds, 0, "no diamonds when total=0");
        assert_eq!(tools, 0, "no tools when total=0");
        assert_eq!(free, 100, "all cells free when total=0");
    }

    #[test]
    fn bar_all_used_when_completely_full() {
        // Fill the bar entirely with messages so we can count by glyph
        // (system+messages share the diamond; tools and free are distinct).
        let mut snap = snapshot();
        snap.total = 1_000;
        snap.used = 1_000;
        snap.system_prompt_tokens = 0;
        snap.tool_definitions_tokens = 0;
        snap.message_tokens = 1_000;
        snap.free_tokens = 0;
        snap.usage_pct = 100;
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (diamonds, tools, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(diamonds, 100, "messages should fill the entire bar");
        assert_eq!(tools, 0);
        assert_eq!(free, 0);
    }

    #[test]
    fn tool_schemas_take_their_own_band_without_overshooting() {
        let mut snap = snapshot();
        snap.total = 1_000;
        snap.used = 1_000;
        snap.system_prompt_tokens = 495;
        snap.message_tokens = 10;
        snap.tool_definitions_tokens = 800;
        snap.free_tokens = 0;
        snap.usage_pct = 100;
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (diamonds, dotted, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100, "bar must always render exactly 100 cells");
        assert_eq!(dotted, 0, "informational rows must never enter the bar");
        // The measured parts claim 1_305 tokens of a 1_000-token `used`; the
        // used band still fills exactly the bar and the remainder absorbs it.
        assert_eq!(diamonds, 100, "used band fills the bar at 100% usage");
        assert_eq!(free, 0, "no free cells at 100% usage");
        assert_eq!(
            block.facts.bar.tools, 49,
            "tools clamped into the used band"
        );
        assert_eq!(block.facts.bar.unattributed, 0);
    }

    #[test]
    fn tool_schemas_are_a_legend_row_and_shrink_the_remainder() {
        // A real grok-build shape: a wide toolset that used to disappear into
        // an "overhead" row larger than the toolset itself.
        let snap = ContextInfo {
            used: 100_000,
            total: 500_000,
            system_prompt_tokens: 5_000,
            tool_definitions_count: 190,
            tool_definitions_tokens: 75_000,
            compaction_count: 0,
            turn_count: 1,
            tool_call_count: 0,
            message_count: 4,
            message_tokens: 25_000,
            free_tokens: 400_000,
            usage_pct: 20,
            auto_compact_threshold_percent: 65,
            usage_categories: vec![],
        };
        let block = ContextInfoBlock::new(snap, &[], "grok-build");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);

        let all = all_text(&lines);
        assert!(
            !all.contains("Reasoning/overhead"),
            "the everything-else bucket must be gone:\n{all}"
        );
        assert!(
            !all.contains("Unattributed = "),
            "nothing is left over here, so the note must not print:\n{all}"
        );
        assert!(
            all.contains("Tool schemas") && all.contains("75.0k"),
            "tool schemas must be a legend row carrying their measured size:\n{all}"
        );
        assert!(
            all.contains("190 tools"),
            "the tool count is the actionable half of the row:\n{all}"
        );

        let (diamonds, dotted, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(dotted, 0, "informational rows excluded from the bar");
        assert_eq!(diamonds, 20, "used band must equal used/total");
        assert_eq!(free, 80);
        // 75k of the 100k used is tool schemas; the band is clamped to the 14
        // cells the measured rows above it leave inside the 20-cell used band.
        assert_eq!(block.facts.bar.tools, 14);
    }

    #[test]
    fn the_remainder_prints_what_it_is_made_of() {
        let mut snap = snapshot();
        snap.used = 40_000; // 3.3k more than the measured parts account for
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let all = all_text(&block.build_lines(&test_theme(), BarLayout::WIDE));
        assert!(
            all.contains("Unattributed"),
            "remainder row missing:\n{all}"
        );
        assert!(
            all.contains("bytes/4 estimate"),
            "a one-word label must not be left implying the client knows:\n{all}"
        );
    }

    #[test]
    fn every_note_line_fits_the_narrowest_layout() {
        // The usage modal does not wrap; a line wider than the narrow bar's
        // breakpoint would be clipped there rather than reflowed.
        for line in UNATTRIBUTED_NOTE {
            assert!(
                line.chars().count() < BarLayout::NARROW_BREAKPOINT as usize,
                "note line too wide to survive the modal: {line:?}"
            );
        }
    }

    #[test]
    fn build_lines_renders_usage_categories_with_details() {
        let mut snap = snapshot();
        snap.usage_categories = vec![
            TokenUsageCategory::skills_listing(&"x".repeat(9_600), 21),
            TokenUsageCategory::mcp_servers(&"y".repeat(1_200), 4),
        ];
        let block = ContextInfoBlock::new(snap, &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("Skills") && all.contains("21 skills"),
            "skills row missing:\n{all}"
        );
        assert!(
            all.contains("MCP servers") && all.contains("4 servers"),
            "mcp row missing:\n{all}"
        );
        assert!(all.contains("\u{00b7} 12 tools"), "tools count:\n{all}");
        let (_, tools, _, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(tools, 0, "usage categories must never enter the bar");

        // Token, percent, and count columns line up across all rows;
        // single-digit counts are right-aligned ("·  4 servers").
        let is_row = |l: &&str| {
            (l.starts_with('\u{25C6}') || l.starts_with('\u{25C8}') || l.starts_with('\u{25C7}'))
                && l.contains(" tokens ")
        };
        let cols = |needle: &str| -> Vec<usize> {
            all.lines()
                .filter(is_row)
                .filter_map(|l| l.find(needle))
                .collect()
        };
        for needle in [" tokens ", ")"] {
            let positions = cols(needle);
            assert!(
                positions.windows(2).all(|w| w[0] == w[1]),
                "{needle:?} column misaligned: {positions:?}\n{all}"
            );
        }
        assert!(
            all.contains("\u{00b7}  4 servers"),
            "single-digit count must be right-aligned:\n{all}"
        );
    }

    #[test]
    fn percent_of_window_returns_dash_for_zero_total() {
        // Exercised the `total == 0` early return — pct can't be computed.
        assert_eq!(percent_of_window(100, 0), "-");
        assert_eq!(percent_of_window(0, 0), "-");
    }

    #[test]
    fn percent_of_window_formatting() {
        assert_eq!(percent_of_window(1, 1_000_000), "0.1%");
        assert_eq!(percent_of_window(0, 1_000_000), "0.0%");
        assert_eq!(percent_of_window(50_000, 1_000_000), "5.0%");
        assert_eq!(percent_of_window(500_000, 1_000_000), "50%");
    }

    // -------------------------------------------------------------------
    // Responsive bar layout tests
    //
    // The bar's shape (5×20 vs 10×10) is chosen by `BarLayout::for_width`
    // based on terminal width, so narrow terminals get a square bar that
    // still fits in their column budget.
    // -------------------------------------------------------------------

    #[test]
    fn bar_layout_wide_is_5_rows_of_20() {
        assert_eq!(BarLayout::WIDE.rows, 5);
        assert_eq!(BarLayout::WIDE.row_len, 20);
        assert_eq!(BarLayout::WIDE.total(), 100);
    }

    #[test]
    fn bar_layout_narrow_is_10_rows_of_10() {
        assert_eq!(BarLayout::NARROW.rows, 10);
        assert_eq!(BarLayout::NARROW.row_len, 10);
        assert_eq!(BarLayout::NARROW.total(), 100);
    }

    #[test]
    fn bar_layout_for_width_picks_wide_at_breakpoint_and_above() {
        // At the breakpoint and above, the wide layout is selected.
        assert_eq!(
            BarLayout::for_width(BarLayout::NARROW_BREAKPOINT),
            BarLayout::WIDE
        );
        assert_eq!(BarLayout::for_width(80), BarLayout::WIDE);
        assert_eq!(BarLayout::for_width(200), BarLayout::WIDE);
        assert_eq!(BarLayout::for_width(u16::MAX), BarLayout::WIDE);
    }

    #[test]
    fn bar_layout_for_width_picks_narrow_below_breakpoint() {
        // Below the breakpoint, the narrow layout is selected so the bar
        // fits without being clipped.
        assert_eq!(
            BarLayout::for_width(BarLayout::NARROW_BREAKPOINT - 1),
            BarLayout::NARROW
        );
        assert_eq!(BarLayout::for_width(40), BarLayout::NARROW);
        assert_eq!(BarLayout::for_width(20), BarLayout::NARROW);
        assert_eq!(BarLayout::for_width(0), BarLayout::NARROW);
    }

    #[test]
    fn narrow_bar_renders_10_rows() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        // The bar starts at index 5 (header / blank / tokens / model /
        // blank). Each of the next 10 rows must be a non-empty bar row.
        for (offset, line) in lines[5..15].iter().enumerate() {
            assert!(
                !line.spans.is_empty(),
                "narrow bar row {offset} must be non-empty"
            );
        }
        // The line right after the bar should be the spacer blank.
        let after_bar: String = lines[15].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(after_bar, "", "expected blank line after narrow bar");
    }

    #[test]
    fn narrow_bar_total_cells_still_100() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        let (_, _, _, total) = count_bar_glyphs(&lines, BarLayout::NARROW);
        assert_eq!(total, 100, "narrow bar must still render exactly 100 cells");
    }

    #[test]
    fn narrow_bar_each_row_has_at_most_10_cells() {
        // Sanity: no single bar row should exceed the narrow row_len.
        // We count cell glyphs (not separator spaces) per row.
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        for (offset, line) in lines[5..15].iter().enumerate() {
            let cell_count = line
                .spans
                .iter()
                .filter(|s| {
                    let c = s.content.as_ref();
                    c == SYSTEM_GLYPH_TEST
                        || c == TOOLS_GLYPH_TEST
                        || c == MESSAGES_GLYPH_TEST
                        || c == FREE_GLYPH_TEST
                })
                .count();
            assert!(
                cell_count <= BarLayout::NARROW.row_len,
                "row {offset} has {cell_count} cells, expected <= {}",
                BarLayout::NARROW.row_len
            );
        }
    }

    #[test]
    fn wide_bar_each_row_has_at_most_20_cells() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        for (offset, line) in lines[5..10].iter().enumerate() {
            let cell_count = line
                .spans
                .iter()
                .filter(|s| {
                    let c = s.content.as_ref();
                    c == SYSTEM_GLYPH_TEST
                        || c == TOOLS_GLYPH_TEST
                        || c == MESSAGES_GLYPH_TEST
                        || c == FREE_GLYPH_TEST
                })
                .count();
            assert!(
                cell_count <= BarLayout::WIDE.row_len,
                "row {offset} has {cell_count} cells, expected <= {}",
                BarLayout::WIDE.row_len
            );
        }
    }

    // -------------------------------------------------------------------
    // Legend label color + responsive wrapping tests
    // -------------------------------------------------------------------

    /// Helper: find the legend row (or row 1, for the narrow layout)
    /// whose label text starts with `label_prefix`. Returns the matching
    /// `Line` so the caller can assert on its spans.
    fn find_legend_line<'a>(
        lines: &'a [Line<'static>],
        label_prefix: &str,
    ) -> Option<&'a Line<'static>> {
        lines.iter().find(|line| {
            line.spans
                .iter()
                .any(|s| s.content.as_ref().trim_start().starts_with(label_prefix))
        })
    }

    #[test]
    fn legend_label_uses_secondary_color_wide() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let row = find_legend_line(&lines, "System prompt").expect("legend row");
        // Span layout for WIDE: [glyph+space, label, " ", tokens, ...].
        // The label span content begins with "System prompt".
        let label_span = row
            .spans
            .iter()
            .find(|s| s.content.as_ref().starts_with("System prompt"))
            .expect("label span");
        assert_eq!(
            label_span.style.fg,
            Some(theme.text_secondary),
            "label should use text_secondary, got {:?}",
            label_span.style.fg
        );
    }

    #[test]
    fn legend_label_uses_secondary_color_narrow() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        let row = find_legend_line(&lines, "System prompt").expect("legend row 1");
        // Span layout for NARROW row 1: [glyph+space, label].
        let label_span = row
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "System prompt")
            .expect("label span");
        assert_eq!(
            label_span.style.fg,
            Some(theme.text_secondary),
            "narrow label should use text_secondary, got {:?}",
            label_span.style.fg
        );
    }

    #[test]
    fn narrow_legend_wraps_to_two_lines_per_category() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        let categories = [
            ("System prompt", "1.2k"),
            ("Messages", "29.9k"),
            ("Tool schemas", "5.6k"),
            ("Free", "963k"),
        ];
        let mut idx = 16;
        for (label, expected_tokens) in categories {
            let row1: String = lines[idx]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            let row2: String = lines[idx + 1]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert!(
                row1.contains(label),
                "expected row {idx} to contain `{label}`, got: {row1:?}"
            );
            assert!(
                row2.contains(expected_tokens),
                "expected row {} to contain `{expected_tokens}` for `{label}`, got: {row2:?}",
                idx + 1
            );
            idx += 2;
        }
    }

    #[test]
    fn narrow_legend_data_row_starts_with_one_space_indent() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        // The data row for the first legend entry sits at index 17
        // (16 = "System prompt" header row, 17 = its data row).
        let data_row = &lines[17];
        let first = data_row
            .spans
            .first()
            .expect("data row should have a leading indent span");
        assert_eq!(
            first.content.as_ref(),
            " ",
            "narrow data row must start with exactly one space, got {:?}",
            first.content.as_ref()
        );
    }

    #[test]
    fn wide_legend_remains_single_line_per_category() {
        let block = ContextInfoBlock::new(snapshot(), &[], "grok-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let row_text =
            |i: usize| -> String { lines[i].spans.iter().map(|s| s.content.as_ref()).collect() };
        let l11 = row_text(11);
        assert!(
            l11.contains("System prompt") && l11.contains("1.2k"),
            "wide legend should keep label + tokens on one line, got: {l11:?}"
        );
        let l12 = row_text(12);
        assert!(
            l12.contains("Messages") && l12.contains("29.9k"),
            "wide legend should keep label + tokens on one line, got: {l12:?}"
        );
        let l13 = row_text(13);
        assert!(
            l13.contains("Tool schemas") && l13.contains("5.6k"),
            "wide legend should keep label + tokens on one line, got: {l13:?}"
        );
        let l14 = row_text(14);
        assert!(
            l14.contains("Free") && l14.contains("963k"),
            "wide legend should keep label + tokens on one line, got: {l14:?}"
        );
    }
}

#[cfg(test)]
mod injection_tests {
    use super::tests::*;
    use super::*;
    use xai_grok_shell::session::TokenUsageCategory;

    fn block_with(rows: Vec<TokenUsageCategory>) -> ContextInfoBlock {
        let mut snap = snapshot();
        snap.usage_categories = rows;
        ContextInfoBlock::new(snap, &[], "grok-4")
    }

    #[test]
    fn each_block_is_headed_by_its_size_and_followed_by_its_text() {
        let block = block_with(vec![TokenUsageCategory::project_instructions(
            "Always fix clippy warnings.",
            2,
        )]);
        let out = all_text(&block.injection_lines(&test_theme(), 80));
        assert!(out.contains("Project instructions"), "{out}");
        assert!(out.contains("2 files"), "{out}");
        assert!(
            out.contains("Always fix clippy warnings."),
            "the text is the point of the view:\n{out}"
        );
    }

    #[test]
    fn a_row_without_text_says_so_rather_than_rendering_empty() {
        // What an older shell sends: the size, but not what it measured.
        let block = block_with(vec![TokenUsageCategory {
            label: "Skills".to_string(),
            tokens: 2_400,
            detail: Some("21 skills".to_string()),
            text: None,
        }]);
        let out = all_text(&block.injection_lines(&test_theme(), 80));
        assert!(out.contains("Skills"), "{out}");
        assert!(
            out.contains("reports the size but not the text"),
            "an empty section would read as an empty block:\n{out}"
        );
    }

    #[test]
    fn a_session_with_no_injections_says_that_too() {
        let out = all_text(&block_with(vec![]).injection_lines(&test_theme(), 80));
        assert!(out.contains("injects nothing"), "{out}");
    }

    #[test]
    fn the_empty_case_is_wrapped_like_every_other_line() {
        // The sentence is wider than a narrow pane; unwrapped it would be
        // clipped by the modal into a half-sentence.
        let lines = block_with(vec![]).injection_lines(&test_theme(), 30);
        for line in &lines {
            let width: usize = line
                .spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>();
            assert!(width <= 30, "line overflows the content width: {line:?}");
        }
        assert!(all_text(&lines).contains("injects nothing"), "{lines:?}");
    }

    #[test]
    fn injected_text_is_wrapped_to_the_content_width() {
        // The usage modal renders one row per logical line and clips the rest,
        // so anything past the right edge would simply be lost.
        let long = "word ".repeat(60);
        let block = block_with(vec![TokenUsageCategory::session_start_hooks(&long)]);
        let lines = block.injection_lines(&test_theme(), 40);
        for line in &lines {
            let width: usize = line
                .spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>();
            assert!(width <= 40, "line overflows the content width: {line:?}");
        }
    }

    #[test]
    fn the_view_says_what_it_does_not_cover() {
        let block = block_with(vec![TokenUsageCategory::memory_index("- an entry\n", 1)]);
        let out = all_text(&block.injection_lines(&test_theme(), 80));
        assert!(
            out.contains("Not listed: the system prompt and the tool schemas"),
            "a view of the injections must not read as a view of everything:\n{out}"
        );
    }
}
