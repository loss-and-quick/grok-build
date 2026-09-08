// The dock's Tasks and Watchers, folded from the session's own stream.
//
// The claim this file exists to hold is that nothing was added to the wire for
// either section. Every update below is one the shell already sends and already
// persists, and the filters are the pager's own — so several of these tests read
// the Rust rather than restating it, the way `subagents.test.ts` pins the kill
// timeout.
import { describe, expect, test } from "bun:test";

import { fmtElapsed } from "../src/rail.ts";
import {
  MONITOR_PREFIX,
  createTasks,
  loopRailRow,
  runningMonitors,
  runningTasks,
  snapshotLabel,
  sortedLoops,
  taskElapsedMs,
  taskLabel,
  taskRailRow,
  watcherCount,
  watcherRailRows,
  backgroundWork,
  type Tasks,
} from "../src/tasks.ts";
import { epochMs, type SessionUpdate, type TaskSnapshot } from "../src/wire.ts";

const CRATES = new URL("../../../crates/codegen/", import.meta.url);
const rust = (path: string): Promise<string> => Bun.file(new URL(path, CRATES)).text();

const SESSION = "sess-1";

function backgrounded(over: Record<string, unknown> = {}): SessionUpdate {
  return {
    sessionUpdate: "task_backgrounded",
    tool_call_id: "tc-1",
    task_id: "t-1",
    command: "cargo test",
    cwd: "/repo",
    output_file: "/tmp/out",
    ...over,
  } as unknown as SessionUpdate;
}

function snapshot(over: Partial<TaskSnapshot> = {}): TaskSnapshot {
  return {
    task_id: "t-1",
    command: "cargo test",
    cwd: "/repo",
    start_time: { secs_since_epoch: 1_700_000_000, nanos_since_epoch: 0 },
    completed: false,
    kind: "bash",
    is_backgrounded: true,
    ...over,
  };
}

function loopCreated(over: Record<string, unknown> = {}): SessionUpdate {
  return {
    sessionUpdate: "scheduled_task_created",
    task_id: "loop-1",
    prompt: "check CI status",
    human_schedule: "every 5m",
    next_fire_at: null,
    ...over,
  } as unknown as SessionUpdate;
}

function live(store: Tasks, update: SessionUpdate): void {
  store.apply(update, false);
}

describe("which rows are Tasks and which are Watchers", () => {
  test("the split is the monitor flag, and it is the pager's own filter", async () => {
    // `dock_task_rows` takes running bg tasks with `!t.is_monitor`;
    // `dock_watcher_rows` takes the monitors and then the scheduled loops. If
    // those two filters ever stop being complementary the sections start
    // double-counting, which is why they are read here rather than restated.
    const panes = await rust("xai-grok-pager/src/app/agent_view/panes.rs");
    expect(panes).toContain("BgTaskStatus::Running && !t.is_monitor");
    expect(panes).toContain("BgTaskStatus::Running && t.is_monitor");

    const store = createTasks();
    live(store, backgrounded({ task_id: "run", command: "cargo test" }));
    live(store, backgrounded({ task_id: "mon", monitor_description: "watch the build" }));
    live(store, loopCreated());

    expect(runningTasks(store.tasks).map((t) => t.taskId)).toEqual(["run"]);
    expect(runningMonitors(store.tasks).map((t) => t.taskId)).toEqual(["mon"]);
    expect(watcherCount(store.tasks, store.loops)).toBe(2);
  });

  test("a monitor announced only by its command prefix is still a Watcher", async () => {
    // Reparented monitors and any backend predating `monitor_description` bake
    // `[monitor] ` into the command instead. A client reading the structured
    // field alone files those under Tasks and shows the prefix at a reader.
    const agent = await rust("xai-grok-pager/src/app/agent.rs");
    const declared = /pub const MONITOR_PREFIX: &str = "([^"]*)";/.exec(agent);
    expect(declared![1]).toBe(MONITOR_PREFIX);

    const store = createTasks();
    live(store, backgrounded({ task_id: "mon", command: `${MONITOR_PREFIX}watch the build` }));
    const [row] = runningMonitors(store.tasks);
    expect(row!.monitor).toBe(true);
    // And the prefix becomes the label rather than staying in the text.
    expect(taskLabel(row!)).toBe("watch the build");
  });

  test("a finished task leaves both sections", () => {
    const store = createTasks();
    live(store, backgrounded({ task_id: "run" }));
    expect(runningTasks(store.tasks)).toHaveLength(1);
    live(store, {
      sessionUpdate: "task_completed",
      task_snapshot: snapshot({ task_id: "run", completed: true }),
    } as unknown as SessionUpdate);
    expect(runningTasks(store.tasks)).toHaveLength(0);
  });

  test("Watchers is monitors first, then loops, each oldest first", () => {
    const store = createTasks();
    live(store, loopCreated({ task_id: "loop-a" }));
    live(store, backgrounded({ task_id: "mon-a", monitor_description: "a" }));
    live(store, backgrounded({ task_id: "mon-b", monitor_description: "b" }));
    live(store, loopCreated({ task_id: "loop-b" }));
    expect(watcherRailRows(store.tasks, store.loops, 0).map((row) => row.key)).toEqual([
      "mon-a",
      "mon-b",
      "loop-a",
      "loop-b",
    ]);
  });
});

