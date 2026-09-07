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

export function sessionLabel(entry: RosterEntry): string {
  return entry.title?.trim() || entry.lastTurnSummary?.trim() || entry.sessionId;
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
