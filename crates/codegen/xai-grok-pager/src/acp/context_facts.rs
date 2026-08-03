//! Resolved context-window and compaction facts for the `/context` view.
//!
//! Every number here is derived once, off the render path, from two inputs the
//! pager already holds: the shell's [`ContextInfo`] snapshot (the same one
//! feeding the status-bar context bar) and the session's compaction history.
//! No second token count is estimated locally — if a figure is not in one of
//! those two inputs it is absent rather than invented, so the panel cannot
//! disagree with the bar or with the `Context compacted: … → …` lines.

use crate::acp::tracker::CompactionRecord;
use xai_grok_shell::session::ContextInfo;

/// Which part of the window a contributor accounts for.
///
/// The view maps this to a glyph and a color; the resolver never names one, so
/// the facts stay independent of what draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContributorKind {
    /// The system prompt.
    SystemPrompt,
    /// Conversation items — user, assistant and tool responses.
    Messages,
    /// The part of `used` that neither the system prompt nor the conversation
    /// accounts for: tool schemas, reasoning, and any per-request scaffolding.
    Overhead,
    /// Unused capacity.
    Free,
    /// A row that itemizes tokens already counted inside another contributor.
    Itemized,
}

/// One row of the breakdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contributor {
    pub kind: ContributorKind,
    pub label: String,
    pub tokens: u64,
    /// Count-then-noun detail from the shell, e.g. `"12 tools"`.
    pub detail: Option<String>,
}

impl Contributor {
    /// This row's share of the whole window, or `None` when there is no window
    /// to take a share of.
    pub fn share_pct(&self, total: u64) -> Option<f64> {
        (total > 0).then(|| xai_token_estimation::usage_percentage(self.tokens, total))
    }
}

/// The window split into one hundred cells, in draw order.
///
/// Resolved here rather than in the block because it is arithmetic over the
/// snapshot, and because the rounding is load-bearing: the bands are clamped so
/// they always sum to exactly [`BarPartition::CELLS`], even when the per-category
/// estimates add up to more than `used` (they are independent estimates and
/// routinely do).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BarPartition {
    pub system: usize,
    pub messages: usize,
    pub overhead: usize,
    pub free: usize,
}

impl BarPartition {
    /// Total cells. The legend prints percentages of the same window, so this
    /// being 100 is what keeps a cell readable as one percent.
    pub const CELLS: usize = 100;

    fn resolve(used: u64, total: u64, system: u64, messages: u64) -> Self {
        if total == 0 {
            return Self {
                free: Self::CELLS,
                ..Self::default()
            };
        }
        let cells_for = |tokens: u64| -> usize {
            ((tokens as f64 / total as f64) * Self::CELLS as f64).round() as usize
        };
        // `used` is the authority for how much of the bar is filled; the
        // per-category estimates only decide how that band is divided. Without
        // the clamps a system+messages estimate exceeding `used` would push the
        // bands past the used band and overflow the hundred cells.
        let used_cells = cells_for(used).min(Self::CELLS);
        let system = cells_for(system).min(used_cells);
        let messages = cells_for(messages).min(used_cells - system);
        Self {
            system,
            messages,
            overhead: used_cells - system - messages,
            free: Self::CELLS - used_cells,
        }
    }

    /// Cells standing for consumed capacity.
    pub fn used(&self) -> usize {
        self.system + self.messages + self.overhead
    }
}

/// Where the window stands relative to the auto-compact trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoCompact {
    /// The resolved trigger percent for the active model, as the shell
    /// resolved it — not a local default.
    pub threshold_percent: u8,
    /// Tokens the trigger sits at.
    pub threshold_tokens: u64,
    /// Tokens left before the trigger, zero once it is reached.
    pub remaining_tokens: u64,
    /// The threshold has been reached, so compaction runs on the next turn.
    pub imminent: bool,
    /// Close enough to the trigger to be worth mentioning, but not there yet.
    /// Never true at the same time as `imminent`: past the trigger, advising a
    /// manual compaction contradicts the auto-compaction about to run anyway.
    pub approaching: bool,
}

impl AutoCompact {
    /// Where the advisory band starts. Below this the window is nobody's
    /// problem; above it the trigger is close enough to plan around.
    const APPROACHING_PERCENT: u8 = 80;

