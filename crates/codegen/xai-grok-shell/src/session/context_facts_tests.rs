//! Regression tests for the resolved `/context` facts.
//!
//! Two kinds live here. The arithmetic tests pin the derivation the agent
//! now owns on behalf of every client. The wire tests pin the compatibility
//! contract in both directions: a payload from a build that predates a
//! field still deserializes, and a payload from a newer one does not break
//! this build.

use super::*;
use crate::session::TokenUsageCategory;

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
fn tool_schemas_are_a_contributor_carrying_the_measured_count() {
    // Their bytes are counted from the definitions the turn serializes, so
    // the row reports a measurement rather than a share of a remainder.
    let facts = ContextFacts::resolve(&snapshot(), &[]);
    let tools = facts
        .contributors
        .iter()
        .find(|c| c.kind == ContributorKind::ToolSchemas)
        .expect("tool schema row");
    assert_eq!(tools.label, "Tool schemas");
    assert_eq!(tools.tokens, 5_600);
    assert_eq!(tools.detail.as_deref(), Some("12 tools"));
}

#[test]
fn tool_schema_row_is_absent_when_the_snapshot_carries_no_count() {
    // A partial snapshot leaves the breakdown fields at zero; an empty row
    // would read as "this agent has no tools".
    let mut snap = snapshot();
    snap.tool_definitions_tokens = 0;
    snap.tool_definitions_count = 0;
    let facts = ContextFacts::resolve(&snap, &[]);
    assert_eq!(facts.tokens_for(ContributorKind::ToolSchemas), None);
}

#[test]
fn unattributed_is_used_minus_every_measured_part() {
    let mut snap = snapshot();
    snap.used = 40_000;
    let facts = ContextFacts::resolve(&snap, &[]);
    // 40_000 - (1_200 + 29_900 + 5_600) = 3_300.
    assert_eq!(
        facts.tokens_for(ContributorKind::Unattributed),
        Some(3_300),
        "the remainder must shrink by the tool schemas it used to hide"
    );
}

#[test]
fn unattributed_row_is_absent_when_the_measured_parts_account_for_everything() {
    // The fixture's `used` is exactly system + messages + tool schemas.
    let facts = ContextFacts::resolve(&snapshot(), &[]);
    assert_eq!(facts.tokens_for(ContributorKind::Unattributed), None);
}

#[test]
fn unattributed_saturates_when_the_measured_parts_exceed_used() {
    // Before the first response `used` is itself a local estimate that does
    // not yet include the tool schemas, so the measured parts can outrun it.
    let mut snap = snapshot();
    snap.used = 10_000;
    snap.system_prompt_tokens = 8_000;
    snap.message_tokens = 5_000;
    let facts = ContextFacts::resolve(&snap, &[]);
    assert_eq!(facts.tokens_for(ContributorKind::Unattributed), None);
    assert_eq!(
        facts.tokens_for(ContributorKind::ToolSchemas),
        Some(5_600),
        "the measured row keeps its measurement; the remainder absorbs the clash"
    );
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
fn tool_schemas_do_not_also_appear_as_an_itemized_row() {
    // They partition `used` now; leaving the old informational row in place
    // would show the same 5.6k twice under two different headings.
    let facts = ContextFacts::resolve(&snapshot(), &[]);
    assert!(facts.itemized.is_empty());
}

#[test]
fn shell_usage_categories_are_carried_verbatim() {
    let mut snap = snapshot();
    snap.usage_categories = vec![
        TokenUsageCategory::skills_listing(&"x".repeat(9_600), 21),
        TokenUsageCategory::mcp_servers(&"y".repeat(1_200), 4),
    ];
    let facts = ContextFacts::resolve(&snap, &[]);
    let labels: Vec<&str> = facts.itemized.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["Skills", "MCP servers"]);
    assert_eq!(facts.itemized[0].detail.as_deref(), Some("21 skills"));
    assert_eq!(facts.itemized[1].detail.as_deref(), Some("4 servers"));
}

// ── bar partition ──────────────────────────────────────────────────

