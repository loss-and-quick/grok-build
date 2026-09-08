import { For, Show, type JSX } from "solid-js";

import {
  UNATTRIBUTED_NOTE,
  autoCompactLine,
  barBands,
  compactTip,
  compactionRow,
  compactionSummary,
  contributorGlyph,
  contributorRole,
  footerLine,
  formatTokens,
  hasCompaction,
  headline,
  percentOfWindow,
  undetailedCompactions,
} from "../context.ts";
import { cssVarName } from "../theme.ts";
import type { ContextFacts, Contributor } from "../wire.ts";

/**
 * `/context`, in the widget rail.
 *
 * This is the one thing the terminal has shown all along that a browser could
 * not, and the reason it could not was never the geometry: the whole picture
 * was a pure function compiled into the pager. It is on the wire now
 * (`SessionInfoData.contextFacts`), so what is left here is glyphs, colours and
 * CSS — every number below arrived resolved and none of it is recomputed.
 *
 * Three of those numbers are on the wire *because* they are decisions rather
 * than arithmetic, and each is rendered rather than re-derived:
 *
 *  - the unattributed remainder, which is a labelled row plus the note under
 *    the legend saying what it holds — because the label alone would imply this
 *    client knows, and it does not;
 *  - the advisory band before auto-compaction, read as `approaching` rather
 *    than compared against an 80 written down here;
 *  - the partition, whose bands are already clamped in legend order so that the
 *    remainder is what gets squeezed when the measured bands overrun `used`.
 *
 * What stays with the terminal is the bar's *shape*: 5×20 or 10×10, chosen by
 * terminal columns. A browser has no cells to spend, and the wire type says so
 * itself — the hundred units are "a terminal spends them as cells and a browser
 * as percent" — which is why the bar below is one strip of percentage widths
 * and no grid is chosen at all.
 */
export function ContextWidget(props: {
  facts: ContextFacts;
  /** The active model, from the same reply. Absent on a session that answered nothing. */
  model?: string | null;
  /**
   * Whether this is the **Open** dialog rather than the rail's column.
   *
   * The one thing the wide form adds is the injected text: the agent sends the
   * text each itemized row was measured over, and a 360px column is not where
   * it is read. The pager makes the same split — a compact tab, and a separate
   * "Injected context" view for the text.
   */
  full?: boolean;
  onRefresh?: () => void;
}): JSX.Element {
  const facts = (): ContextFacts => props.facts;
  const itemized = (): Contributor[] => facts().itemized ?? [];
  const hasUnattributed = (): boolean =>
    facts().contributors.some((row) => row.kind === "unattributed");

  return (
    <div class="context">
      <p class="context-headline">{headline(facts())}</p>
      <Show when={props.model}>{(model) => <p class="context-model">{model()}</p>}</Show>

      {/* Decorative, and marked as such: every band below is stated in words in
          the legend, so a reader who is handed both hears the window twice. */}
      <div class="context-bar" aria-hidden="true">
        <For each={barBands(facts().bar)}>
          {(band) => (
            <span
              class="context-band"
              style={{
                width: `${band.units}%`,
                background: `var(${cssVarName(contributorRole(band.kind))})`,
              }}
            />
          )}
        </For>
      </div>

      <ul class="context-legend">
        <For each={facts().contributors}>
          {(row) => <LegendRow row={row} total={facts().total} />}
        </For>
      </ul>

      <Show when={hasUnattributed()}>
        <p class="context-note">{UNATTRIBUTED_NOTE}</p>
      </Show>

      {/* Below the legend and never in the bar: these tokens are already
          counted inside one of the bands above — the injected blocks land in
          the first user turn, so they overlap Messages — and adding them would
          count them twice. The pager reserves a different glyph for exactly
          this distinction. */}
      <Show when={itemized().length > 0}>
        <ul class="context-legend context-itemized">
          <For each={itemized()}>{(row) => <LegendRow row={row} total={facts().total} />}</For>
        </ul>
      </Show>

      <Show when={props.full}>
        <Injected rows={itemized()} />
      </Show>

      <Show when={facts().total > 0}>
        <p class="context-auto" classList={{ imminent: facts().autoCompact.imminent }}>
          {autoCompactLine(facts().autoCompact)}
        </p>
      </Show>
      <Show when={compactTip(facts().autoCompact)}>
        {(tip) => <p class="context-tip">{tip()}</p>}
      </Show>

      <Show when={hasCompaction(facts().compaction)}>
        <div class="context-compaction">
          <h3 class="context-subhead">Compaction</h3>
          <p class="context-note">{compactionSummary(facts().compaction)}</p>
          <ul class="context-records">
            <For each={facts().compaction.records ?? []}>
              {(record) => (
                <li class="context-record">
                  <span class="context-ordinal">{record.ordinal}</span>
                  <span>{compactionRow(record)}</span>
                </li>
              )}
            </For>
          </ul>
          <Show when={undetailedCompactions(facts().compaction) > 0}>
            <p class="context-note">
              {undetailedCompactions(facts().compaction) === 1
                ? "1 compaction ran before this session was opened"
                : `${undetailedCompactions(facts().compaction)} compactions ran before this session was opened`}
            </p>
          </Show>
        </div>
      </Show>

      <p class="context-footer">{footerLine(facts())}</p>

      {/* There is no notification carrier for any of this — `session/info` is
          request/response and the pager debounces its own asking — so the
          window is re-read on attach, at the end of a turn, and here. A timer
          would be a cadence this product has not got. */}
      <Show when={props.onRefresh}>
        {(refresh) => (
          <button class="context-refresh" type="button" onClick={() => refresh()()}>
            Refresh
          </button>
        )}
      </Show>
    </div>
  );
}

