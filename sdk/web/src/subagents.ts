// A fan-out, folded from the wire.
//
// Pure and DOM-free, like `transcript.ts`, so the fold is testable without a
// browser and the component below it only renders. Nothing here asks the agent
// anything: every field is either something a `subagent_*` update said, or
// `undefined` because the wire has not said it yet. That distinction is the
// whole discipline of this module — a browser that fills a blank with a
// plausible zero is a browser that lies about a child it cannot see.
//
// What the terminal decided, and is therefore reproduced rather than invented:
//
//   - the row label (persona > role > type > `[tag]` > "general"), and the
//     `[tag]` prefix being stripped from the description either way
//     (`app/subagent.rs:840`, `format_subagent_label`);
//   - `"general-purpose"` displaying as `general` (`:810`);
//   - the meta suffix ` (persona · role · model)`, with persona and role
//     collapsed when they name the same thing (`:875`, `:798`);
//   - the activity vocabulary — "Thinking", "Responding", "Running: <title>"
//     clamped to 40 characters (`app/subagent.rs:891`,
//     `acp/tracker.rs:80`);
//   - the compact duration format (`pager-render/src/util.rs:83`);
//   - the sort: running first, then agent type alphabetically, then newest
//     first, then a stable id (`views/tasks_pane.rs:941`);
//   - hiding finished rows behind a toggle (`tasks_pane.rs:900`, `show_done`).
import { createStore, produce } from "solid-js/store";

import { fmtElapsed, type RailRow } from "./rail.ts";
import type {
  SessionUpdate,
  SessionUpdateSubagentFinished,
  SessionUpdateSubagentProgress,
  SessionUpdateSubagentSpawned,
  SubagentLiveSnapshot,
} from "./wire.ts";

/**
 * How long a stop gesture stands before it goes back to offering itself.
 *
 * The pager's own `PENDING_KILL_TIMEOUT_SECS` (`app/agent.rs:153`), used here
 * for both halves of the same idea: how long the button stays armed waiting for
 * a second click, and how long a sent-but-unanswered stop keeps the row marked.
 * A test reads the Rust so the two cannot drift.
 */
export const PENDING_KILL_TIMEOUT_MS = 10_000;

/** Where `format_duration` changes unit (`pager-render/src/util.rs:83`). */
export const DURATION_BREAKS = { tenths: 10, minute: 60, hour: 60 } as const;

/** `MAX_ACTIVITY_SUBJECT_CHARS` (`acp/tracker.rs:80`). */
export const MAX_ACTIVITY_SUBJECT_CHARS = 40;

/**
 * A child's status, as the wire can express it.
 *
 * `subagent_finished` carries the three terminal words verbatim
 * (`extensions/notification.rs:730`). `"running"` is not on the stream at all:
 * it is the absence of a finish, which is exactly how the pager defines it
 * (`SubagentInfo::is_running`, `app/subagent.rs:188`). The agent separately
 * knows an `initializing` state and will report it through
 * `x.ai/subagent/get`, but it never announces it, so a client watching the
 * stream cannot tell a child that is being set up from one that is working.
 */
export type SubagentStatus = "running" | "completed" | "failed" | "cancelled" | (string & {});

/** One child, as far as this client has been told. `undefined` means "not said". */
export interface Subagent {
  subagentId: string;
  parentSessionId: string;
  childSessionId: string;
  subagentType: string;
  /** The model's one-line summary of the task. Never the task prompt; see the README. */
  description: string;
  persona?: string;
  role?: string;
  model?: string;
  /** `"new"` or `"resumed"`, after the agent resolved what it actually did. */
  contextSource?: string;
  contextNormalized: boolean;
  capabilityMode?: string;
  resumedFrom?: string;
  workflowRunId?: string;
  parentPromptId?: string;
  /** Arrival order of the spawn, which is the only "when" the stream carries. */
  seq: number;
  /** Depth below the attached session: 0 for its own children, 1 for theirs. */
  depth: number;

  status: SubagentStatus;
  /** The agent's own elapsed measurement, from the last frame that carried one. */
  durationMs?: number;
  /** `performance.now()` when `durationMs` arrived, so it can be carried forward honestly. */
  durationReadAtMs?: number;
  turns?: number;
  toolCalls?: number;
  tokensUsed?: number;
  contextWindowTokens?: number;
  contextUsagePct?: number;
  toolsUsed?: string[];
  errorCount?: number;

