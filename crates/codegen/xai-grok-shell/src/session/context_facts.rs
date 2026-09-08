//! The resolved `/context` picture, as a wire type.
//!
//! ## Why this is on the wire
//!
//! Every number `/context` shows is derived from one snapshot the agent
//! already sends ([`ContextInfo`]) plus the compactions this session has run.
//! The derivation, though, was a pure function compiled into the pager: which
//! rows exist, what they are called, what the unlabelled remainder means, how
//! the window divides into a drawable partition, and where the advisory band
//! before auto-compaction starts. None of that is reachable by a client that is
//! not the pager, so a second client could only reimplement it — and a
//! reimplementation of a rounding rule or of an advisory threshold is a
//! divergence waiting to happen, not a port.
//!
//! So the agent resolves it once and every client renders the result. There are
//! no host types here and no behaviour beyond arithmetic: a client picks glyphs
//! or colors or CSS for these structures and draws them.
//!
//! ## What stays with the client
//!
//! The shape a client gives the partition. [`BarPartition`] is a hundred
//! units, which a terminal spends as cells over a width-dependent grid and a
//! browser spends as percent. Choosing that grid, the glyph per
//! [`ContributorKind`], the color, the number formatting and the wrapping is
//! rendering, and none of it belongs here.
//!
//! ## Compatibility
//!
//! Every field carries `#[serde(default)]`, so an agent that predates a field
//! deserializes as its zero rather than failing, and a client that predates one
//! ignores it. `SessionInfoData::context_facts` is itself optional for the same
//! reason: against an agent too old to resolve them, a Rust client calls
//! [`ContextFacts::resolve`] here on the [`ContextInfo`] it did get, which is
//! the same code the agent would have run.

use serde::{Deserialize, Serialize};

use super::acp_types::ContextInfo;

/// Which part of the window a contributor accounts for.
///
/// A client maps this to a glyph and a color; the resolver never names one, so
/// the facts stay independent of what draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContributorKind {
    /// The system prompt.
    SystemPrompt,
    /// Conversation items — user, assistant and tool responses.
    Messages,
    /// The tool definitions sent with every request.
    ///
    /// Measured, not inferred: the agent serializes the exact definition list
    /// the turn sends and counts its bytes. This is the one part of the former
    /// overhead bucket a user can act on, by changing the agent's toolset.
    ToolSchemas,
    /// What is left of `used` once the measured parts are subtracted.
    ///
    /// Deliberately not named after a mechanism. It holds at least three
    /// things nothing here can separate: reasoning tokens the provider billed
    /// but did not itemize, per-request scaffolding, and the drift between the
    /// bytes/4 estimate and the provider's tokenizer. Splitting it further
    /// would need per-part counts nothing measures, so it stays a labelled
    /// remainder rather than a guess.
    Unattributed,
    /// Unused capacity.
    Free,
    /// A row that itemizes tokens already counted inside another contributor.
    Itemized,
    /// A kind this build has no name for, sent by a newer agent.
    ///
    /// Itemized is what it degrades to, and that is the safe direction:
    /// itemized rows are informational and never enter the partition, so a row
    /// this client cannot classify is shown rather than counted twice.
    #[serde(other)]
    #[default]
    Unknown,
}

/// One row of the breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Contributor {
    pub kind: ContributorKind,
    pub label: String,
    pub tokens: u64,
    /// Count-then-noun detail, e.g. `"12 tools"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// For an injected block, the text `tokens` was measured over, so a client
    /// can show what the tokens bought and not only how many. `None` for rows
    /// measured without a text of their own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl Contributor {
    /// This row's share of the whole window, or `None` when there is no window
    /// to take a share of.
    pub fn share_pct(&self, total: u64) -> Option<f64> {
        (total > 0).then(|| xai_token_estimation::usage_percentage(self.tokens, total))
    }
}

/// The window split into one hundred units, in draw order.
///
/// Resolved here rather than in a client because it is arithmetic over the
/// snapshot, and because the rounding is load-bearing: the bands are clamped so
/// they always sum to exactly [`BarPartition::UNITS`], even when the
/// per-category estimates add up to more than `used` (they are independent
/// estimates and routinely do).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BarPartition {
    pub system: usize,
    pub messages: usize,
    pub tools: usize,
    pub unattributed: usize,
    pub free: usize,
}