    fn resolve(snapshot: &ContextInfo) -> Self {
        let threshold_percent = snapshot.auto_compact_threshold_percent;
        // Both comparisons use the shell's rounded `usage_pct`, not the
        // precise share, so the band the panel reports is the same one the
        // shell will actually trigger on.
        let imminent = snapshot.usage_pct >= threshold_percent;
        // `div_ceil`, not truncating division, so this agrees with the rounded
        // `usage_pct` the shell sends: truncating leaves tiny windows reporting
        // zero remaining while `usage_pct` is still under the threshold.
        let threshold_tokens = snapshot
            .total
            .saturating_mul(threshold_percent as u64)
            .div_ceil(100);
        Self {
            threshold_percent,
            threshold_tokens,
            remaining_tokens: threshold_tokens.saturating_sub(snapshot.used),
            imminent,
            approaching: !imminent && snapshot.usage_pct >= Self::APPROACHING_PERCENT,
        }
    }
}

/// What compaction has done to this session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactionFacts {
    /// Compactions the shell counted for this session.
    pub reported_count: u64,
    /// The compactions the pager holds detail for, oldest first. Shorter than
    /// `reported_count` when compactions ran before this client attached.
    pub records: Vec<CompactionRecord>,
    /// Tokens recovered across the records that carry a before count.
    pub recovered_tokens: u64,
    /// Records that carry no before count, so their recovery is unknown and is
    /// missing from `recovered_tokens`.
    pub records_without_recovery: usize,
    /// Time spent compacting across the records the shell timed.
    pub elapsed_ms: i64,
}

impl CompactionFacts {
    fn resolve(reported_count: u64, records: &[CompactionRecord]) -> Self {
        Self {
            reported_count,
            records: records.to_vec(),
            recovered_tokens: records.iter().filter_map(|r| r.recovered()).sum(),
            records_without_recovery: records.iter().filter(|r| r.recovered().is_none()).count(),
            elapsed_ms: records.iter().filter_map(|r| r.elapsed_ms).sum(),
        }
    }

    /// Compactions the shell counted but this client has no detail for.
    ///
    /// Non-zero on a session resumed without a full replay. The view says so
    /// rather than letting `records.len()` pass itself off as the total.
    pub fn undetailed(&self) -> u64 {
        self.reported_count
            .saturating_sub(self.records.len() as u64)
    }

    /// Whether there is anything to show.
    pub fn is_empty(&self) -> bool {
        self.reported_count == 0 && self.records.is_empty()
    }
}

/// The resolved context and compaction picture for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextFacts {
    pub used: u64,
    pub total: u64,
    /// Share of the window in use, at full precision. The snapshot's own
    /// `usage_pct` is a pre-rounded `u8` and is kept for the threshold
    /// comparison only, so the two can never be mistaken for each other.
    pub usage_pct: f64,
    /// Rows that partition the window: the contributors to `used`, then the
    /// free remainder. Overhead is present only when it is non-zero.
    pub contributors: Vec<Contributor>,
    /// Rows itemizing tokens already counted inside `contributors` — tool
    /// definitions, plus whatever the shell itemized in `usage_categories`.
    /// Adding these to `contributors` would double-count.
    pub itemized: Vec<Contributor>,
    pub bar: BarPartition,
    pub auto_compact: AutoCompact,
    pub turn_count: u64,
    pub tool_call_count: u64,
    pub compaction: CompactionFacts,
}

impl ContextFacts {
    /// Resolve from the shell's snapshot and the session's compaction history.
    ///
    /// Pure: no I/O and no clock, so the same inputs always give the same
    /// panel. Call it when the view is built, not while drawing it.
    pub fn resolve(snapshot: &ContextInfo, history: &[CompactionRecord]) -> Self {
        let used = snapshot.used;
        let total = snapshot.total;
        let system = snapshot.system_prompt_tokens;
        let messages = snapshot.message_tokens;
        // Saturating because the two categories are independent estimates over
        // the same conversation and can together exceed the server's `used`.
        let overhead = used.saturating_sub(system.saturating_add(messages));

        let mut contributors = vec![
            Contributor {
                kind: ContributorKind::SystemPrompt,
                label: "System prompt".to_string(),
                tokens: system,
                detail: None,
            },
            Contributor {
                kind: ContributorKind::Messages,
                label: "Messages".to_string(),
                tokens: messages,
                detail: None,
            },
        ];
        if overhead > 0 {
            contributors.push(Contributor {
                kind: ContributorKind::Overhead,
                label: "Reasoning/overhead".to_string(),
                tokens: overhead,
                detail: None,
            });
        }
        contributors.push(Contributor {
            kind: ContributorKind::Free,
            label: "Free".to_string(),
            tokens: snapshot.free_tokens,
            detail: None,
        });

        let itemized = std::iter::once(Contributor {
            kind: ContributorKind::Itemized,
            label: "Tool definitions".to_string(),
            tokens: snapshot.tool_definitions_tokens,
            detail: Some(xai_grok_shell::session::count_detail(
                snapshot.tool_definitions_count,
                "tool",
            )),
        })
        .chain(snapshot.usage_categories.iter().map(|c| Contributor {
            kind: ContributorKind::Itemized,
            label: c.label.clone(),
            tokens: c.tokens,
            detail: c.detail.clone(),
        }))
        .collect();

        Self {
            used,
            total,
            usage_pct: xai_token_estimation::usage_percentage(used, total),
            contributors,
            itemized,
            bar: BarPartition::resolve(used, total, system, messages),
            auto_compact: AutoCompact::resolve(snapshot),
            turn_count: snapshot.turn_count,
            tool_call_count: snapshot.tool_call_count,
            compaction: CompactionFacts::resolve(snapshot.compaction_count, history),
        }
    }

