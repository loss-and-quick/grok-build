// The dock's Tasks and Watchers, folded from the session's own stream.
//
// Pure and DOM-free, like `subagents.ts` beside it, and built on the same
// discipline: every field is either something an update said or `undefined`
// because nothing has said it. The em dash a row draws for an unknown elapsed
// time is that rule showing through.
//
// ## Nothing was added to the wire for this
//
// The brief for this module said Tasks and Watchers "appear nowhere on the
// wire". They appear on all of it. What is missing is only the *aggregate* the
// pager builds in memory — there is no `x.ai/watchers/list`, and there does not
// need to be, because a watcher is not a thing the agent stores. It is a filter
// over two things it does:
//
//   - **Tasks** are the running background commands that are not monitors, and
//   - **Watchers** are the running monitors plus the scheduled `/loop` tasks
//
// (`app/agent_view/panes.rs:338-412`, and the counts at `:428-440` which must
// agree with them). Both halves of both sections arrive as ordinary session
// updates — `task_backgrounded` with its `monitor_description`,
// `task_completed`, `scheduled_task_created` / `_fired` / `_deleted` — and all
// of them are persisted and replayed by `session/load`, which is why the
// pager's own handlers read `_meta.isReplay` off them
// (`acp_handler/background.rs:92`, `:403-420`). So the pager and this client
// fold the same frames, and there is no second form for either to drift from.
//
// The one thing the stream does not carry is *when* a task started:
// `task_backgrounded` has no timestamp, so the pager dates a task from the
// moment its own process saw the frame (`background.rs:157`) — which, on a
// replay, is the moment of attaching. `x.ai/task/list` states it properly
// (`extensions/task.rs:394`), and this client asks, the way it asks
// `x.ai/subagent/list_running` for the same reason.
import { createStore, produce } from "solid-js/store";

import { fmtElapsed, type RailRow } from "./rail.ts";
import { PENDING_KILL_TIMEOUT_MS } from "./subagents.ts";
import {
  epochMs,
  type SessionUpdate,
  type SessionUpdateScheduledTaskCreated,
  type SessionUpdateScheduledTaskDeleted,
  type SessionUpdateScheduledTaskFired,
  type SessionUpdateTaskBackgrounded,
  type SessionUpdateTaskCompleted,
  type TaskSnapshot,
} from "./wire.ts";

/**
 * The prefix older backends bake into a monitor's command.
 *
 * The pager's `MONITOR_PREFIX` (`app/agent.rs:156`), and it is not decoration:
 * a reparented monitor and any backend predating the structured
 * `monitor_description` field announce themselves only this way, so a client
 * that reads the field alone files those rows under Tasks and bash-highlights
 * `[monitor] watch the build` at a reader (`acp_handler/background.rs:118-121`).
 */
export const MONITOR_PREFIX = "[monitor] ";

/** One backgrounded command: a Tasks row, or a Watchers row when it is a monitor. */
export interface BackgroundTask {
  taskId: string;
  /** Whether it belongs under Watchers rather than under Tasks. */
  monitor: boolean;
  /** The shell command, kept for the dialog even when a label is shown. */
  command: string;
  /** The model's label for the command; the row falls back to the command. */
  description?: string;
  cwd?: string;
  running: boolean;
  /**
   * The agent's clock, when anything has stated it.
   *
   * `undefined` for a task known only from a replayed `task_backgrounded`,
   * because that frame carries no time and the moment this browser attached is
   * not when the command started. `x.ai/task/list` fills it in.
   */
  startedAtEpochMs?: number;
  endedAtEpochMs?: number;
  /** Arrival order, which is the only ordering a stream-only task has. */
  seq: number;
  /** A kill was sent and no `task_completed` has arrived yet. */
  pendingKill: boolean;
  killRequestedAtMs?: number;
}

/** One scheduled `/loop`: always a Watchers row. */
export interface ScheduledLoop {
  taskId: string;
  /** The prompt the loop runs; `dock.rs` shows it as the row's description. */
  prompt: string;
  /** `every 5m`, and the row's whole right-hand column. */
  humanSchedule: string;
  nextFireAt?: string;
  seq: number;
}

