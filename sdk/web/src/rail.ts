// The widget rail's model — a port of the pager's dock, not a new design.
//
// `views/dock.rs` is the Figma "Exploration" layout as the terminal built it:
// "one header row per non-empty section (Subagents / Tasks / Watchers / Queued)
// directly above the prompt, each with a live count and a rule filling the rest
// of the line. Sections with a zero count are hidden; an all-zero dock renders
// nothing." A browser has a window manager, so the same stack of sections
// becomes a column beside the session instead of a strip above the prompt — and
// when the window is too narrow for a third column it goes back to being that
// strip. Same model, two geometries.
//
// What is ported here is the part a second implementation would otherwise get
// subtly different: which lines exist, which of them the cursor can land on, and
// how it moves. `dock.rs` derives all of that from one walk — "so the cursor,
// mouse hit-testing, height, and paint can't drift" — and that walk is
// [`visualRows`]. The component below it renders the walk rather than deciding
// for itself what to draw, for exactly the reason the module comment there
// gives.
//
// This is the same argument `commands.ts` makes for porting `nucleo` instead of
// taking a fuzzy-matching library: a list widget from elsewhere would supply a
// *different* traversal, which is divergence introduced in the name of avoiding
// work.

/**
 * Rows a list section shows before its "N more" line.
 *
 * The pager's own `MAX_SECTION_ROWS`. Taken rather than chosen: a browser could
 * afford more, but a count the product has already named is worth more than a
 * better one invented here — the same rule `TYPED_LINE_LIMIT` follows.
 */
export const MAX_SECTION_ROWS = 2;

/** One line inside a list section. */
export interface RailRow {
  /** Stable within its section; what an activated row is reported as. */
  key: string;
  label: string;
  /** Right-hand column: an elapsed time, a model, a status. */
  meta?: string;
  /** Marks the row as live, which is what earns it the running accent. */
  running?: boolean;
}

/**
 * A section of the rail.
 *
 * Three shapes, because the rail carries three kinds of thing. A `list` is the
 * dock's own section: a count, and rows capped at {@link MAX_SECTION_ROWS}. A
 * `panel` is a plugin's published `PanelViewModel`, drawn by `Panel.tsx` as its
 * own body. A `widget` is a built-in whose body is a picture rather than rows —
 * the context window is one, and the dock's row cap has nothing to say about a
 * bar and its legend.
 *
 * The third shape is not a hedge against the second. It was the first thing the
 * rail was asked for that is neither: `dock.rs` has only list sections because
 * every one of its four *is* a list, and a client that filed the context
 * breakdown as two capped rows would be inventing a shape neither product has.
 */
export type RailSection =
  | { kind: "list"; key: string; label: string; rows: readonly RailRow[] }
  | { kind: "widget"; key: string; label: string; note?: string }
  | { kind: "panel"; key: string; label: string; source: string };

/** One painted line of the rail, in order. */
export type RailVisual =
  | { kind: "header"; section: string }
  | { kind: "row"; section: string; at: number }
  | { kind: "more"; section: string; hidden: number }
  | { kind: "body"; section: string };

/** A line the cursor may land on. */
export type RailItem = Extract<RailVisual, { kind: "header" | "row" }>;

/**
 * Whether a section is drawn at all.
 *
 * `dock.rs`: a section with a zero count is skipped, and a dock whose sections
 * are all zero renders nothing. A panel section is never empty in that sense —
 * publishing one *is* the plugin asking for the space, which is why it has no
 * count to be zero — and neither is a built-in widget: whoever supplies one has
 * already decided it has something to say, and a widget with nothing is simply
 * not supplied. The emptiness rule is about counts, and only lists have one.
 */
export function sectionShown(section: RailSection): boolean {
  return section.kind !== "list" || section.rows.length > 0;
}

/** How many rows a list section draws before the "N more" line. */
export function shownRows(length: number): number {
  return Math.min(length, MAX_SECTION_ROWS);
}

/**
 * The rail's painted line sequence.
 *
 * The single walk everything else derives from: the render, the item list the
 * cursor indexes, and the section a line belongs to. Keeping them one function
 * is the whole of `dock.rs`'s "can't drift" claim.
 */
export function visualRows(
  sections: readonly RailSection[],
  collapsed: ReadonlySet<string>,
): RailVisual[] {
  const rows: RailVisual[] = [];
  for (const section of sections) {
    if (!sectionShown(section)) continue;
    rows.push({ kind: "header", section: section.key });
    if (collapsed.has(section.key)) continue;
    if (section.kind !== "list") {
      rows.push({ kind: "body", section: section.key });
      continue;
    }
    const shown = shownRows(section.rows.length);
    for (let at = 0; at < shown; at += 1) rows.push({ kind: "row", section: section.key, at });
    if (section.rows.length > shown) {
      rows.push({ kind: "more", section: section.key, hidden: section.rows.length - shown });
    }
  }
  return rows;
}

/**
 * The lines the cursor walks, in render order.
 *
 * Headers and rows interleaved, exactly as `dock.rs` does it — the cursor is
 * one sequence over both, so Down from a section's last row lands on the next
 * section's header rather than skipping to its first row. A "N more" line is
 * not selectable there and is not here. Neither is a section body: what is
 * inside one is ordinary content with its own focusable elements, and Tab is
 * what reaches those.
 */
export function railItems(
  sections: readonly RailSection[],
  collapsed: ReadonlySet<string>,
): RailItem[] {
  return visualRows(sections, collapsed).filter(
    (visual): visual is RailItem => visual.kind === "header" || visual.kind === "row",
  );
}

/** The identity of an item, for a roving `tabindex` and for tests. */
export function itemKey(item: RailItem): string {
  return item.kind === "header" ? `h:${item.section}` : `r:${item.section}:${item.at}`;
}

/**
 * Where the cursor lands after a move.
 *
 * Clamped, not wrapped — the pager's own `saturating_sub` and `min`. A cursor
 * that wraps from the last row to the first turns "press Down until it stops"
 * into a loop with no end, which is worse in a column than in a menu because
 * the column is the thing being read.
 */
export function moveCursor(count: number, cursor: number, delta: number): number {
  if (count === 0) return 0;
  return Math.min(Math.max(cursor + delta, 0), count - 1);
}

/**
 * Whether this browser draws the rail.
 *
 * The gate is the agent's own. `dock_enabled` rides `x.ai/settings/update`,
 * which the shell sends with `forward_fire_and_forget` — to *every* attached
 * client, not to the terminal alone (`mvp_agent/mod.rs:2080-2087`) — and this
 * client used to drop the notification on the floor, so a cohort the dock was
 * turned on for saw no sign of it in a browser.
 *
 * `local` is this browser's own layer, and it is not an invention either: the
 * pager resolves the same feature through `pin → env → config → remote →
 * default off` (`app/mod.rs:199-209`), where `config` is a `[features] dock` in
 * a file on that machine. A browser has no such file; what it has is storage,
 * and it sits in the same place in the order — above the cohort flag, because a
 * person who asked for this on this machine has said something more specific
 * than a rollout did.
 */
export function railEnabled(local: boolean | null, remote: boolean | null): boolean {
  return local ?? remote ?? false;
}