  /** Set on a `failed` finish. */
  error?: string;
  /** The child's final answer, on a `completed` finish. */
  output?: string;

  /** Folded from the child's own update stream; `undefined` until it says something. */
  activity?: string;

  /** A stop was sent and no finish has arrived yet. */
  pendingKill: boolean;
  killRequestedAtMs?: number;
}

export interface Subagents {
  readonly rows: readonly Subagent[];
  /** Child session ids worth listening to, so the caller can route their updates here. */
  childSessions(): ReadonlySet<string>;
  /** Fold one `subagent_*` update, arriving on `sessionId`. Ignores every other tag. */
  apply(sessionId: string, update: SessionUpdate): void;
  /** Fold one of a child's own updates into that child's activity label. */
  applyChild(childSessionId: string, update: SessionUpdate): void;
  /** Seed from `x.ai/subagent/list_running`, for children that started before this client attached. */
  seed(parentSessionId: string, snapshots: readonly SubagentLiveSnapshot[]): void;
  /** Mark a stop as sent; cleared by the finish, or by {@link PENDING_KILL_TIMEOUT_MS}. */
  markKillSent(subagentId: string, nowMs: number): void;
  /**
   * Close a row when the agent said no finish is coming.
   *
   * The counters are left exactly as the last tick reported them. Synthesizing
   * a `subagent_finished` here instead would write zero tool calls and a zero
   * duration over numbers the agent actually sent, which is the shape of lie
   * this module exists to avoid.
   */
  finalize(subagentId: string, status: SubagentStatus): void;

  /** Drop this row's pending-kill mark: the stop failed, so the child may still be running. */
  clearKill(subagentId: string): void;
  /** Drop pending-kill marks the agent never answered, so the stop can be offered again. */
  expireKills(nowMs: number): void;
}

