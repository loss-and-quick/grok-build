import { Index, Show, type Accessor, type JSX } from "solid-js";

import { BULLET, DISCLOSURE_OPEN } from "../glyphs.ts";
import { shownRows, type RailRow } from "../rail.ts";

/**
 * The rows of one list section, capped the way the dock caps them.
 *
 * `dock.rs` paints `    ◆ Kind description — activity` with the meta column
 * right-aligned, then a `▾ N more` line when the section holds more than it
 * shows (`:326-372`, `:165-171`). This is that, in CSS: the same glyph, the
 * same cap, the same overflow line, and the same rule about which lines the
 * cursor may land on.
 *
 * The rows carry `data-rail-item` and `tabindex={-1}`, which is the pager's
 * cursor model rather than a browser one: a row is reached by walking down from
 * its section header, not by Tab. Tab is left for the controls *inside* a row —
 * the stop button — because those are what a keyboard user must reach without
 * first knowing the rail has a walk.
 *
 * The "N more" line is not one of them, here or there. It is not a button
 * either: **Open** already raises the whole section, and giving the overflow
 * line its own way in would be a second answer to a question already answered.
 *
 * **`Index`, not `For`.** A row's elapsed time moves every tick, so the row
 * objects are rebuilt every tick, and `For` keys by reference — it would
 * discard and rebuild every row several times a second, taking the focused row
 * and a half-armed stop button with it. `Index` keys by position and updates
 * the text in place, which is also what the terminal does: `dock.rs` addresses
 * a row as `Row(section, i)`.
 */
export function RailList(props: {
  /** The section key; a row's identity in the walk is built from it. */
  section: string;
  rows: readonly RailRow[];
  /** Drawn at the end of a row — a stop button, or nothing. */
  action?: (row: Accessor<RailRow>, at: number) => JSX.Element;
}): JSX.Element {
  const shown = (): RailRow[] => props.rows.slice(0, shownRows(props.rows.length));
  const hidden = (): number => props.rows.length - shown().length;

  return (
    <ul class="rail-rows">
      <Index each={shown()}>
        {(row, at) => (
          <li
            class="rail-row"
            classList={{ running: row().running === true }}
            data-rail-item={`r:${props.section}:${at}`}
            tabindex={-1}
          >
            <span class="rail-row-mark" aria-hidden="true">
              {BULLET}
            </span>
            <span class="rail-row-text">
              <Show when={row().kind}>{(kind) => <span class="rail-row-kind">{kind()}</span>}</Show>
              <span class="rail-row-label">{row().label}</span>
              <Show when={row().activity}>
                {(activity) => <span class="rail-row-activity">{activity()}</span>}
              </Show>
            </span>
            <Show when={row().meta}>{(meta) => <span class="rail-row-meta">{meta()}</span>}</Show>
            {props.action?.(row, at)}
          </li>
        )}
      </Index>
      <Show when={hidden() > 0}>
        <li class="rail-more">
          <span aria-hidden="true">{DISCLOSURE_OPEN}</span> {hidden()} more
        </li>
      </Show>
    </ul>
  );
}
