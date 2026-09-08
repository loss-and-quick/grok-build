// `/context` in a browser: the agent's numbers, this client's chrome.
//
// Every figure the widget shows arrives resolved on `x.ai/session/info` as
// `contextFacts`, and the wire type says exactly why: the derivation "holds
// decisions (what the unlabelled remainder means, where the advisory band
// before auto-compaction starts, how a partition that overruns `used` is
// squeezed) that have no other expression on the wire". So **nothing here
// derives anything**. There is no percentage recomputed from `used`, no band
// re-clamped, no threshold compared against a number written down on this side.
//
// What is here is the half the wire type hands back: "a client picks glyphs or
// colors or CSS for these structures and draws them", and the number
// formatting with it. Those formatters are ports of the pager's own — the same
// rounding, the same cut-over points, the same floor on a tiny share — because
// two clients printing the same window in different words is the divergence the
// resolver was moved out of the pager to prevent, and it would be silly to
// re-open it one decimal place lower.
//
// The bar is the piece a browser gets for free. `BarPartition` is a hundred
// units "so that one unit reads as one percent: a terminal spends them as cells
// and a browser as percent", which is why {@link barBands} hands the units
// straight to CSS and no grid is chosen here. The 5×20 / 10×10 grid the pager
// picks by terminal width *is* terminal geometry, and it stays there.
import type {
  AutoCompact,
  BarPartition,
  CompactionFacts,
  CompactionRecord,
  ContextFacts,
  ContributorKind,
} from "./wire.ts";
import type { ThemeRole } from "./theme.ts";
import { BULLET, DIAMOND_HOLLOW, GROUP_DIAMOND } from "./glyphs.ts";

/**
 * Format a token count compactly: `123`, `1.2k`, `99.5k`, `100k`.
 *
 * The pager's `fmt_tok`, cut-over included. It switches from `{:.1}k` to a
 * whole `Nk` at 99_500 rather than 100_000 because `99_999` would otherwise
 * round to `"100.0k"` and the next value print `"100k"` — the same magnitude
 * two characters wider. A terminal notices because the column alignment moves;
 * this client keeps the rule anyway, because the point is that the two clients
 * print one number one way.
 */