export function createSubagents(rootSessionId: string): Subagents {
  const [rows, setRows] = createStore<Subagent[]>([]);
  const at = new Map<string, number>();
  // Every session whose updates belong to this fan-out: the attached session
  // and every live child of it. The leader subscribes a client to a child the
  // moment the parent's `subagent_spawned` goes out (`leader/server.rs:2315`)
  // and to a replayed one on attach (`:2133`), so these frames arrive whether
  // or not anything asks for them — this set is only how they are recognised.
  const sessions = new Map<string, number>([[rootSessionId, -1]]);

  const spawn = (sessionId: string, update: SessionUpdateSubagentSpawned): void => {
    const id = update.subagent_id;
    if (at.has(id)) return; // A duplicate spawn must not replace live state.
    const parentDepth = sessions.get(sessionId);
    const row: Subagent = {
      subagentId: id,
      parentSessionId: update.parent_session_id || sessionId,
      childSessionId: update.child_session_id,
      subagentType: update.subagent_type,
      description: update.description,
      persona: nonEmpty(update.persona),
      role: nonEmpty(update.role),
      model: nonEmpty(update.model),
      contextSource: nonEmpty(update.effective_context_source),
      contextNormalized: update.context_normalized === true,
      capabilityMode: nonEmpty(update.capability_mode),
      resumedFrom: nonEmpty(update.resumed_from),
      workflowRunId: nonEmpty(update.workflow_run_id),
      parentPromptId: nonEmpty(update.parent_prompt_id),
      seq: rows.length,
      depth: (parentDepth ?? -1) + 1,
      status: "running",
      pendingKill: false,
    };
    sessions.set(row.childSessionId, row.depth);
    at.set(id, rows.length);
    setRows(rows.length, row);
  };

  const progress = (update: SessionUpdateSubagentProgress): void => {
    edit(update.subagent_id, (row) => {
      row.durationMs = update.duration_ms;
      row.durationReadAtMs = performance.now();
      row.turns = update.turn_count;
      row.toolCalls = update.tool_call_count;
      row.tokensUsed = update.tokens_used;
      row.contextWindowTokens = update.context_window_tokens;
      row.contextUsagePct = update.context_usage_pct;
      row.toolsUsed = update.tools_used;
      row.errorCount = update.error_count;
    });
  };

  const finish = (update: SessionUpdateSubagentFinished): void => {
    edit(update.subagent_id, (row) => {
      // One terminal transition per child; a duplicate finish must not
      // re-finalize, which is the pager's rule too (`subagent.rs:50`).
      if (row.status !== "running") return;
      row.status = update.status;
      row.error = nonEmpty(update.error);
      row.output = nonEmpty(update.output);
      row.toolCalls = update.tool_calls;
      row.turns = update.turns;
      row.durationMs = update.duration_ms;
      row.durationReadAtMs = undefined;
      if (typeof update.tokens_used === "number") row.tokensUsed = update.tokens_used;
      row.pendingKill = false;
      row.killRequestedAtMs = undefined;
      // The child is gone, so its last activity is not what it is doing now.
      row.activity = undefined;
    });
    sessions.delete(update.child_session_id);
  };

  const edit = (subagentId: string, change: (row: Subagent) => void): void => {
    const index = at.get(subagentId);
    if (index === undefined) return;
    setRows(index, produce(change));
  };

  return {
    rows,
    childSessions: () => new Set([...sessions.keys()].filter((id) => id !== rootSessionId)),

    apply(sessionId, update) {
      switch (update.sessionUpdate) {
        case "subagent_spawned":
          // A spawn on a session this fan-out does not contain belongs to
          // another client's session on the same leader; dropping it is what
          // keeps a browser tab showing one session's children.
          if (!sessions.has(sessionId)) return;
          spawn(sessionId, update as unknown as SessionUpdateSubagentSpawned);
          return;
        case "subagent_progress":
          progress(update as unknown as SessionUpdateSubagentProgress);
          return;
        case "subagent_finished":
          finish(update as unknown as SessionUpdateSubagentFinished);
          return;
        default:
          return;
      }
    },

    applyChild(childSessionId, update) {
      const row = rows.find((candidate) => candidate.childSessionId === childSessionId);
      if (!row || row.status !== "running") return;
      const label = activityOf(update);
      if (label === undefined) return;
      edit(row.subagentId, (target) => {
        target.activity = label;
      });
    },

    seed(parentSessionId, snapshots) {
      for (const snapshot of snapshots) {
        const known = at.get(snapshot.subagentId);
        if (known === undefined) {
          spawn(parentSessionId, {
            sessionUpdate: "subagent_spawned",
            subagent_id: snapshot.subagentId,
            parent_session_id: snapshot.parentSessionId,
            child_session_id: snapshot.childSessionId,
            subagent_type: snapshot.subagentType,
            description: snapshot.description,
          });
        }
        // The counters are the same fields a progress tick carries, so they
        // fold through the same path rather than a second one.
        progress({
          sessionUpdate: "subagent_progress",
          subagent_id: snapshot.subagentId,
          parent_session_id: snapshot.parentSessionId,
          child_session_id: snapshot.childSessionId,
          duration_ms: snapshot.durationMs,
          turn_count: snapshot.turnCount,
          tool_call_count: snapshot.toolCallCount,
          tokens_used: snapshot.tokensUsed,
          context_window_tokens: snapshot.contextWindowTokens,
          context_usage_pct: snapshot.contextUsagePct,
          tools_used: snapshot.toolsUsed,
          error_count: snapshot.errorCount,
        });
      }
    },

    markKillSent(subagentId, nowMs) {
      edit(subagentId, (row) => {
        row.pendingKill = true;
        row.killRequestedAtMs = nowMs;
      });
    },

    finalize(subagentId, status) {
      edit(subagentId, (row) => {
        if (row.status !== "running") return;
        row.status = status;
        row.pendingKill = false;
        row.killRequestedAtMs = undefined;
        row.activity = undefined;
        row.durationReadAtMs = undefined;
      });
    },

    clearKill(subagentId) {
      edit(subagentId, (row) => {
        row.pendingKill = false;
        row.killRequestedAtMs = undefined;
      });
    },

    expireKills(nowMs) {
      for (const row of rows) {
        if (!row.pendingKill || row.killRequestedAtMs === undefined) continue;
        if (nowMs - row.killRequestedAtMs < PENDING_KILL_TIMEOUT_MS) continue;
        edit(row.subagentId, (target) => {
          target.pendingKill = false;
          target.killRequestedAtMs = undefined;
        });
      }
    },
  };
}