export interface Tasks {
  readonly tasks: readonly BackgroundTask[];
  readonly loops: readonly ScheduledLoop[];
  /**
   * Fold one update. `isReplay` decides only whether "now" is a start time:
   * a live frame means the command started as it arrived, a replayed one means
   * nothing about when.
   */
  apply(update: SessionUpdate, isReplay: boolean): void;
  /** Seed from `x.ai/task/list`, for work that started before this client attached. */
  seed(sessionId: string, snapshots: readonly TaskSnapshot[]): void;
  /** Mark a kill as sent; cleared by the completion, or by {@link expireKills}. */
  markKillSent(taskId: string, nowMs: number): void;
  /** Drop the pending-kill mark: the kill failed, so the task may still be running. */
  clearKill(taskId: string): void;
  /** Drop marks the agent never answered, so the stop can be offered again. */
  expireKills(nowMs: number): void;
  /**
   * Drop a row the agent has no record of.
   *
   * `not_found` from `x.ai/task/kill` is a stale row — replayed from a session
   * whose process is long gone — and the pager removes it rather than leaving a
   * stop button over nothing (`app/dispatch/turn.rs:799-807`).
   */
  forget(taskId: string): void;
  /**
   * Remove a loop locally, before the agent confirms.
   *
   * The pager's own optimism (`app/dispatch/turn.rs:690`): deleting a schedule
   * is a decision, not a request, and the `scheduled_task_deleted` broadcast
   * that follows is a no-op on a row already gone.
   */
  removeLoop(taskId: string): void;
}

