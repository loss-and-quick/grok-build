import { Show, type JSX } from "solid-js";

import { DISCLOSURE_CLOSED, DISCLOSURE_OPEN } from "../glyphs.ts";

/**
 * One section of the rail: a header row, and what it holds.
 *
 * The header is the pager's, line for line — `dock.rs`'s `section_header` draws
 * "chevron, label, count, then a rule filling the rest of the line", and that is
 * what this is in CSS. The rule is not decoration: it is what makes a stack of
 * sections read as a stack rather than as a list of unrelated headings, and the
 * terminal spends a whole line's worth of cells on it for that reason.
 *
 * The header is a button because the whole row is the control, which is how the
 * terminal folds anything: Enter on a section header collapses it
 * (`dock.rs:9-11`), not Enter on a widget beside it.
 */
export function Widget(props: {
  /** Identity in the rail's keyboard walk; see `rail.ts`. */
  itemKey: string;
  label: string;
  /** A list section's live count. Panels have no count — they have a source. */
  count?: number;
  /**
   * A built-in widget's one-line summary, for when the section is folded shut.
   *
   * The terminal has the same idea in the same place — its collapsed plugin
   * panel is a status chip that keeps the title and a count — because the whole
   * point of folding a section is that it still says whether it is worth
   * opening. A context widget folded to nothing but the word "Context" would
   * have to be opened to answer the only question it is ever asked.
   */
  note?: string;
  /** The plugin that published this panel. */
  source?: string;
  open: boolean;
  onToggle: () => void;
  /**
   * Raise the whole thing in a dialog.
   *
   * The rail is a column; some content is wider than a column can be, and a
   * `PanelBlock::Table` of four columns is measurably one of them. The terminal
   * has the same problem and the same answer — a chip in the status bar and the
   * full panel on F6 — so this is that key, as a button. Taking F6 itself is not
   * on the table: Firefox and Chrome use it to move focus between browser
   * regions, and that is an accessibility control.
   */
  onOpenFully?: () => void;
  children?: JSX.Element;
}): JSX.Element {
  const bodyId = (): string => `rail-body-${props.itemKey}`;
  return (
    <>
      <div class="rail-header">
        <button
          class="rail-disclosure"
          type="button"
          data-rail-item={props.itemKey}
          aria-expanded={props.open}
          aria-controls={bodyId()}
          onClick={() => props.onToggle()}
        >
          <span class="rail-chevron" aria-hidden="true">
            {props.open ? DISCLOSURE_OPEN : DISCLOSURE_CLOSED}
          </span>
          <span class="rail-label">{props.label}</span>
          <Show when={props.count !== undefined}>
            <span class="rail-count">{props.count}</span>
          </Show>
          <Show when={props.note}>{(note) => <span class="rail-note">{note()}</span>}</Show>
          <Show when={props.source}>
            {(source) => <span class="rail-source">{source()}</span>}
          </Show>
          <span class="rail-rule" aria-hidden="true" />
        </button>
        <Show when={props.onOpenFully}>
          {(open) => (
            <button
              class="rail-open"
              type="button"
              title={`Open ${props.label} in a dialog`}
              onClick={() => open()()}
            >
              Open
            </button>
          )}
        </Show>
      </div>
      <Show when={props.open}>
        <div class="rail-body" id={bodyId()}>
          {props.children}
        </div>
      </Show>
    </>
  );
}