function nonEmpty(value: string | null | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

// ---------------------------------------------------------------------------
// The pager's own labelling, reproduced
// ---------------------------------------------------------------------------

/** `format_type_label` (`app/subagent.rs:810`). */
export function typeLabel(subagentType: string): string {
  return subagentType === "general-purpose" ? "general" : subagentType;
}

/**
 * `parse_tag_prefix` (`app/subagent.rs:826`): a leading `[tag]` is a label, not
 * part of the description, and is stripped from the description either way so
 * no bracket noise renders inline.
 */
export function parseTagPrefix(description: string): { tag?: string; rest: string } {
  if (!description.startsWith("[")) return { rest: description };
  const close = description.indexOf("]", 1);
  if (close < 0) return { rest: description };
  const tag = description.slice(1, close).trim();
  if (!tag) return { rest: description };
  return { tag, rest: description.slice(close + 1).replace(/^\s+/, "") };
}

/**
 * `format_subagent_label` (`app/subagent.rs:840`) — one label and a cleaned
 * description, in the pager's own order of preference.
 */
export function subagentLabel(row: Subagent): { label: string; description: string } {
  const { tag, rest } = parseTagPrefix(row.description);
  const raw =
    row.persona ??
    row.role ??
    (row.subagentType !== "general-purpose" ? typeLabel(row.subagentType) : undefined) ??
    tag ??
    "general";
  return { label: raw.charAt(0).toUpperCase() + raw.slice(1), description: rest };
}

/**
 * `format_subagent_meta` (`app/subagent.rs:875`), including the
 * `dedup_persona_role` collapse at `:798`: when persona and role name the same
 * title, only one of them is shown.
 */
export function metaSuffix(row: Subagent): string {
  const same =
    row.persona !== undefined &&
    row.role !== undefined &&
    row.persona.trim().toLowerCase() === row.role.trim().toLowerCase();
  const parts = [row.persona, same ? undefined : row.role, row.model].filter(
    (part): part is string => part !== undefined,
  );
  return parts.length === 0 ? "" : ` (${parts.join(" · ")})`;
}

/** `format_context_badge` (`app/subagent.rs:817`): only these two are badges. */
export function contextBadge(row: Subagent): string {
  return row.contextSource === "resumed" || row.contextSource === "forked" ? row.contextSource : "";
}

/** `format_duration` (`pager-render/src/util.rs:83`): `5.2s`, `32s`, `2m5s`, `1h2m`. */
export function formatDuration(ms: number): string {
  const totalSecs = Math.floor(ms / 1000);
  if (totalSecs < DURATION_BREAKS.tenths) return `${(ms / 1000).toFixed(1)}s`;
  if (totalSecs < DURATION_BREAKS.minute) return `${totalSecs}s`;
  const mins = Math.floor(totalSecs / DURATION_BREAKS.minute);
  const secs = totalSecs % DURATION_BREAKS.minute;
  if (mins < DURATION_BREAKS.hour) return `${mins}m${secs}s`;
  return `${Math.floor(mins / DURATION_BREAKS.hour)}h${mins % DURATION_BREAKS.hour}m`;
}

/**
 * How long the child has been running, or `undefined` when nothing has said.
 *
 * `durationMs` is the agent's own measurement; the time since that frame
 * arrived is added so the number moves between ticks — which can be two
 * seconds apart, or eight on a quiet child. Both halves are measured, neither
 * is assumed. A child that has not ticked yet has no start time on the wire at
 * all, and gets `undefined` rather than a zero it did not earn.
 */
export function elapsedMs(row: Subagent, nowMs: number): number | undefined {
  if (row.durationMs === undefined) return undefined;
  if (row.durationReadAtMs === undefined) return row.durationMs;
  return row.durationMs + Math.max(0, nowMs - row.durationReadAtMs);
}

/** `clamp_activity_subject` (`acp/tracker.rs:82`), for the tool title in an activity label. */
export function clampSubject(subject: string): string {
  const line = subject.split("\n").find((candidate) => candidate.trim() !== "")?.trim() ?? subject.trim();
  const chars = [...line];
  return chars.length <= MAX_ACTIVITY_SUBJECT_CHARS
    ? line
    : `${chars.slice(0, MAX_ACTIVITY_SUBJECT_CHARS).join("")}…`;
}

/**
 * What one of the child's own updates says the child is doing.
 *
 * The pager derives the same label from the same events
 * (`format_activity_label`, `app/subagent.rs:891`), from a `TurnActivity` its
 * tracker folds out of the child's stream. Only the three states a client can
 * read straight off that stream are produced here; the pager's other arms
 * (compaction, retry, the several waiting reasons) come from state the agent
 * reports to the pager on paths a browser is not on, and inventing them from
 * chunk traffic would be exactly the guess this client refuses to make.
 *
 * `undefined` means "this update says nothing about activity" — the caller
 * leaves the previous label alone rather than blanking it.
 */
export function activityOf(update: SessionUpdate): string | undefined {
  switch (update.sessionUpdate) {
    case "agent_thought_chunk":
      return "Thinking";
    case "agent_message_chunk":
      return "Responding";
    case "tool_call":
    case "tool_call_update": {
      const status = update["status"];
      if (status !== undefined && status !== "pending" && status !== "in_progress") return undefined;
      const title = String(update["title"] ?? "");
      return title === "" ? "Running tool" : `Running: ${clampSubject(title)}`;
    }
    default:
      return undefined;
  }
}

/**
 * The terminal's order (`views/tasks_pane.rs:941`): running before done, then
 * agent type alphabetically, then newest first, then a stable id so equal rows
 * do not reshuffle.
 *
 * "Newest" is arrival order of the spawn, because that is the only *when* the
 * update stream carries — `subagent_spawned` has no timestamp. The pager reads
 * its own `Instant` at spawn, which is the same measurement from the same
 * event.
 */
export function sortRows(rows: readonly Subagent[]): Subagent[] {
  return [...rows].sort((a, b) => {
    const running = Number(b.status === "running") - Number(a.status === "running");
    if (running !== 0) return running;
    const byType = subagentLabel(a).label.localeCompare(subagentLabel(b).label);
    if (byType !== 0) return byType;
    if (a.seq !== b.seq) return b.seq - a.seq;
    return a.subagentId.localeCompare(b.subagentId);
  });
}


// ---------------------------------------------------------------------------
// The dock's Subagents section
// ---------------------------------------------------------------------------

/**
 * The children the dock's Subagents section lists (`panes.rs:306-337`).
 *
 * Running only, and workflow children excluded — a workflow run is its own
 * thing in the terminal and its steps are not loose subagents. Oldest first,
 * which the pager reads off `started_at`; the stream carries no spawn time, so
 * arrival order is the same measurement from the same event.
 *
 * Deliberately not `sortRows`: that is the *tasks pane*'s order (running first,
 * then type, then newest) and the pane still uses it. The dock has its own, and
 * a client that used one order in both places would be choosing where the
 * terminal did not.
 */
export function dockSubagents(rows: readonly Subagent[]): Subagent[] {
  return rows
    .filter((row) => row.status === "running" && row.workflowRunId === undefined)
    .sort((a, b) => a.seq - b.seq);
}

/**
 * One child as a dock row.
 *
 * `dock_subagent_rows` builds the meta as the model then the elapsed time, in
 * `fmt_elapsed`'s coarse form rather than the pane's `format_duration`. Either
 * half can be missing here and neither is invented: a child that has not ticked
 * yet has no elapsed time on the wire at all, and a spawn without a model said
 * nothing about one.
 */
export function subagentRailRow(row: Subagent, nowMs: number): RailRow {
  const named = subagentLabel(row);
  const ms = elapsedMs(row, nowMs);
  const meta = [row.model, ms === undefined ? undefined : fmtElapsed(ms / 1000)]
    .filter((part): part is string => part !== undefined)
    .join(" ");
  return {
    key: row.subagentId,
    kind: named.label,
    label: named.description,
    activity: row.activity,
    meta: meta === "" ? undefined : meta,
    running: true,
    killable: !row.pendingKill,
  };
}