/** One legend line: glyph, label, tokens, share, and the count-then-noun detail. */
function LegendRow(props: { row: Contributor; total: number }): JSX.Element {
  return (
    <li class="context-row">
      <span
        class="context-glyph"
        aria-hidden="true"
        style={{ color: `var(${cssVarName(contributorRole(props.row.kind))})` }}
      >
        {contributorGlyph(props.row.kind)}
      </span>
      <span class="context-label">{props.row.label}</span>
      <span class="context-tokens">{formatTokens(props.row.tokens)} tokens</span>
      <span class="context-percent">{percentOfWindow(props.row.tokens, props.total)}</span>
      {/* Only when there is one: an empty cell here is not free. In a column
          narrow enough for the detail to take a line of its own, an always
          rendered span would give every row that second line whether it had
          anything to say or not. */}
      <Show when={props.row.detail}>
        {(detail) => <span class="context-detail">· {detail()}</span>}
      </Show>
    </li>
  );
}

/**
 * The injected blocks, with the text each one's size was measured over.
 *
 * The size alone says how much a block costs; the text says what the cost
 * bought, and only one of the two can be acted on. An agent that reports the
 * size without the text says which half is missing rather than rendering an
 * empty block that reads as "this one is empty".
 */
function Injected(props: { rows: Contributor[] }): JSX.Element {
  return (
    <Show when={props.rows.length > 0}>
      <div class="context-injected">
        <h3 class="context-subhead">Injected context</h3>
        <For each={props.rows}>
          {(row) => (
            <section class="context-injection">
              <h4 class="context-injection-title">{row.label}</h4>
              <pre class="context-injection-text">
                {row.text ?? "(this agent reports the size but not the text)"}
              </pre>
            </section>
          )}
        </For>
        <p class="context-note">
          These blocks ride in every request and are already counted in the bands above, so they are
          listed here rather than added to them. Not listed: the system prompt and the tool schemas,
          both sized above; and the user-info prefix these blocks hang off, which cannot be
          re-rendered without re-running the git status it carries.
        </p>
      </div>
    </Show>
  );
}
