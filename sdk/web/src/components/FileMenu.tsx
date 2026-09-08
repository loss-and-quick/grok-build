import { For, Show, createEffect, createMemo, createSignal, type JSX } from "solid-js";

import {
  MAX_VISIBLE_ROWS,
  acceptInto,
  countHint,
  detectAt,
  drillInto,
  isDirMode,
  isHiddenMode,
  matchRuns,
  matcherQuery,
  type Acceptance,
  type AtContext,
  type FileSearch,
  type FuzzyMatch,
} from "../filesearch.ts";
import { PROMPT_ARROW } from "../glyphs.ts";

/**
 * The composer's `@`-menu, as state.
 *
 * Split from the component for the reason the slash menu is: the keys that drive
 * it arrive at the *textarea*. The pager is shaped the same way — `FileSearchState`
 * holds the context and the selection, and `handle_file_search_key` in the prompt
 * widget moves through it.
 *
 * What this does **not** hold is the result list. That lives on the gateway,
 * because the search is the session's and outlives any one opening of this menu:
 * see {@link FileSearch}.
 */
export interface FileMenu {
  /** Whether the list should be drawn. */
  open: () => boolean;
  rows: () => FuzzyMatch[];
  selected: () => number;
  select: (index: number) => void;
  /** Wraps at both ends, like the pager's `move_selection`. */
  move: (delta: number) => void;
  /** PageUp / PageDown: half a screen, the pager's own `page_move(±1, 8)`. */
  page: (delta: number) => void;
  /** Re-read the composer after any edit or caret move. */
  sync: (text: string, caret: number) => void;
  dismiss: () => void;
  revive: () => void;
  /** Tab and Enter: commit the selected row into `text`. */
  accept: (text: string, row?: FuzzyMatch) => Acceptance | null;
  /** The right arrow: step into a directory without committing it. */
  drill: (text: string) => Acceptance | null;
  /** The `k/n` the pager writes in the corner of its border. */
  hint: () => string;
}

export function createFileMenu(search: FileSearch): FileMenu {
  const [context, setContext] = createSignal<AtContext | null>(null);
  const [selected, setSelected] = createSignal(0);
  const [dismissedAt, setDismissedAt] = createSignal<string | null>(null);
  // The directory last stepped into. Whitespace inside it is part of the token,
  // so `@my dir/` keeps the menu up; `detectAt` drops it again as soon as the
  // typed text stops starting with it.
  const [drilled, setDrilled] = createSignal<string | null>(null);

  const rows = (): FuzzyMatch[] => (context() === null ? [] : search.matches());
  const open = (): boolean => {
    const asked = context();
    // Zero results closes the list rather than showing an empty box: the pager's
    // `is_visible()` is `context.is_some() && !topk.is_empty()`, and it has no
    // "no matches" row to draw. The typed text is left alone either way.
    return asked !== null && asked.query !== dismissedAt() && rows().length > 0;
  };
  const row = (): FuzzyMatch | null => (open() ? (rows()[selected()] ?? null) : null);

  const settle = (result: Acceptance): Acceptance => {
    setDrilled(result.drill);
    if (!result.keepOpen) setContext(null);
    return result;
  };

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
    page: (delta) => {
      const count = rows().length;
      if (count === 0) return;
      const step = Math.floor(MAX_VISIBLE_ROWS / 2) * delta;
      setSelected((current) => Math.min(Math.max(current + step, 0), count - 1));
    },

    sync: (text, caret) => {
      const next = detectAt(text, caret, drilled());
      const previous = context();
      setContext(next);
      if (!next) return;
      if (previous && previous.query === next.query) return;
      // The query moved, so the selection goes back to the top: any other rule
      // lets a keystroke that reorders the list also move which row Enter takes.
      setSelected(0);
      search.ask({
        query: matcherQuery(next),
        dirsOnly: isDirMode(next),
        hidden: isHiddenMode(next),
      });
    },

    dismiss: () => setDismissedAt(context()?.query ?? null),
    revive: () => setDismissedAt(null),

    accept: (text, chosen) => {
      const asked = context();
      const picked = chosen ?? row();
      if (!asked || !picked) return null;
      return settle(acceptInto(text, asked, picked, search.root()));
    },

    drill: (text) => {
      const asked = context();
      const picked = row();
      if (!asked || !picked) return null;
      return settle(drillInto(text, asked, picked, search.root()));
    },

    hint: () => countHint(rows().length, search.total()),
  };
}

/**
 * The dropdown.
 *
 * The pager's own layout: the prompt arrow on the selected row, then the path
 * **relative to the search root** with the matched characters picked out in
 * `fuzzy_accent`. One column, not a name and a dim directory beside it — the
 * pager draws the whole relative path as one string and highlights across it,
 * because that is what the matcher scored.
 *
 * Eight rows before it scrolls (`MAX_DROPDOWN_ROWS`), and the count the pager
 * writes into its top border goes in the header here, where a border with text
 * in it would have to be drawn out of characters.
 */
export function FileMenu(props: {
  menu: FileMenu;
  /** Directory rows show a trailing `/`; see the note in {@link Row}. */
  dirMode: boolean;
  root: string;
  onTake: (row: FuzzyMatch) => void;
}): JSX.Element {
  return (
    <Show when={props.menu.open()}>
      <div class="file-menu" style={{ "--file-rows": String(MAX_VISIBLE_ROWS) }}>
        <div class="file-menu-hint" aria-hidden="true">
          {props.menu.hint()}
        </div>
        <ul class="file-list" role="listbox" aria-label="Files">
          <For each={props.menu.rows()}>
            {(match, index) => (
              <Row
                match={match}
                root={props.root}
                dirMode={props.dirMode}
                selected={index() === props.menu.selected()}
                onPick={() => {
                  props.menu.select(index());
                  props.onTake(match);
                }}
              />
            )}
          </For>
        </ul>
      </div>
    </Show>
  );
}

function Row(props: {
  match: FuzzyMatch;
  root: string;
  dirMode: boolean;
  selected: boolean;
  onPick: () => void;
}): JSX.Element {
  let element: HTMLLIElement | undefined;
  createEffect(() => {
    if (props.selected) element?.scrollIntoView({ block: "nearest" });
  });

  // The runs come from `indices` on the wire. Nothing here re-runs a matcher:
  // the agent scored these paths with `nucleo` and sent the positions with them,
  // and a second matcher in the browser would disagree with the terminal about
  // which characters lit up.
  const runs = createMemo(() => matchRuns(props.root, props.match));

  return (
    <li
      ref={element}
      class="file-row"
      classList={{ selected: props.selected }}
      role="option"
      aria-selected={props.selected}
      // Mousedown, not click: a click blurs the textarea first, and the accept
      // that follows would have nowhere to put the text back.
      onMouseDown={(event) => {
        event.preventDefault();
        props.onPick();
      }}
    >
      <span class="file-caret" aria-hidden="true">
        {props.selected ? PROMPT_ARROW : ""}
      </span>
      <span class="file-path">
        <For each={runs()}>
          {(run) => <span classList={{ "file-match": run.match }}>{run.text}</span>}
        </For>
        {/* The trailing slash follows the *query's* dir mode rather than the
            row's own kind, which is the pager's rule (`dropdown.rs` gates it on
            `dir_mode`). In dir mode the matcher returns only directories, so the
            two agree; keeping the pager's condition keeps them agreeing if that
            ever stops being true. */}
        <Show when={props.dirMode}>
          <span class="file-slash">/</span>
        </Show>
      </span>
    </li>
  );
}