export function createTasks(): Tasks {
  const [tasks, setTasks] = createStore<BackgroundTask[]>([]);
  const [loops, setLoops] = createStore<ScheduledLoop[]>([]);
  const taskAt = new Map<string, number>();
  const loopAt = new Map<string, number>();

  const editTask = (taskId: string, change: (row: BackgroundTask) => void): boolean => {
    const index = taskAt.get(taskId);
    if (index === undefined) return false;
    setTasks(index, produce(change));
    return true;
  };

  const backgrounded = (update: SessionUpdateTaskBackgrounded, isReplay: boolean): void => {
    // A monitor says so with the structured field, or — reparented, or from a
    // backend that predates it — by the prefix on the command itself.
    const prefixed = update.command.startsWith(MONITOR_PREFIX)
      ? update.command.slice(MONITOR_PREFIX.length)
      : undefined;
    const monitor = nonEmpty(update.monitor_description) !== undefined || prefixed !== undefined;
    // The pager's order of preference, blank counting as absent so an empty
    // wire `description` cannot shadow a real one (`background.rs:131-140`).
    const description =
      nonEmpty(update.monitor_description) ??
      nonEmpty(prefixed) ??
      nonEmpty(update.description);
    const known = taskAt.get(update.task_id);
    if (known !== undefined) {
      // The completion can beat the backgrounding — a short command exits
      // before its own notification is sent — and the pager guards the same
      // race by never writing Running back over a terminal state (`:161-164`).
      setTasks(known, produce((row) => {
        row.monitor = monitor;
        row.command = update.command;
        row.description = description ?? row.description;
        row.cwd = update.cwd || row.cwd;
      }));
      return;
    }
    taskAt.set(update.task_id, tasks.length);
    setTasks(tasks.length, {
      taskId: update.task_id,
      monitor,
      command: update.command,
      description,
      cwd: update.cwd,
      running: true,
      // A live frame *is* the start; a replayed one says nothing about when.
      startedAtEpochMs: isReplay ? undefined : Date.now(),
      seq: tasks.length,
      pendingKill: false,
    });
  };

  const completed = (update: SessionUpdateTaskCompleted): void => {
    const snapshot = update.task_snapshot;
    if (!snapshot?.task_id) return;
    if (
      editTask(snapshot.task_id, (row) => {
        row.running = false;
        row.endedAtEpochMs = epochMs(snapshot.end_time) ?? Date.now();
        row.startedAtEpochMs = epochMs(snapshot.start_time) ?? row.startedAtEpochMs;
        row.pendingKill = false;
        row.killRequestedAtMs = undefined;
      })
    ) {
      return;
    }
    // A completion for a task this client never saw start. The pager records a
    // tombstone so the late `task_backgrounded` merges into it instead of
    // inserting a fresh Running row (`background.rs:670-684`); the same reason
    // applies here, and a finished row is filtered out of both sections anyway.
    insertSnapshot(snapshot, false);
  };

  const insertSnapshot = (snapshot: TaskSnapshot, running: boolean): void => {
    const monitor = snapshot.kind === "monitor" || snapshot.command.startsWith(MONITOR_PREFIX);
    taskAt.set(snapshot.task_id, tasks.length);
    setTasks(tasks.length, {
      taskId: snapshot.task_id,
      monitor,
      command: snapshot.command,
      description: snapshotLabel(snapshot),
      cwd: snapshot.cwd,
      running,
      startedAtEpochMs: epochMs(snapshot.start_time),
      endedAtEpochMs: epochMs(snapshot.end_time),
      seq: tasks.length,
      pendingKill: false,
    });
  };

  const scheduled = (
    update: SessionUpdateScheduledTaskCreated | SessionUpdateScheduledTaskFired,
  ): void => {
    const known = loopAt.get(update.task_id);
    if (known !== undefined) {
      setLoops(known, produce((row) => {
        row.prompt = update.prompt || row.prompt;
        row.humanSchedule = update.human_schedule || row.humanSchedule;
        row.nextFireAt = nonEmpty(update.next_fire_at) ?? row.nextFireAt;
      }));
      return;
    }
    loopAt.set(update.task_id, loops.length);
    setLoops(loops.length, {
      taskId: update.task_id,
      prompt: update.prompt,
      humanSchedule: update.human_schedule,
      nextFireAt: nonEmpty(update.next_fire_at),
      seq: loops.length,
    });
  };

  const dropLoop = (taskId: string): void => {
    const index = loopAt.get(taskId);
    if (index === undefined) return;
    setLoops((current) => current.filter((row) => row.taskId !== taskId));
    loopAt.delete(taskId);
    reindex(loops, loopAt, (row) => row.taskId);
  };

  return {
    tasks,
    loops,

    apply(update, isReplay) {
      switch (update.sessionUpdate) {
        case "task_backgrounded":
          backgrounded(update as unknown as SessionUpdateTaskBackgrounded, isReplay);
          return;
        case "task_completed":
          completed(update as unknown as SessionUpdateTaskCompleted);
          return;
        case "scheduled_task_created":
          scheduled(update as unknown as SessionUpdateScheduledTaskCreated);
          return;
        case "scheduled_task_fired":
          // A fire on an unknown id is the shell's restore re-announcement
          // having been missed, and the payload carries everything a row needs
          // — so it makes one, exactly as the pager does (`background.rs:377`).
          scheduled(update as unknown as SessionUpdateScheduledTaskFired);
          return;
        case "scheduled_task_deleted":
          dropLoop((update as unknown as SessionUpdateScheduledTaskDeleted).task_id);
          return;
        default:
          return;
      }
    },

    seed(sessionId, snapshots) {
      for (const snapshot of snapshots) {
        if (!snapshot?.task_id) continue;
        // Tasks are listed from the session's tool bridge, which a reparented
        // subagent can share. The dock shows the attached session's own work
        // (`panes.rs:338`), and `owner_session_id` is the field that says so;
        // its absence is an older shell, and an unowned row is this session's.
        const owner = nonEmpty(snapshot.owner_session_id);
        if (owner !== undefined && owner !== sessionId) continue;
        // A foreground command is not a dock row; only backgrounded work is.
        if (snapshot.is_backgrounded === false) continue;
        const running = snapshot.completed !== true;
        const known = taskAt.get(snapshot.task_id);
        if (known === undefined) {
          insertSnapshot(snapshot, running);
          continue;
        }
        // Known from a replayed frame that could not date it. The snapshot is
        // the agent's own clock, so it fills the gap rather than overwriting a
        // live measurement with a second one.
        setTasks(known, produce((row) => {
          row.startedAtEpochMs = epochMs(snapshot.start_time) ?? row.startedAtEpochMs;
          row.endedAtEpochMs = epochMs(snapshot.end_time) ?? row.endedAtEpochMs;
          row.running = running;
          row.description = row.description ?? snapshotLabel(snapshot);
        }));
      }
    },

    markKillSent(taskId, nowMs) {
      editTask(taskId, (row) => {
        row.pendingKill = true;
        row.killRequestedAtMs = nowMs;
      });
    },

    clearKill(taskId) {
      editTask(taskId, (row) => {
        row.pendingKill = false;
        row.killRequestedAtMs = undefined;
      });
    },

    expireKills(nowMs) {
      for (const row of tasks) {
        if (!row.pendingKill || row.killRequestedAtMs === undefined) continue;
        if (nowMs - row.killRequestedAtMs < PENDING_KILL_TIMEOUT_MS) continue;
        editTask(row.taskId, (target) => {
          target.pendingKill = false;
          target.killRequestedAtMs = undefined;
        });
      }
    },

    forget(taskId) {
      if (!taskAt.has(taskId)) return;
      setTasks((current) => current.filter((row) => row.taskId !== taskId));
      taskAt.delete(taskId);
      reindex(tasks, taskAt, (row) => row.taskId);
    },

    removeLoop: dropLoop,
  };
}