impl BarPartition {
    /// Total units. A hundred, so that one unit reads as one percent: a
    /// terminal spends them as cells and a browser as percent, and both agree
    /// with a legend that prints percentages of the same window.
    pub const UNITS: usize = 100;

    fn resolve(used: u64, total: u64, system: u64, messages: u64, tools: u64) -> Self {
        if total == 0 {
            return Self {
                free: Self::UNITS,
                ..Self::default()
            };
        }
        let units_for = |tokens: u64| -> usize {
            ((tokens as f64 / total as f64) * Self::UNITS as f64).round() as usize
        };
        // `used` is the authority for how much of the bar is filled; the
        // per-category figures only decide how that band is divided. Without
        // the clamps a system+messages+tools figure exceeding `used` would push
        // the bands past the used band and overflow the hundred units.
        //
        // Clamping in legend order means the measured bands keep their true
        // width and the unattributed remainder is what gets squeezed — which is
        // the right way round, since the remainder is the one band that has no
        // measurement of its own to defend.
        let used_units = units_for(used).min(Self::UNITS);
        let system = units_for(system).min(used_units);
        let messages = units_for(messages).min(used_units - system);
        let tools = units_for(tools).min(used_units - system - messages);
        Self {
            system,
            messages,
            tools,
            unattributed: used_units - system - messages - tools,
            free: Self::UNITS - used_units,
        }
    }

    /// Units standing for consumed capacity.
    pub fn used(&self) -> usize {
        self.system + self.messages + self.tools + self.unattributed
    }
}

/// Where the window stands relative to the auto-compact trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AutoCompact {
    /// The resolved trigger percent for the active model, as the agent
    /// resolved it — not a client-side default.
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
    ///
    /// It is a policy number with no other expression on the wire, which is
    /// why it is resolved here instead of being left for each client to
    /// hardcode and then disagree about.
    const APPROACHING_PERCENT: u8 = 80;

    fn resolve(snapshot: &ContextInfo) -> Self {
        let threshold_percent = snapshot.auto_compact_threshold_percent;
        // Both comparisons use the agent's rounded `usage_pct`, not the
        // precise share, so the band reported is the same one the agent will
        // actually trigger on.
        let imminent = snapshot.usage_pct >= threshold_percent;
        // `div_ceil`, not truncating division, so this agrees with the rounded
        // `usage_pct`: truncating leaves tiny windows reporting zero remaining
        // while `usage_pct` is still under the threshold.
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

/// One completed compaction.
///
/// Every field is a value the agent reported for that compaction — nothing is
/// re-derived from a second token count, so the record can never disagree with
/// the `Context compacted: … → …` line a client drew when it happened. The
/// number of conversation items collapsed is deliberately absent: it is not
/// measured, and estimating it would be a fabricated number.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CompactionRecord {
    /// 1-based position in the session — the first compaction is 1. Ordinal
    /// rather than wall-clock because the records are read back from the
    /// session's own log, where "now" says nothing about when they ran.
    pub ordinal: usize,
    /// Tokens in the window before compaction. `None` from an agent too old to
    /// report it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_before: Option<u64>,
    /// Tokens in the window after compaction.
    pub tokens_after: u64,
    /// How long compaction took, when it was timed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<i64>,
    /// Preview of the summary that replaced the collapsed history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_preview: Option<String>,
}

impl CompactionRecord {
    /// Tokens this compaction gave back, or `None` when no before count was
    /// reported. Saturating: a compaction that somehow ended larger reports
    /// zero recovered rather than wrapping.
    pub fn recovered(&self) -> Option<u64> {
        self.tokens_before
            .map(|before| before.saturating_sub(self.tokens_after))
    }
}