describe("what a row is allowed to say about time", () => {
  test("a live announcement is a start time; a replayed one is not", () => {
    // `task_backgrounded` carries no timestamp. The pager stamps its own clock
    // on it either way (`acp_handler/background.rs:157`), which on a resume
    // dates every replayed task to the moment of resuming.
    const store = createTasks();
    store.apply(backgrounded({ task_id: "live" }), false);
    store.apply(backgrounded({ task_id: "replayed" }), true);
    const [liveRow, replayed] = store.tasks;
    expect(taskElapsedMs(liveRow!, Date.now())).toBeGreaterThanOrEqual(0);
    expect(taskElapsedMs(replayed!, Date.now())).toBeUndefined();
    expect(taskRailRow(replayed!, Date.now()).meta).toBe("—");
  });

  test("the snapshot fills a start time the replay could not carry", () => {
    const store = createTasks();
    store.apply(backgrounded({ task_id: "t-1" }), true);
    const started = { secs_since_epoch: 1_700_000_000, nanos_since_epoch: 0 };
    store.seed(SESSION, [snapshot({ task_id: "t-1", start_time: started })]);
    const [row] = store.tasks;
    expect(row!.startedAtEpochMs).toBe(epochMs(started));
    expect(taskElapsedMs(row!, epochMs(started)! + 74_000)).toBe(74_000);
  });

  test("the elapsed column is the dock's coarse form, not the pane's", async () => {
    // Two different formatters live side by side in the terminal and say
    // different things; using one for both would be this client deciding
    // something the terminal did not.
    const dock = await rust("xai-grok-pager/src/views/dock.rs");
    expect(dock).toContain('format!("{}m{:02}s", secs / 60, secs % 60)');
    expect(fmtElapsed(42)).toBe("42s");
    expect(fmtElapsed(59)).toBe("59s");
    expect(fmtElapsed(60)).toBe("1m00s");
    expect(fmtElapsed(134)).toBe("2m14s");
  });
});

describe("seeding from `x.ai/task/list`", () => {
  test("another session's tasks are not this session's dock", () => {
    const store = createTasks();
    store.seed(SESSION, [
      snapshot({ task_id: "mine", owner_session_id: SESSION }),
      snapshot({ task_id: "child", owner_session_id: "sess-child" }),
      snapshot({ task_id: "unowned", owner_session_id: null }),
    ]);
    expect(store.tasks.map((t) => t.taskId)).toEqual(["mine", "unowned"]);
  });

  test("a foreground command is not a dock row", () => {
    const store = createTasks();
    store.seed(SESSION, [snapshot({ task_id: "fg", is_backgrounded: false })]);
    expect(store.tasks).toHaveLength(0);
  });

  test("a completed snapshot lands as a finished row rather than a running one", () => {
    const store = createTasks();
    store.seed(SESSION, [snapshot({ task_id: "done", completed: true })]);
    expect(runningTasks(store.tasks)).toHaveLength(0);
  });

  test("a label equal to the command it labels is not a label", () => {
    // `display_command` differs from `command` for monitors and for
    // isolation-wrapped shells, and only then is it worth showing.
    expect(snapshotLabel(snapshot({ description: "run the suite" }))).toBe("run the suite");
    expect(
      snapshotLabel(snapshot({ command: "cargo test", display_command: "cargo test" })),
    ).toBeUndefined();
    expect(
      snapshotLabel(snapshot({ command: "sh -c x", display_command: `${MONITOR_PREFIX}watch` })),
    ).toBe("watch");
  });
});

