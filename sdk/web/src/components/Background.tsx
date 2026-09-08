import { For, Show, type JSX } from "solid-js";

import { createTick } from "../animation.ts";
import type { Gateway } from "../gateway.ts";
import { fmtElapsed } from "../rail.ts";
import {
  backgroundWork,
  runningMonitors,
  runningTasks,
  sortedLoops,
  taskElapsedMs,
  taskLabel,
  type BackgroundTask,
  type ScheduledLoop,
} from "../tasks.ts";
import { StopButton } from "./StopButton.tsx";

/**
 * Tasks and Watchers, whole.
 *
 * What the rail row leaves out. A dock row is one line — a kind, a label and a
 * time — because the terminal has one line to spend; the label it shows is the
 * model's description of the command, not the command, and the working
 * directory is nowhere on it at all. Both are what a person asks for the moment
 * a row is worth stopping, so this is where they are.
 *
 * It is also where the rows past `MAX_SECTION_ROWS` are. The rail says "2 more"
 * and this is the page the count refers to, which is the same arrangement the
 * context widget and a plugin panel already use.
 */
export function BackgroundPane(props: {
  gateway: Gateway;
  section: "tasks" | "watchers";
}): JSX.Element {
  const tick = createTick();
  const now = (): number => {
    void tick();
    return Date.now();
  };

  // The same seam the rail reads through; see `backgroundWork`.
  const work = () => backgroundWork(props.gateway);

  const tasks = (): BackgroundTask[] => {
    const held = work();
    if (!held) return [];
    return props.section === "tasks"
      ? runningTasks(held.tasks.tasks)
      : runningMonitors(held.tasks.tasks);
  };

  const loops = (): ScheduledLoop[] => {
    const held = work();
    return held && props.section === "watchers" ? sortedLoops(held.tasks.loops) : [];
  };

  return (
    <div class="background-pane">
      <Show when={tasks().length + loops().length === 0}>
        <p class="background-empty">Nothing running.</p>
      </Show>
      <ul class="background-rows">
        <For each={tasks()}>
          {(task) => (
            <li class="background-row">
              <div class="background-line">
                <span class="background-kind">{task.monitor ? "Monitor" : "Run"}</span>
                <span class="background-label">{taskLabel(task)}</span>
                <span class="background-meta">{elapsed(task, now())}</span>
                <StopButton
                  tick={tick}
                  subject={task.taskId}
                  pending={task.pendingKill}
                  title={
                    task.monitor
                      ? "Kill this monitor. The agent stops being told what it was watching."
                      : "Kill this background command. Whatever it had not finished is not finished."
                  }
                  onConfirm={() => work()?.killTask(task.taskId)}
                />
              </div>
              {/* The command itself, always — the row above shows the model's
                  label for it whenever there is one, and a label is not a thing
                  you can check before killing a process. */}
              <pre class="background-command">{task.command}</pre>
              <Show when={task.cwd}>{(cwd) => <div class="background-cwd">{cwd()}</div>}</Show>
            </li>
          )}
        </For>
        <For each={loops()}>
          {(loop) => (
            <li class="background-row">
              <div class="background-line">
                <span class="background-kind">Loop</span>
                <span class="background-label">{loop.prompt}</span>
                <span class="background-meta">{loop.humanSchedule}</span>
                <StopButton
                  tick={tick}
                  subject={loop.taskId}
                  verb="remove"
                  pendingLabel="removing…"
                  title="Delete this schedule. It will not run again."
                  onConfirm={() => work()?.cancelScheduledLoop(loop.taskId)}
                />
              </div>
              {/* Only when the agent has said one. A schedule with no next fire
                  time on the wire is not a schedule that has stopped; it is one
                  this client has not been told the next time for. */}
              <Show when={loop.nextFireAt}>
                {(at) => <div class="background-cwd">next: {at()}</div>}
              </Show>
            </li>
          )}
        </For>
      </ul>
    </div>
  );
}

/** The dock's coarse elapsed, or an em dash where nothing has said. */
function elapsed(task: BackgroundTask, nowMs: number): string {
  const ms = taskElapsedMs(task, nowMs);
  return ms === undefined ? "—" : fmtElapsed(ms / 1000);
}