/// What compaction has done to this session.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CompactionFacts {
    /// Compactions counted for this session.
    pub reported_count: u64,
    /// The compactions there is detail for, oldest first. Shorter than
    /// `reported_count` when a record could not be recovered.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<CompactionRecord>,
    /// Tokens recovered across the records that carry a before count.
    pub recovered_tokens: u64,
    /// Records that carry no before count, so their recovery is unknown and is
    /// missing from `recovered_tokens`.
    pub records_without_recovery: usize,
    /// Time spent compacting across the records that were timed.
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

    /// Compactions counted but with no detail behind them.
    ///
    /// Non-zero when the session's log no longer holds a record — a relocated
    /// or truncated transcript, or an agent too old to have written one. A
    /// client says so rather than letting `records.len()` pass itself off as
    /// the total.
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
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextFacts {
    pub used: u64,
    pub total: u64,
    /// Share of the window in use, at full precision. The snapshot's own
    /// `usage_pct` is a pre-rounded `u8` and is kept for the threshold
    /// comparison only, so the two can never be mistaken for each other.
    pub usage_pct: f64,
    /// Rows that partition the window: the contributors to `used`, then the
    /// free remainder. The tool-schema and unattributed rows are present only
    /// when non-zero.
    pub contributors: Vec<Contributor>,
    /// Rows itemizing tokens already counted inside `contributors` — whatever
    /// the agent itemized in `usage_categories`. Adding these to
    /// `contributors` would double-count.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub itemized: Vec<Contributor>,
    pub bar: BarPartition,
    pub auto_compact: AutoCompact,
    pub turn_count: u64,
    pub tool_call_count: u64,
    pub compaction: CompactionFacts,
}

impl ContextFacts {
    /// Resolve from a context snapshot and the session's compaction history.
    ///
    /// Pure: no I/O and no clock, so the same inputs always give the same
    /// facts. The agent calls it when answering `x.ai/session/info`; a Rust
    /// client calls it only as the fallback for an agent that answered
    /// without them.
    pub fn resolve(snapshot: &ContextInfo, history: &[CompactionRecord]) -> Self {
        let used = snapshot.used;
        let total = snapshot.total;
        let system = snapshot.system_prompt_tokens;
        let messages = snapshot.message_tokens;
        let tools = snapshot.tool_definitions_tokens;
        // Saturating because these are independently measured over the same
        // request and can together exceed the server's `used` — most obviously
        // before the first response, when `used` is itself a local estimate
        // that has not yet been replaced by a provider count that includes the
        // tool schemas.
        let unattributed =
            used.saturating_sub(system.saturating_add(messages).saturating_add(tools));

        let mut contributors = vec![
            Contributor {
                kind: ContributorKind::SystemPrompt,
                label: "System prompt".to_string(),
                tokens: system,
                detail: None,
                text: None,
            },
            Contributor {
                kind: ContributorKind::Messages,
                label: "Messages".to_string(),
                tokens: messages,
                detail: None,
                text: None,
            },
        ];
        // Tool schemas are a contributor, not an informational row: they are a
        // measured slice of `used` in their own right, and the only reason they
        // used to sit below the bar is that the bucket above them was called
        // "overhead" and swallowed them whole.
        if tools > 0 {
            contributors.push(Contributor {
                kind: ContributorKind::ToolSchemas,
                label: "Tool schemas".to_string(),
                tokens: tools,
                detail: Some(super::acp_types::count_detail(
                    snapshot.tool_definitions_count,
                    "tool",
                )),
                text: None,
            });
        }
        if unattributed > 0 {
            contributors.push(Contributor {
                kind: ContributorKind::Unattributed,
                label: "Unattributed".to_string(),
                tokens: unattributed,
                detail: None,
                text: None,
            });
        }
        contributors.push(Contributor {
            kind: ContributorKind::Free,
            label: "Free".to_string(),
            tokens: snapshot.free_tokens,
            detail: None,
            text: None,
        });

        let itemized = snapshot
            .usage_categories
            .iter()
            .map(|c| Contributor {
                kind: ContributorKind::Itemized,
                label: c.label.clone(),
                tokens: c.tokens,
                detail: c.detail.clone(),
                text: c.text.clone(),
            })
            .collect();

        Self {
            used,
            total,
            usage_pct: xai_token_estimation::usage_percentage(used, total),
            contributors,
            itemized,
            bar: BarPartition::resolve(used, total, system, messages, tools),
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
#[path = "context_facts_tests.rs"]
mod tests;
