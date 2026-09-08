import { For, Show, createEffect, createMemo, createSignal, type JSX } from "solid-js";

import {
  MAX_VISIBLE_ROWS,
  badgeFor,
  menuQuery,
  rankCommands,
  type CommandRow,
} from "../commands.ts";
import { PROMPT_ARROW } from "../glyphs.ts";
import { highlightRuns } from "../highlight.ts";
import type { AvailableCommand } from "../wire.ts";

/**
 * The composer's slash menu, as state.
 *
 * Split from the component because the keys that drive it — arrows, Tab, Enter,
 * Escape — arrive at the *textarea*, not at the list. The pager has the same
 * shape: `SlashSnapshot` is derived from the prompt text and the cursor, and
 * the composer's key handler moves the selection through the controller rather
 * than the dropdown owning any of it.
 */
export interface CommandMenu {
  /** Whether the list should be drawn: a live query, not dismissed, with rows. */
  open: () => boolean;
  rows: () => CommandRow[];
  selected: () => number;
  select: (index: number) => void;
  /** Wraps at both ends, like `SlashController::move_selection`. */
  move: (delta: number) => void;
  /** Re-read the composer after any edit or caret move. */
  sync: (text: string, caret: number) => void;
  /** Escape, or the composer losing focus: closed until the query changes. */
  dismiss: () => void;
  /** Focus returning to the composer: an earlier Escape stops applying. */
  revive: () => void;
  /** The row Tab or Enter would take, or `null` when the menu is shut. */
  accept: () => CommandRow | null;
}

export function createCommandMenu(commands: () => readonly AvailableCommand[]): CommandMenu {
  const [query, setQuery] = createSignal<string | null>(null);
  const [selected, setSelected] = createSignal(0);
  // The query Escape was pressed at. The menu stays shut while the composer
  // still says that, and comes back the moment the text moves on — so escaping
  // out of a menu does not make the rest of the line unfilterable.
  const [dismissedAt, setDismissedAt] = createSignal<string | null>(null);

  const rows = createMemo(() => {
    const asked = query();
    return asked === null ? [] : rankCommands(commands(), asked);
  });

  const open = (): boolean => query() !== null && query() !== dismissedAt() && rows().length > 0;

  return {
    open,
    rows,
    selected: () => Math.min(selected(), Math.max(rows().length - 1, 0)),
    select: setSelected,
    move: (delta) => {
      const count = rows().length;
      if (count === 0) return;
      setSelected((current) => (((current + delta) % count) + count) % count);
    },
    sync: (text, caret) => {
      const asked = menuQuery(text, caret);
      // `carry_selection` keeps a selection only within one query; a changed
      // query starts at the top. Anything else lets a keystroke that reorders
      // the list also move which row Enter would take.
      if (asked !== query()) {
        setQuery(asked);
        setSelected(0);
      }
    },
    dismiss: () => setDismissedAt(query()),
    revive: () => setDismissedAt(null),
    accept: () => (open() ? (rows()[Math.min(selected(), rows().length - 1)] ?? null) : null),
  };
}

/**
 * The dropdown itself.
 *
 * The pager's own layout: a caret on the selected row, the command name with
 * the matched characters picked out in `fuzzy_accent`, the description after
 * it, and the provenance badge right-aligned. Eight rows before it scrolls, the
 * pager's `MAX_VISIBLE_SUGGESTIONS`.
 *
 * The badge is drawn on **every** row, where the pager draws it only on rows in
 * a name collision. The pager can afford that rule because it has another way
 * to answer "where did this come from" — its own `/plugins` and `/hooks`
 * screens, and a skill it renders visibly as a skill — and because the thing
 * its badge disambiguates is a *pager builtin* being shadowed. A browser has
 * none of that: this list is the only place a command's origin is stated at
 * all, so an unbadged row would be a plugin's command reading as built-in,
 * which is the confusion the badge was added for.
 */
export function CommandMenu(props: {
  menu: CommandMenu;
  /** Insert this row; the composer owns the text, so it owns the insert. */
  onTake: (row: CommandRow) => void;
}): JSX.Element {
  return (
    <Show when={props.menu.open()}>
      <ul
        class="slash-menu"
        role="listbox"
        aria-label="Slash commands"
        style={{ "--slash-rows": String(MAX_VISIBLE_ROWS) }}
      >
        <For each={props.menu.rows()}>
          {(row, index) => (
            <Row
              row={row}
              selected={index() === props.menu.selected()}
              onPick={() => {
                props.menu.select(index());
                props.onTake(row);
              }}
            />
          )}
        </For>
      </ul>
    </Show>
  );
}

function Row(props: { row: CommandRow; selected: boolean; onPick: () => void }): JSX.Element {
  let element: HTMLLIElement | undefined;
  // Keep the selected row in view as the arrows walk past the eighth one. The
  // pager scrolls its dropdown for the same reason; `nearest` is what keeps it
  // from jumping the whole list on every step.
  createEffect(() => {
    if (props.selected) element?.scrollIntoView({ block: "nearest" });
  });

  return (
    <li
      ref={element}
      class="slash-row"
      classList={{ selected: props.selected }}
      role="option"
      aria-selected={props.selected}
      // Chosen on mousedown, not click: a click would first blur the textarea,
      // and the accept that follows would have nowhere to put the text back.
      onMouseDown={(event) => {
        event.preventDefault();
        props.onPick();
      }}
    >
      <span class="slash-caret" aria-hidden="true">
        {props.selected ? PROMPT_ARROW : ""}
      </span>
      <span class="slash-name">
        <Highlighted text={props.row.display} indices={props.row.indices} />
      </span>
      <span class="slash-description">{props.row.command.description}</span>
      <span class="slash-badge">{badgeFor(props.row.provenance)}</span>
    </li>
  );
}

/**
 * The command name with the characters that spelled the query picked out —
 * `build_highlighted_spans`, which coalesces runs of the same style rather than
 * emitting a span per character.
 *
 * The coalescing is shared with the file list; **where the indices come from is
 * not.** These are computed here, because the command catalog on the wire
 * carries no match positions. The file list's arrive with the results, and it
 * computes nothing.
 */
function Highlighted(props: { text: string; indices: readonly number[] }): JSX.Element {
  const runs = createMemo(() => highlightRuns(props.text, props.indices));

  return (
    <For each={runs()}>
      {(run) => <span classList={{ "slash-match": run.match }}>{run.text}</span>}
    </For>
  );
}