describe("scheduled loops", () => {
  test("a fire for an unknown id makes the row rather than dropping it", () => {
    // The shell re-announces schedules when the scheduler restores
    // (`scheduler/actor.rs:777`), and a client that missed that announcement
    // would otherwise never show the loop at all.
    const store = createTasks();
    live(store, {
      sessionUpdate: "scheduled_task_fired",
      task_id: "loop-1",
      prompt: "check CI",
      human_schedule: "every 5m",
      next_fire_at: "2026-09-09T10:00:00Z",
    } as unknown as SessionUpdate);
    expect(sortedLoops(store.loops)).toHaveLength(1);
    expect(loopRailRow(store.loops[0]!).meta).toBe("every 5m");
  });

  test("a second announcement updates the row instead of doubling it", () => {
    const store = createTasks();
    live(store, loopCreated());
    live(store, loopCreated({ human_schedule: "every 10m" }));
    expect(store.loops).toHaveLength(1);
    expect(store.loops[0]!.humanSchedule).toBe("every 10m");
  });

  test("deleting removes it, and removing twice is not an error", () => {
    const store = createTasks();
    live(store, loopCreated());
    live(store, loopCreated({ task_id: "loop-2" }));
    live(store, {
      sessionUpdate: "scheduled_task_deleted",
      task_id: "loop-1",
      reason: "user",
    } as unknown as SessionUpdate);
    expect(store.loops.map((l) => l.taskId)).toEqual(["loop-2"]);
    store.removeLoop("loop-1");
    expect(store.loops.map((l) => l.taskId)).toEqual(["loop-2"]);
  });
});

describe("stopping", () => {
  test("a sent stop takes the action off the row until it is answered", () => {
    const store = createTasks();
    live(store, backgrounded({ task_id: "t-1" }));
    expect(taskRailRow(store.tasks[0]!, 0).killable).toBe(true);
    store.markKillSent("t-1", 1_000);
    expect(taskRailRow(store.tasks[0]!, 0).killable).toBe(false);
  });

  test("an unanswered stop is released on the pager's own timeout", async () => {
    const agent = await rust("xai-grok-pager/src/app/agent.rs");
    const secs = Number(/pub const PENDING_KILL_TIMEOUT_SECS: u64 = (\d+);/.exec(agent)![1]);
    const store = createTasks();
    live(store, backgrounded({ task_id: "t-1" }));
    store.markKillSent("t-1", 1_000);
    store.expireKills(1_000 + secs * 1000 - 1);
    expect(store.tasks[0]!.pendingKill).toBe(true);
    store.expireKills(1_000 + secs * 1000);
    expect(store.tasks[0]!.pendingKill).toBe(false);
  });

  test("a task the agent has no record of is dropped, not left with a button", () => {
    // `KillOutcome::NotFound` is a row replayed out of a session whose process
    // died with it; the pager removes it (`app/dispatch/turn.rs:799-807`).
    const store = createTasks();
    live(store, backgrounded({ task_id: "t-1" }));
    live(store, backgrounded({ task_id: "t-2" }));
    store.forget("t-1");
    expect(store.tasks.map((t) => t.taskId)).toEqual(["t-2"]);
    // The index the store keeps beside the rows has to follow the removal, or
    // the next edit writes to the wrong row.
    store.markKillSent("t-2", 1_000);
    expect(store.tasks[0]!.pendingKill).toBe(true);
  });
});

describe("the seam to the gateway", () => {
  test("a gateway that carries no background work draws no sections", () => {
    // The two sections are complete up to here and no further; `backgroundWork`
    // is where they stop. Until the gateway carries the store and the two
    // methods, this is `undefined` and both sections are absent — which is what
    // `dock.rs` does with a section that has nothing in it.
    expect(backgroundWork({ attached: () => ({}) })).toBeUndefined();
    expect(backgroundWork({ attached: () => null })).toBeUndefined();
    expect(backgroundWork({ attached: () => ({ tasks: createTasks() }) })).toBeUndefined();
  });

  test("and lights up whole, never by halves", () => {
    const tasks = createTasks();
    const wired = backgroundWork({
      attached: () => ({ tasks }),
      killTask: () => {},
      cancelScheduledLoop: () => {},
    });
    expect(wired?.tasks).toBe(tasks);
  });
});