    /// Tokens attributed to one contributor kind, if the row is present.
    pub fn tokens_for(&self, kind: ContributorKind) -> Option<u64> {
        self.contributors
            .iter()
            .find(|c| c.kind == kind)
            .map(|c| c.tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_shell::session::TokenUsageCategory;

    fn snapshot() -> ContextInfo {
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

    fn record(ordinal: usize, before: Option<u64>, after: u64) -> CompactionRecord {
        CompactionRecord {
            ordinal,
            tokens_before: before,
            tokens_after: after,
            elapsed_ms: Some(500),
            summary_preview: None,
        }
    }

    // ── window totals ──────────────────────────────────────────────────

    #[test]
    fn usage_pct_is_precise_not_the_snapshot_u8() {
        let facts = ContextFacts::resolve(&snapshot(), &[]);
        // The snapshot's own usage_pct rounds 3.67 to 4; the panel must not.
        assert_eq!(snapshot().usage_pct, 4);
        assert!((facts.usage_pct - 3.67).abs() < 1e-9, "{}", facts.usage_pct);
    }

    #[test]
    fn usage_pct_is_zero_for_an_unknown_window() {
        let mut snap = snapshot();
        snap.total = 0;
        assert_eq!(ContextFacts::resolve(&snap, &[]).usage_pct, 0.0);
    }

    // ── contributors ───────────────────────────────────────────────────

    #[test]
    fn overhead_is_used_minus_system_and_messages() {
        let facts = ContextFacts::resolve(&snapshot(), &[]);
        // 36_700 - (1_200 + 29_900) = 5_600.
        assert_eq!(facts.tokens_for(ContributorKind::Overhead), Some(5_600));
    }

    #[test]
    fn overhead_row_is_absent_when_the_categories_account_for_everything() {
        let mut snap = snapshot();
        snap.used = 31_100; // exactly system + messages
        let facts = ContextFacts::resolve(&snap, &[]);
        assert_eq!(facts.tokens_for(ContributorKind::Overhead), None);
    }

    #[test]
    fn overhead_saturates_when_estimates_exceed_used() {
        // The two categories are independent estimates over the same
        // conversation and can together exceed the server's `used`.
        let mut snap = snapshot();
        snap.used = 10_000;
        snap.system_prompt_tokens = 8_000;
        snap.message_tokens = 5_000;
        let facts = ContextFacts::resolve(&snap, &[]);
        assert_eq!(facts.tokens_for(ContributorKind::Overhead), None);
    }

    #[test]
    fn free_row_uses_the_snapshot_figure_not_a_local_subtraction() {
        let facts = ContextFacts::resolve(&snapshot(), &[]);
        assert_eq!(facts.tokens_for(ContributorKind::Free), Some(963_300));
    }

    #[test]
    fn contributor_share_is_of_the_whole_window() {
        let facts = ContextFacts::resolve(&snapshot(), &[]);
        let messages = facts
            .contributors
            .iter()
            .find(|c| c.kind == ContributorKind::Messages)
            .expect("messages row");
        // 29_900 / 1_000_000 = 2.99%.
        let share = messages.share_pct(facts.total).expect("window is known");
        assert!((share - 2.99).abs() < 1e-9, "{share}");
    }

    #[test]
    fn contributor_share_is_absent_without_a_window() {
        let mut snap = snapshot();
        snap.total = 0;
        let facts = ContextFacts::resolve(&snap, &[]);
        assert!(facts.contributors[0].share_pct(facts.total).is_none());
    }

    // ── itemized rows ──────────────────────────────────────────────────

    #[test]
    fn tool_definitions_are_itemized_not_a_contributor() {
        // Their tokens already sit inside overhead; a contributor row would
        // count them twice.
        let facts = ContextFacts::resolve(&snapshot(), &[]);
        assert!(
            facts
                .contributors
                .iter()
                .all(|c| c.label != "Tool definitions")
        );
        let tools = &facts.itemized[0];
        assert_eq!(tools.label, "Tool definitions");
        assert_eq!(tools.tokens, 5_600);
        assert_eq!(tools.detail.as_deref(), Some("12 tools"));
    }

    #[test]
    fn shell_usage_categories_follow_tool_definitions_verbatim() {
        let mut snap = snapshot();
        snap.usage_categories = vec![
            TokenUsageCategory::skills_listing(&"x".repeat(9_600), 21),
            TokenUsageCategory::mcp_servers(&"y".repeat(1_200), 4),
        ];
        let facts = ContextFacts::resolve(&snap, &[]);
        let labels: Vec<&str> = facts.itemized.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["Tool definitions", "Skills", "MCP servers"]);
        assert_eq!(facts.itemized[1].detail.as_deref(), Some("21 skills"));
        assert_eq!(facts.itemized[2].detail.as_deref(), Some("4 servers"));
    }

    // ── bar partition ──────────────────────────────────────────────────

    #[test]
    fn bar_always_sums_to_one_hundred_cells() {
        for snap in [
            snapshot(),
            ContextInfo {
                total: 0,
                free_tokens: 0,
                ..snapshot()
            },
            ContextInfo {
                used: 1_000_000,
                free_tokens: 0,
                ..snapshot()
            },
        ] {
            let bar = ContextFacts::resolve(&snap, &[]).bar;
            assert_eq!(
                bar.used() + bar.free,
                BarPartition::CELLS,
                "partition must be exact for {snap:?}"
            );
        }
    }

    #[test]
    fn bar_used_band_tracks_used_over_the_window() {
        let facts = ContextFacts::resolve(&snapshot(), &[]);
        // 36_700 / 1_000_000 rounds to 4 cells.
        assert_eq!(facts.bar.used(), 4);
        assert_eq!(facts.bar.free, 96);
    }

    #[test]
    fn bar_used_band_does_not_overshoot_when_estimates_exceed_used() {
        let mut snap = snapshot();
        snap.total = 100_000;
        snap.used = 10_000;
        snap.system_prompt_tokens = 8_000;
        snap.message_tokens = 5_000;
        snap.free_tokens = 90_000;
        let bar = ContextFacts::resolve(&snap, &[]).bar;
        // system+messages estimate 13% of a window that is 10% used; the used
        // band stays at 10 cells and the categories are clamped into it.
        assert_eq!(bar.used(), 10);
        assert_eq!(bar.system, 8);
        assert_eq!(bar.messages, 2);
        assert_eq!(bar.overhead, 0);
        assert_eq!(bar.free, 90);
    }

    #[test]
    fn bar_is_all_free_for_an_unknown_window() {
        let mut snap = snapshot();
        snap.total = 0;
        let bar = ContextFacts::resolve(&snap, &[]).bar;
        assert_eq!(
            bar,
            BarPartition {
                free: BarPartition::CELLS,
                ..BarPartition::default()
            }
        );
    }

    // ── auto-compact ───────────────────────────────────────────────────

    #[test]
    fn auto_compact_uses_the_threshold_the_shell_resolved() {
        let mut snap = snapshot();
        snap.auto_compact_threshold_percent = 65;
        let auto = ContextFacts::resolve(&snap, &[]).auto_compact;
        assert_eq!(auto.threshold_percent, 65);
        assert_eq!(auto.threshold_tokens, 650_000);
        assert_eq!(auto.remaining_tokens, 650_000 - 36_700);
        assert!(!auto.imminent);
    }

    #[test]
    fn auto_compact_threshold_rounds_up() {
        // Truncating division would report 0 remaining on a window that has
        // not reached the threshold.
        let mut snap = snapshot();
        snap.total = 999;
        snap.used = 0;
        snap.auto_compact_threshold_percent = 85;
        let auto = ContextFacts::resolve(&snap, &[]).auto_compact;
        assert_eq!(auto.threshold_tokens, 850); // 849.15 rounded up
        assert_eq!(auto.remaining_tokens, 850);
    }

    #[test]
    fn auto_compact_is_imminent_at_the_threshold() {
        let mut snap = snapshot();
        snap.usage_pct = 85;
        let auto = ContextFacts::resolve(&snap, &[]).auto_compact;
        assert!(auto.imminent);
        assert_eq!(auto.remaining_tokens, 850_000 - 36_700);
    }

    #[test]
    fn auto_compact_approaching_band_sits_below_the_threshold() {
        let mut snap = snapshot();
        for (pct, approaching) in [(79u8, false), (80, true), (84, true), (85, false)] {
            snap.usage_pct = pct;
            let auto = ContextFacts::resolve(&snap, &[]).auto_compact;
            assert_eq!(auto.approaching, approaching, "at {pct}%");
            assert!(
                !(auto.approaching && auto.imminent),
                "the two bands must never overlap, at {pct}%"
            );
        }
    }

    #[test]
    fn auto_compact_approaching_band_follows_a_lowered_threshold() {
        // With a trigger below the advisory floor there is no band at all —
        // the window is already past the trigger by the time it reaches 80%.
        let mut snap = snapshot();
        snap.auto_compact_threshold_percent = 65;
        snap.usage_pct = 80;
        let auto = ContextFacts::resolve(&snap, &[]).auto_compact;
        assert!(auto.imminent);
        assert!(!auto.approaching);
    }

    #[test]
    fn auto_compact_remaining_floors_at_zero_past_the_threshold() {
        let mut snap = snapshot();
        snap.used = 900_000;
        snap.usage_pct = 90;
        let auto = ContextFacts::resolve(&snap, &[]).auto_compact;
        assert!(auto.imminent);
        assert_eq!(auto.remaining_tokens, 0);
    }

    // ── compaction history ─────────────────────────────────────────────

    #[test]
    fn compaction_facts_are_empty_for_a_fresh_session() {
        let facts = ContextFacts::resolve(&snapshot(), &[]);
        assert!(facts.compaction.is_empty());
        assert_eq!(facts.compaction.undetailed(), 0);
    }

    #[test]
    fn compaction_totals_sum_the_recorded_events() {
        let mut snap = snapshot();
        snap.compaction_count = 2;
        let history = [
            record(1, Some(858_000), 43_000),
            record(2, Some(900_000), 60_000),
        ];
        let c = ContextFacts::resolve(&snap, &history).compaction;
        assert_eq!(c.reported_count, 2);
        assert_eq!(c.records.len(), 2);
        assert_eq!(c.recovered_tokens, 815_000 + 840_000);
        assert_eq!(c.elapsed_ms, 1_000);
        assert_eq!(c.records_without_recovery, 0);
        assert_eq!(c.undetailed(), 0);
    }

    #[test]
    fn compaction_recovery_skips_events_with_no_before_count() {
        let mut snap = snapshot();
        snap.compaction_count = 2;
        let history = [record(1, Some(858_000), 43_000), record(2, None, 60_000)];
        let c = ContextFacts::resolve(&snap, &history).compaction;
        assert_eq!(
            c.recovered_tokens, 815_000,
            "an event with no before count contributes nothing to the total"
        );
        assert_eq!(
            c.records_without_recovery, 1,
            "and the view is told the total is short by one event"
        );
    }

    #[test]
    fn compactions_the_client_missed_are_reported_as_undetailed() {
        // A session resumed without a full replay: the shell counted five,
        // this client only ever saw the last two.
        let mut snap = snapshot();
        snap.compaction_count = 5;
        let history = [
            record(1, Some(858_000), 43_000),
            record(2, Some(900_000), 60_000),
        ];
        let c = ContextFacts::resolve(&snap, &history).compaction;
        assert_eq!(c.reported_count, 5);
        assert_eq!(c.records.len(), 2);
        assert_eq!(
            c.undetailed(),
            3,
            "the count is the shell's; the panel must not pass 2 off as the total"
        );
    }

    #[test]
    fn undetailed_floors_at_zero_when_the_client_holds_more_than_reported() {
        // The shell's count and the client's history come from different
        // places; a partial snapshot can leave the count behind.
        let mut snap = snapshot();
        snap.compaction_count = 0;
        let history = [record(1, Some(858_000), 43_000)];
        let c = ContextFacts::resolve(&snap, &history).compaction;
        assert_eq!(c.undetailed(), 0);
        assert!(!c.is_empty(), "a recorded event still counts as history");
    }

    #[test]
    fn resolve_is_pure_over_its_inputs() {
        let history = [record(1, Some(858_000), 43_000)];
        let snap = snapshot();
        assert_eq!(
            ContextFacts::resolve(&snap, &history),
            ContextFacts::resolve(&snap, &history)
        );
    }
}