/** After a removal the store's indices have shifted; the map has to follow. */
function reindex<T>(rows: readonly T[], at: Map<string, number>, key: (row: T) => string): void {
  at.clear();
  rows.forEach((row, index) => at.set(key(row), index));
}

function nonEmpty(value: string | null | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

/**
 * A snapshot's label, the pager's way (`acp_handler/background.rs:648-666`).
 *
 * The model's `description`, else `display_command` — with the monitor prefix
 * stripped — and only when it actually differs from the command, because a
 * label equal to the thing it labels is not a label.
 */
export function snapshotLabel(snapshot: TaskSnapshot): string | undefined {
  const described = nonEmpty(snapshot.description);
  if (described !== undefined) return described;
  const display = nonEmpty(snapshot.display_command);
  if (display === undefined) return undefined;
  const bare = display.startsWith(MONITOR_PREFIX) ? display.slice(MONITOR_PREFIX.length) : display;
  return bare.trim() === snapshot.command.trim() ? undefined : nonEmpty(bare);
}

/** What a row says it is: the model's label, or the command it was given. */
export function taskLabel(task: BackgroundTask): string {
  return task.description ?? task.command;
}

/**
 * How long the task has been running, or `undefined` when nothing has said.
 *
 * A replayed task with no snapshot behind it has no start time at all, and gets
 * an em dash rather than a zero it did not earn — the rule `subagents.ts` sets
 * for every counter it draws.
 */
export function taskElapsedMs(task: BackgroundTask, nowMs: number): number | undefined {
  if (task.startedAtEpochMs === undefined) return undefined;
  return Math.max(0, (task.endedAtEpochMs ?? nowMs) - task.startedAtEpochMs);
}

/**
 * The Tasks section's rows: running background commands that are not monitors.
 *
 * `dock_task_rows` (`panes.rs:338-367`), including its sort — oldest first, by
 * start time, which here is arrival order for anything the wire did not date.
 */
export function runningTasks(tasks: readonly BackgroundTask[]): BackgroundTask[] {
  return tasks.filter((task) => task.running && !task.monitor).sort(byStart);
}

/** Running monitors: the first half of the Watchers section (`panes.rs:369-390`). */
export function runningMonitors(tasks: readonly BackgroundTask[]): BackgroundTask[] {
  return tasks.filter((task) => task.running && task.monitor).sort(byStart);
}

/** Scheduled loops, oldest first: the second half (`panes.rs:397-412`). */
export function sortedLoops(loops: readonly ScheduledLoop[]): ScheduledLoop[] {
  return [...loops].sort((a, b) => a.seq - b.seq);
}

/**
 * The Watchers count: monitors plus loops.
 *
 * A single function because `dock_counts` insists the count and the rows come
 * out of the same filter — "filters must match the `dock_*_rows` builders so
 * the item list and hit-testing line up with what render paints"
 * (`panes.rs:417-419`). Here the rows are the count.
 */
export function watcherCount(
  tasks: readonly BackgroundTask[],
  loops: readonly ScheduledLoop[],
): number {
  return runningMonitors(tasks).length + loops.length;
}

function byStart(a: BackgroundTask, b: BackgroundTask): number {
  const at = a.startedAtEpochMs;
  const bt = b.startedAtEpochMs;
  if (at !== undefined && bt !== undefined && at !== bt) return at - bt;
  return a.seq - b.seq;
}

// ---------------------------------------------------------------------------
// The dock's Tasks and Watchers sections
// ---------------------------------------------------------------------------

/**
 * One background command as a dock row.
 *
 * `dock_task_rows` and the monitor half of `dock_watcher_rows` are the same
 * builder with a different word for `kind` (`panes.rs:352-364`, `:380-388`), so
 * this is one function too. The em dash is the elapsed time of a task replayed
 * onto a client that arrived after it started and whose snapshot has not landed
 * — the terminal cannot say that at all, because it dates such a task from the
 * moment it saw the replay.
 */
export function taskRailRow(task: BackgroundTask, nowMs: number): RailRow {
  const ms = taskElapsedMs(task, nowMs);
  return {
    key: task.taskId,
    kind: task.monitor ? "Monitor" : "Run",
    label: taskLabel(task),
    meta: ms === undefined ? "—" : fmtElapsed(ms / 1000),
    running: task.running,
    killable: !task.pendingKill,
  };
}

/**
 * One scheduled loop as a dock row (`panes.rs:397-412`).
 *
 * The meta is the schedule, not an elapsed time — a loop is not running, it is
 * *due* — and the row is always killable because deleting a schedule needs
 * nothing from a process.
 */
export function loopRailRow(loop: ScheduledLoop): RailRow {
  return {
    key: loop.taskId,
    kind: "Loop",
    label: loop.prompt,
    meta: loop.humanSchedule,
    running: true,
    killable: true,
  };
}

/**
 * The Watchers section's rows: monitors first, then loops.
 *
 * One function because `dock_counts` insists the count and the rows come out of
 * the same filter — "filters must match the `dock_*_rows` builders so the item
 * list and hit-testing line up with what render paints" (`panes.rs:417-419`).
 * Here the rows *are* the count, so the two cannot disagree.
 */
export function watcherRailRows(
  tasks: readonly BackgroundTask[],
  loops: readonly ScheduledLoop[],
  nowMs: number,
): RailRow[] {
  return [
    ...runningMonitors(tasks).map((task) => taskRailRow(task, nowMs)),
    ...sortedLoops(loops).map(loopRailRow),
  ];
}

/** The Tasks section's rows (`panes.rs:338-367`). */
export function taskRailRows(tasks: readonly BackgroundTask[], nowMs: number): RailRow[] {
  return runningTasks(tasks).map((task) => taskRailRow(task, nowMs));
}

// ---------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------

/**
 * What the rail's Tasks and Watchers sections need from the gateway.
 *
 * **These sections are wired up to here and no further.** Everything above is
 * complete — the fold, the filters, the sort, the labels, the kill discipline —
 * but a fold needs frames, and the only place a frame arrives is the gateway's
 * notification dispatch. `gateway.ts` belongs to another line of work in this
 * tree, and a merge conflict in it would cost more than this wiring saves, so
 * the three lines it needs are not written there. They are:
 *
 * 1. `tasks: createTasks()` in the object `attach` builds, beside `subagents`;
 * 2. `current.tasks.apply(update, meta.isReplay)` in the dispatch, beside
 *    `current.subagents.apply(...)` — `isReplay` because a live
 *    `task_backgrounded` means the command started as it was announced and a
 *    replayed one says nothing about when;
 * 3. `killTask` and `cancelScheduledLoop`, which are `x.ai/task/kill` and
 *    `x.ai/scheduler/delete` — the two methods the pager already calls
 *    (`app/effects/mod.rs:1743`, `:1805`).
 *
 * Optionally a fourth: seeding from `x.ai/task/list` on attach, the way the
 * fan-out seeds from `x.ai/subagent/list_running`, which is the only way to
 * date a task the replay could not. Without it those rows show an em dash for
 * elapsed, which is true rather than merely tolerable.
 *
 * Until then this returns `undefined` and both sections are simply absent,
 * which is `dock.rs`'s own rule for a section with nothing in it — so nothing
 * on screen is wrong in the meantime, and the day the field appears they light
 * up with no further change here.
 */
export interface BackgroundWork {
  tasks: Tasks;
  killTask(taskId: string): void;
  cancelScheduledLoop(taskId: string): void;
}

/** The shape {@link backgroundWork} looks for, as a gateway that has it would have it. */
interface MaybeWired {
  attached?: () => { tasks?: Tasks } | null | undefined;
  killTask?: (taskId: string) => unknown;
  cancelScheduledLoop?: (taskId: string) => unknown;
}

/**
 * The background work this gateway carries, or `undefined` when it carries none.
 *
 * Structural rather than typed against `Gateway`, for two reasons: `gateway.ts`
 * imports this module, so naming its type here would be a cycle; and the point
 * of the check is precisely that the field may not be there yet.
 */
export function backgroundWork(gateway: unknown): BackgroundWork | undefined {
  const wired = gateway as MaybeWired;
  const store = wired.attached?.()?.tasks;
  if (
    store === undefined ||
    typeof wired.killTask !== "function" ||
    typeof wired.cancelScheduledLoop !== "function"
  ) {
    return undefined;
  }
  return {
    tasks: store,
    killTask: (taskId) => void wired.killTask?.(taskId),
    cancelScheduledLoop: (taskId) => void wired.cancelScheduledLoop?.(taskId),
  };
}