export function formatTokens(n: number): string {
  if (n >= 99_500) return `${Math.floor((n + 500) / 1000)}k`;
  if (n >= 1_000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/**
 * Like {@link formatTokens} but rolls over to `1.0m` at a million.
 *
 * The pager's `fmt_tok_big`, used for the same rows it uses it for: the
 * at-a-glance totals, the auto-compact remainder and the compaction records,
 * so a 1m/2m/4m window reads as `1.0m` rather than `1000k`. Per-row legend
 * figures stay on `formatTokens`, where the finer `k` resolution is what makes
 * a fractional-million breakdown readable.
 */
export function formatTokensBig(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}m`;
  return formatTokens(n);
}

/**
 * A row's share of the whole window, as the pager prints it.
 *
 * `percent_of_window`: a `-` when there is no window, a floor of `0.1%` on a
 * tiny non-zero share so the column never reads `0.0%` for something that is
 * there, one decimal below ten percent and none above it.
 *
 * This is arithmetic over two numbers that both arrived on the wire, not a
 * second opinion about any of them: `Contributor::share_pct` computes exactly
 * this on the Rust side and is not serialized.
 */
export function percentOfWindow(part: number, total: number): string {
  if (total <= 0) return "-";
  const raw = (part / total) * 100;
  const p = Math.max(raw, part > 0 ? 0.1 : 0);
  return p < 10 ? `${p.toFixed(1)}%` : `${p.toFixed(0)}%`;
}

/**
 * The headline: what is used, of what, and at what precision.
 *
 * `usagePct` is read, never recomputed. The snapshot carries a pre-rounded
 * integer percentage as well, and the resolver keeps the two apart on purpose
 * — the rounded one is what the agent compares against the auto-compact
 * threshold, and this one is what a person reads — so taking the wrong one
 * here would put a number on screen that disagrees with the band beside it.
 */
export function headline(facts: ContextFacts): string {
  return `${formatTokensBig(facts.used)} / ${formatTokensBig(facts.total)} tokens (${facts.usagePct.toFixed(2)}%)`;
}

/** The share, short enough for a collapsed section header. */
export function usageChip(facts: ContextFacts): string {
  return percentOfWindow(facts.used, facts.total);
}

/**
 * The bar, band by band, in draw order.
 *
 * The units are the wire's and are emitted as percentages verbatim. They
 * already sum to a hundred and they are already clamped: the resolver squeezes
 * the *unattributed* band when the independently measured ones overrun `used`,
 * "since the remainder is the one band that has no measurement of its own to
 * defend". A client that re-normalised here would be re-deciding that in
 * silence, and would disagree with the legend printed under it.
 *
 * Zero-width bands are dropped rather than emitted at `width: 0%`: an element
 * with no width still takes a gap, and a bar with gaps in it is a bar that has
 * stopped summing to the window.
 */
export function barBands(bar: BarPartition): { kind: ContributorKind; units: number }[] {
  return (
    [
      { kind: "systemPrompt", units: bar.system },
      { kind: "messages", units: bar.messages },
      { kind: "toolSchemas", units: bar.tools },
      { kind: "unattributed", units: bar.unattributed },
      { kind: "free", units: bar.free },
    ] as const
  )
    .filter((band) => band.units > 0)
    .map((band) => ({ kind: band.kind as ContributorKind, units: band.units }));
}

/**
 * The colour of a contributor, as a theme role.
 *
 * The pager's own mapping (`context_info.rs`, `chrome`), which it keeps beside
 * its renderer for the reason the wire type gives: the resolver never names a
 * colour, so the naming belongs to whatever is drawing. Messages take the
 * brightest treatment because they are the conversation a person is actually
 * steering; the system prompt shares their glyph in gray so it reads as a quiet
 * base layer under them.
 */
export const CONTRIBUTOR_ROLE: Record<ContributorKind, ThemeRole> = {
  systemPrompt: "gray_bright",
  messages: "text_primary",
  toolSchemas: "accent_skill",
  unattributed: "accent_verify",
  free: "gray_dim",
  itemized: "accent_skill",
  // A kind this build has no name for, from a newer agent. Drawn as
  // informational, which is what the wire type says an unknown kind degrades
  // to — and the safe direction, since informational rows never enter the bar.
  unknown: "accent_skill",
};

/**
 * The glyph of a contributor.
 *
 * `◆` for a band that partitions the window, `◇` for the free remainder, `◈`
 * for a row that does not partition anything. The pager says the glyph *is* the
 * distinction — "◆ adds up to the window, ◈ does not" — so it carries the same
 * meaning here rather than being decoration.
 */
export function contributorGlyph(kind: ContributorKind): string {
  if (kind === "free") return DIAMOND_HOLLOW;
  if (kind === "itemized" || kind === "unknown") return GROUP_DIAMOND;
  return BULLET;
}

/** The theme role for a kind, tolerating one this build has no name for. */
export function contributorRole(kind: ContributorKind): ThemeRole {
  return CONTRIBUTOR_ROLE[kind] ?? CONTRIBUTOR_ROLE.unknown;
}

/**
 * What the unattributed row is made of.
 *
 * The pager prints this under the legend whenever the row is there, and the
 * sentence is its, word for word (`UNATTRIBUTED_NOTE`) — as one paragraph,
 * because it is pre-split into four short lines there for a modal that
 * deliberately does not wrap, and a browser wraps.
 *
 * It is printed at all because the label alone would imply the client knows
 * what it holds. It does not: the wire type says the row is "deliberately not
 * named after a mechanism" and holds at least three things nothing can
 * separate.
 */
export const UNATTRIBUTED_NOTE =
  "Unattributed = used minus what can be measured here: reasoning, per-request " +
  "scaffolding, and the drift between this client's bytes/4 estimate and the " +
  "provider's tokenizer.";

/**
 * Where the window stands against the auto-compact trigger.
 *
 * Both branches are the agent's flags, not a comparison made here. `imminent`
 * means the threshold is reached and compaction runs on the next turn;
 * otherwise the trigger is a distance away and the remaining tokens are the
 * agent's own figure.
 */
export function autoCompactLine(auto: AutoCompact): string {
  return auto.imminent
    ? `Auto-compact triggers next turn (at ${auto.thresholdPercent}%)`
    : `Auto-compact at ${auto.thresholdPercent}% · ~${formatTokensBig(auto.remainingTokens)} tokens remaining`;
}

/**
 * The advisory tip, or nothing.
 *
 * `approaching` is the 80% band, and it is on the wire precisely because it is
 * "a policy number with no other expression" — a client comparing `usagePct`
 * against an 80 of its own would be a second policy that drifts from the first.
 * It is never true at the same time as `imminent`, so the tip cannot contradict
 * the line above it by advising a manual compaction while an automatic one is
 * already due.
 */
export function compactTip(auto: AutoCompact): string | null {
  return auto.approaching ? "Tip: run /compact to free up context space." : null;
}

/** `"1 compaction"` / `"2 compactions"` — the pager's `count_noun`. */
export function countNoun(n: number, noun: string): string {
  return n === 1 ? `${n} ${noun}` : `${n} ${noun}s`;
}

/**
 * The compaction totals line.
 *
 * The count is the agent's, and it is not `records.length`: a session resumed
 * without a full replay counts compactions whose records could not be
 * recovered, and the wire keeps the two apart so a client cannot let the second
 * pass for the first. Where the recovered total omits records, it says so.
 */
export function compactionSummary(compaction: CompactionFacts): string {
  const parts = [countNoun(compaction.reportedCount, "compaction")];
  if (compaction.recoveredTokens > 0) {
    let recovered = `${formatTokensBig(compaction.recoveredTokens)} tokens recovered`;
    if (compaction.recordsWithoutRecovery > 0) {
      recovered += ` (excludes ${countNoun(compaction.recordsWithoutRecovery, "event")})`;
    }
    parts.push(recovered);
  }
  if (compaction.elapsedMs > 0) parts.push(`${(compaction.elapsedMs / 1000).toFixed(1)}s spent`);
  return parts.join(" · ");
}

/**
 * One compaction's numbers.
 *
 * A field the agent did not report is left out rather than guessed — an older
 * agent sends no before count, and `→ 43.0k tokens` is the honest rendering of
 * that, not `0 → 43.0k`.
 */
export function compactionRow(record: CompactionRecord): string {
  const before = record.tokensBefore ?? null;
  let row =
    before === null
      ? `→ ${formatTokensBig(record.tokensAfter)} tokens`
      : `${formatTokensBig(before)} → ${formatTokensBig(record.tokensAfter)} tokens`;
  if (before !== null) {
    // Saturating, as `CompactionRecord::recovered` has it: a compaction that
    // somehow ended larger reports nothing recovered rather than a negative.
    row += `  ·  ${formatTokensBig(Math.max(before - record.tokensAfter, 0))} recovered`;
  }
  if (record.elapsedMs !== null && record.elapsedMs !== undefined) {
    row += `  (${(record.elapsedMs / 1000).toFixed(1)}s)`;
  }
  return row;
}

/**
 * Compactions counted with no record behind them, if any.
 *
 * `CompactionFacts::undetailed`. Non-zero when the session's log no longer
 * holds one — a relocated or truncated transcript, or an agent too old to have
 * written it — and saying so is what stops `records.length` passing itself off
 * as the total.
 */
export function undetailedCompactions(compaction: CompactionFacts): number {
  return Math.max(compaction.reportedCount - (compaction.records?.length ?? 0), 0);
}

/** Whether the compaction section has anything to say. */
export function hasCompaction(compaction: CompactionFacts): boolean {
  return compaction.reportedCount > 0 || (compaction.records?.length ?? 0) > 0;
}

/** The footer: the counters the agent keeps for this session. */
export function footerLine(facts: ContextFacts): string {
  return `Turns: ${facts.turnCount} · Tool calls: ${facts.toolCallCount} · Compactions: ${facts.compaction.reportedCount}`;
}
