// Branching a session: one call, and the two things about it that are not
// obvious from its name.
//
// `x.ai/session/fork` was on the wire and unused here
// (`shell/src/extensions/session_admin.rs:51`). The terminal reaches it from
// `/fork` through `Effect::ForkSession` (`pager/src/app/effects/mod.rs:4424`),
// and builds the params in one shared place, `fork_session_params`
// (`pager/src/app/session_startup.rs:62-83`), which is what this reproduces.
//
// ## It copies files; it does not start a session
//
// `fork_session` copies `chat_history.jsonl`, `updates.jsonl` and the plan
// state into a new session directory and returns — "creates new session files
// but does not start the session" (`shell/src/session/fork.rs:1-2`). So the
// child exists only on disk when the call answers. The terminal's next act is
// `Effect::LoadSession` on the new id (`app/dispatch/session/fork.rs:500-505`),
// and a browser's equivalent is to navigate to it, because navigating is what
// attaches here. Until something loads it, the roster shows it as `Dormant`:
// `merge_roster` emits every on-disk summary that has no resident actor
// (`shell/src/agent/roster.rs:137-140`).
//
// ## Where the child's cwd comes from, and the one thing this cannot do
//
// All three paths in the terminal's params are the parent's cwd in the ordinary
// case: `newCwd` is it outright, `sourceWorkspaceDir` is it when the parent is
// a worktree, and `sourceCwd` is it unless a disk lookup says otherwise. That
// lookup is the gap. `resolve_local_session_any_cwd` scans `$GROK_HOME/sessions/*`
// for the directory the session's files are actually under, so a terminal can
// fork a session whose recorded cwd has moved. A browser has the roster row and
// nothing else, so it sends the row's `cwd` — which is the same value in every
// case the roster is right about, and there is no wire call that would let it
// check.
//
// ## What is deliberately not here
//
// **The worktree branch.** `/fork` asks "Run this fork in an isolated git
// worktree?" and the yes answer does not go through this method at all: it
// calls `x.ai/git/worktree/create_from_worktree_sync` and then `session/new`
// (`app/dispatch/session/fork.rs:231-242`). That is a different pair of calls
// and a different screen, so offering the question here would mean offering an
// answer that does nothing.
//
// **The other two answers to that question**, which are not answers at all:
// "Always worktree" and "Never worktree" write `fork_worktree_mode` to
// `config.toml` (`app/dispatch/session/fork.rs:85-102`, and the persist at
// `router.rs:1308-1316`). A browser that reproduced them would have to write a
// setting, and this client does not write settings.
//
// ## Where to branch from, which is not what the neighbouring call means by it
//
// `targetPromptIndex` is the one field here that no client has ever sent —
// `fork_session_params` never sets it, and the pager answers `/fork --at` with
// "not supported in this version" — while the agent has implemented it all
// along, in `copy_session_data` and under test (`storage/jsonl/copy_tests.rs`).
//
// **Its cut is inclusive, and the identically named field on
// `x.ai/rewind/execute` is not.** `truncate_for_prompt_by` stops at the first
// line of the turn *after* the target — it cuts when `user_turn_count >
// target_prompt_index + 1` (`session/storage/mod.rs:1023-1046`) — so a fork at
// N leaves the child holding prompts `0..=N`. A rewind to N discards prompt N
// and hands its text back for the composer. One list of turns, two opposite
// meanings for the row that is picked, which is why the two dialogs are two
// dialogs rather than one with a flag.
//
// A partial fork also clears three things a full one keeps: the child's
// `last_turn_summary`, that summary's prompt id, and `last_recap`
// (`storage/jsonl/copy.rs:573-589`), because the work they describe may not be
// in the child. The title carries over either way, so the roster still has
// something to call the child; what it loses is the row's second line. That is
// also why "the whole conversation" is sent as an **absent** index rather than
// as the highest one — the same picture, a different call.

import type { RosterEntry } from "./wire.ts";

/** The params, field for field as `fork_session_params` builds them. */
export interface ForkParams {
  sourceSessionId: string;
  sourceCwd: string;
  newCwd: string;
  /** `"fork"`; the worktree path sets `"worktree"` and does not use this method. */
  sessionKind: string;
  /** Only for a worktree parent, which is the terminal's own condition. */
  sourceWorkspaceDir?: string;
  /** The last prompt the child keeps. Absent copies the whole conversation. */
  targetPromptIndex?: number;
}

/**
 * @param targetPromptIndex the last prompt to keep, **inclusive**. Omitted
 * rather than sent as null for the whole conversation: the agent reads an
 * absent key and a null one identically (`Option<usize>` behind
 * `#[serde(default)]`), but only one of them is what the terminal sends, and
 * only one of them says "no cut" rather than "a cut of nothing".
 */
export function forkParams(entry: RosterEntry, targetPromptIndex?: number): ForkParams {
  const params: ForkParams = {
    sourceSessionId: entry.sessionId,
    sourceCwd: entry.cwd,
    newCwd: entry.cwd,
    sessionKind: "fork",
  };
  if (entry.isWorktree) params.sourceWorkspaceDir = entry.cwd;
  if (targetPromptIndex !== undefined) params.targetPromptIndex = targetPromptIndex;
  return params;
}

/**
 * The child's id.
 *
 * `ForkSessionResponse` is `rename_all = "camelCase"` (`shell/src/session/fork.rs:37`)
 * — unlike the rewind responses next door, which carry no rename at all — so
 * there is one spelling here and it is this one. The `result` unwrap and the
 * `error` check are the terminal's, kept because they are cheap and because it
 * is what `fork_response_new_session_id` does (`app/session_startup.rs:129-141`):
 * an answer carrying an error is not an answer, whatever else is on it.
 */
export function readForkedSessionId(response: unknown): string | null {
  if (typeof response !== "object" || response === null) return null;
  const body = response as Record<string, unknown>;
  if (body["error"] != null) return null;
  const direct = body["newSessionId"];
  if (typeof direct === "string" && direct !== "") return direct;
  const inner = body["result"];
  if (typeof inner !== "object" || inner === null) return null;
  const nested = (inner as Record<string, unknown>)["newSessionId"];
  return typeof nested === "string" && nested !== "" ? nested : null;
}
