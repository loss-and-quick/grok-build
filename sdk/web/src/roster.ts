// The roster, grouped by working directory.
//
// This grouping is the product's whole claim, so it is the first thing the
// client draws. A session's root is a parameter of `session/new`, never process
// state; the leader does not rewrite it, and roster entries carry it
// (`crates/codegen/xai-grok-shell/src/agent/roster.rs`). "An instance" is
// therefore a directory with sessions in it, and switching instances is
// switching sessions — not repointing one.
import { createStore, produce } from "solid-js/store";

import type { ThemeRole } from "./theme.ts";
import type { RosterActivity, RosterChanged, RosterEntry } from "./wire.ts";

export interface DirectoryGroup {
  cwd: string;
  sessions: RosterEntry[];
}

export interface Roster {
  replace(sessions: readonly RosterEntry[]): void;
  apply(change: RosterChanged): void;
  get(sessionId: string): RosterEntry | undefined;
  all(): RosterEntry[];
  groups(): DirectoryGroup[];
}

/**
 * Live roster: the `x.ai/sessions/list` snapshot reconciled against
 * `x.ai/sessions/changed`.
 *
 * A store rather than a `Map` so an upsert of one session re-renders that row
 * and not the sidebar. `x.ai/sessions/changed` is a broadcast — every attached
 * client gets every session's activity flip — so this is the collection under
 * the most churn.
 */
export function createRoster(): Roster {
  const [entries, setEntries] = createStore<Record<string, RosterEntry>>({});

  return {
    replace(sessions) {
      setEntries(
        produce((state) => {
          for (const id of Object.keys(state)) delete state[id];
          for (const entry of sessions) state[entry.sessionId] = entry;
        }),
      );
    },
    apply(change) {
      setEntries(
        produce((state) => {
          for (const entry of change.upserted ?? []) state[entry.sessionId] = entry;
          for (const id of change.removed ?? []) delete state[id];
        }),
      );
    },
    get: (sessionId) => entries[sessionId],
    all: () => Object.values(entries),
    groups: () => groupByDirectory(Object.values(entries)),
  };
}

/**
 * Group by `cwd`, newest directory first, newest session first inside it.
 *
 * A directory's position follows its most recently changed session, so the
 * place you were working stays at the top without any client-side notion of
 * "current" — which the wire does not have and this client must not invent.
 */
export function groupByDirectory(entries: readonly RosterEntry[]): DirectoryGroup[] {
  const byCwd = new Map<string, RosterEntry[]>();
  for (const entry of entries) {
    const bucket = byCwd.get(entry.cwd);
    if (bucket) bucket.push(entry);
    else byCwd.set(entry.cwd, [entry]);
  }
  const groups: DirectoryGroup[] = [];
  for (const [cwd, sessions] of byCwd) {
    sessions.sort((a, b) => b.lastChangeUnixMs - a.lastChangeUnixMs);
    groups.push({ cwd, sessions });
  }
  groups.sort((a, b) => {
    const recency = latestChange(b) - latestChange(a);
    return recency !== 0 ? recency : a.cwd.localeCompare(b.cwd);
  });
  return groups;
}

function latestChange(group: DirectoryGroup): number {
  return group.sessions.reduce((max, s) => Math.max(max, s.lastChangeUnixMs), 0);
}

/** Last path segment, for a heading that fits. The full path stays in the title attribute. */
export function directoryLabel(cwd: string): string {
  const trimmed = cwd.replace(/\/+$/, "");
  const slash = trimmed.lastIndexOf("/");
  return slash >= 0 && slash < trimmed.length - 1 ? trimmed.slice(slash + 1) : trimmed || "/";
}

/** Where a derived title is cut. `MAX_TITLE_CHARS` in `views/session_title.rs`. */
export const MAX_TITLE_CHARS = 60;

/** How much of a session id stands in for a name. `entry_title`'s `take(8)`. */
export const SHORT_ID_CHARS = 8;

/**
 * Characters a title may not contain.
 *
 * `is_forbidden_title_char` (`shell/src/session/persistence.rs`): every control
 * character, plus the bidi overrides and isolates. The terminal's reason is
 * that a title is printed into a stream that reads escapes; the browser's is
 * that a right-to-left override in a heading can make it read as something
 * else entirely. Both want them gone, so the class is the same one.
 */
const FORBIDDEN_TITLE_CHAR = /[\p{Cc}\u200E\u200F\u202A-\u202E\u2066-\u2069]/gu;

/**
 * Sanitise and cut a title, the way `entry_title` does before showing one.
 *
 * A forbidden character becomes `U+FFFD` rather than vanishing — the pager's
 * choice, and the better one: something was there, and a reader should be able
 * to tell.
 */
export function displayTitle(text: string): string {
  const clean = text.replace(FORBIDDEN_TITLE_CHAR, "\uFFFD").trim();
  const chars = [...clean];
  return chars.length <= MAX_TITLE_CHARS
    ? clean
    : `${chars.slice(0, MAX_TITLE_CHARS).join("")}...`;
}

/** `session 3f2a1b9c` — what the pager calls a session with nothing else to call it. */
export function shortSessionName(sessionId: string): string {
  return `session ${[...sessionId].slice(0, SHORT_ID_CHARS).join("")}`;
}

/**
 * What to call a session.
 *
 * `entry_title` (`pager/src/views/session_title.rs`) in the same order: the
 * rename, then the generated title, then the first user prompt in the
 * scrollback, then a short form of the id. The last step is the one that
 * mattered — a live session with no title yet reached the fallback, and the
 * fallback was the whole UUID, so the page was headed by 36 characters of hex
 * where the terminal shows eight behind the word "session".
 *
 * `firstPrompt` is optional because the sidebar has no transcript to read one
 * from; the pager's dashboard is in the same position and falls through the
 * same way.
 */
export function sessionLabel(entry: RosterEntry, firstPrompt?: string): string {
  for (const candidate of [entry.title, entry.lastTurnSummary, firstPrompt]) {
    const trimmed = candidate?.trim();
    if (trimmed) return displayTitle(trimmed);
  }
  return shortSessionName(entry.sessionId);
}

/**
 * Colour role for an activity.
 *
 * Roles, not colours: every value here is a key of the generated palette, so a
 * theme change moves this client with the pager.
 */
export const ACTIVITY_ROLE: Record<RosterActivity, ThemeRole> = {
  working: "accent_running",
  idle: "gray",
  needs_input: "warning",
  dormant: "gray_dim",
  completed: "accent_success",
  dead: "accent_error",
};