#[test]
fn bar_always_sums_to_one_hundred_units() {
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
            BarPartition::UNITS,
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
    // system+messages+tools claim 13.6% of a window that is 10% used; the
    // used band stays at 10 cells and the bands are clamped into it, in
    // legend order, until nothing is left for the remainder.
    assert_eq!(bar.used(), 10);
    assert_eq!(bar.system, 8);
    assert_eq!(bar.messages, 2);
    assert_eq!(bar.tools, 0);
    assert_eq!(bar.unattributed, 0);
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
            free: BarPartition::UNITS,
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

// ── the wire contract ──────────────────────────────────────────────────

#[test]
fn facts_survive_a_round_trip_through_json() {
    // The clients render the deserialized form, so a field that serializes
    // but does not come back is a field they silently lose.
    let mut snap = snapshot();
    snap.compaction_count = 1;
    snap.usage_categories = vec![TokenUsageCategory::skills_listing("skills", 3)];
    let facts = ContextFacts::resolve(&snap, &[record(1, Some(858_000), 43_000)]);
    let json = serde_json::to_string(&facts).expect("serialize");
    let back: ContextFacts = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(facts, back);
}

#[test]
fn field_names_on_the_wire_are_camel_case() {
    // A second client reads these names directly; they are the contract, not
    // an artifact of the Rust spelling.
    let value = serde_json::to_value(ContextFacts::resolve(&snapshot(), &[])).expect("serialize");
    let object = value.as_object().expect("object");
    for key in ["usagePct", "autoCompact", "turnCount", "toolCallCount"] {
        assert!(object.contains_key(key), "missing {key} in {object:?}");
    }
    assert!(
        object["autoCompact"]
            .as_object()
            .expect("object")
            .contains_key("thresholdPercent")
    );
}

#[test]
fn a_payload_from_a_build_that_predates_every_field_still_deserializes() {
    // The absence of the facts is carried by the enclosing Option; an empty
    // object has to mean "nothing resolved", not a parse error.
    let facts: ContextFacts = serde_json::from_str("{}").expect("deserialize");
    assert_eq!(facts, ContextFacts::default());
    assert!(facts.compaction.is_empty());
}

#[test]
fn a_compaction_record_from_an_agent_that_reports_no_before_count_deserializes() {
    let record: CompactionRecord =
        serde_json::from_str(r#"{"ordinal":1,"tokensAfter":43000}"#).expect("deserialize");
    assert_eq!(record.tokens_before, None);
    assert_eq!(record.recovered(), None);
}

#[test]
fn a_contributor_kind_this_build_has_no_name_for_degrades_to_unknown() {
    // A newer agent adding a measured band must not stop an older client from
    // rendering the rest of the panel.
    let row: Contributor = serde_json::from_str(
        r#"{"kind":"attachments","label":"Attachments","tokens":900,"stride":2}"#,
    )
    .expect("deserialize");
    assert_eq!(row.kind, ContributorKind::Unknown);
    assert_eq!(row.tokens, 900, "the row's numbers still arrive");
}

#[test]
fn unknown_fields_from_a_newer_agent_are_ignored() {
    let facts: ContextFacts =
        serde_json::from_str(r#"{"used":10,"total":100,"cacheHits":7}"#).expect("deserialize");
    assert_eq!(facts.used, 10);
    assert_eq!(facts.total, 100);
}

#[test]
fn session_info_from_an_agent_that_resolves_no_facts_reads_as_absent() {
    // The fallback path in a Rust client hangs off this being `None` rather
    // than an empty struct: `Default` facts would render as a zero window.
    let response: crate::session::SessionInfoResponse = serde_json::from_str(
        r#"{"sessionId":"s","cwd":"/tmp","model":null,"resolvedModelId":null,
            "modelFingerprint":null,"turns":0,"context":{"used":5,"total":10}}"#,
    )
    .expect("deserialize");
    assert!(response.data.context_facts.is_none());
    assert_eq!(response.data.context.used, 5);
}

#[test]
fn session_info_carries_the_facts_when_the_agent_resolved_them() {
    let mut data = crate::session::SessionInfoData::default();
    data.context = snapshot();
    data.context_facts = Some(ContextFacts::resolve(&data.context, &[]));
    let json = serde_json::to_string(&data).expect("serialize");
    assert!(json.contains("contextFacts"), "{json}");
    let back: crate::session::SessionInfoData = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.context_facts, data.context_facts);
}
