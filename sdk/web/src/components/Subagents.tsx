import { For, Show, createEffect, createSignal, untrack, type JSX } from "solid-js";

import { blendToward, createTick, waveBrightness } from "../animation.ts";
import type { Gateway } from "../gateway.ts";
import {
  ACCENT_BAR,
  BALLOT_X,
  CHECK_MARK,
  DISCLOSURE_CLOSED,
  DISCLOSURE_OPEN,
  spinnerFrame,
} from "../glyphs.ts";
import {
  contextBadge,
  elapsedMs,
  formatDuration,
  metaSuffix,
  sortRows,
  subagentLabel,
  type Subagent,
} from "../subagents.ts";
import { StopButton } from "./StopButton.tsx";

/**
 * The fan-out, whole.
 *
 * Its own region rather than rows inside the transcript, because a fan-out is
 * *current state*, not an event: three children each rewrite their own line
 * several times a minute, and interleaving that with a streaming reply would
 * scroll the thing you are watching off the screen. The pager reached the same
 * place from the other direction — it writes one scrollback line per child and
 * then keeps the live view in a docked pane (`views/tasks_pane.rs`).
 *
 * **Two places draw this, and never at once.** With the rail on it is what the
 * Subagents section's **Open** raises, which is the terminal's own split: the
 * dock carries a one-line row per child and Enter on it opens the child
 * (`panes.rs:464-472`). With the rail off there is no column to carry that row,
 * so this goes back above the transcript, where every panel goes when the rail
 * is off.
 *
 * Everything drawn here is something the wire said. A counter the wire has not
 * carried yet renders as an em dash, never as a zero: "no tool calls" and "the
 * agent has not told this client yet" are different facts, and a browser that
 * prints `0` for the second one is inventing the first.
 */
export function Subagents(props: { gateway: Gateway }): JSX.Element {
  const tick = createTick();
  const [showDone, setShowDone] = createSignal(false);

  // A stop the agent never answered stops marking the row, so it can be offered
  // again — the sweep the pager runs in its own render pass
  // (`agent_view/render.rs:1279-1285`). The rail runs the same one; either
  // surface being on screen is enough, and running both is harmless.
  createEffect(() => {
    void tick();
    const current = props.gateway.attached();
    if (!current) return;
    untrack(() => current.subagents.expireKills(performance.now()));
  });

  const all = (): readonly Subagent[] => props.gateway.attached()?.subagents.rows ?? [];
  const running = (): number => all().filter((row) => row.status === "running").length;
  const done = (): number => all().length - running();
  // `show_done` is the pager's own default: a fan-out you are watching is the
  // running rows, and the finished ones are one keypress away (`tasks_pane.rs:900`).
  const shown = (): Subagent[] =>
    sortRows(all().filter((row) => showDone() || row.status === "running"));

  return (
    <Show when={all().length > 0}>
      <section class="subagents">
        <header class="subagents-header">
          <h2 class="subagents-title">
            Subagents — {running()} running, {done()} done
          </h2>
          <button
            class="subagents-toggle"
            type="button"
            onClick={() => setShowDone(!showDone())}
          >
            {showDone() ? "hide finished" : "show finished"}
          </button>
        </header>
        <ul class="subagent-rows">
          <For each={shown()}>
            {(row) => <Row row={row} tick={tick} gateway={props.gateway} />}
          </For>
        </ul>
      </section>
    </Show>
  );
}

function Row(props: { row: Subagent; tick: () => number; gateway: Gateway }): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const label = () => subagentLabel(props.row);
  const running = () => props.row.status === "running";

  return (
    <li class={`subagent subagent-${props.row.status}`} style={{ "--depth": props.row.depth }}>
      <span
        class="subagent-rail"
        aria-hidden="true"
        style={
          running()
            ? {
                color: blendToward(
                  "var(--grok-bg-base)",
                  "var(--grok-accent-running)",
                  waveBrightness(props.tick()),
                ),
              }
            : undefined
        }
      >
        {ACCENT_BAR}
      </span>
      <span class="subagent-mark" aria-hidden="true">
        {statusGlyph(props.row, props.tick())}
      </span>

      <div class="subagent-body">
        <div class="subagent-line">
          <button class="subagent-name" type="button" onClick={() => setOpen(!open())}>
            <span class="subagent-disclosure" aria-hidden="true">
              {open() ? DISCLOSURE_OPEN : DISCLOSURE_CLOSED}
            </span>
            {label().label}
          </button>
          <span class="subagent-meta">{metaSuffix(props.row)}</span>
          <Show when={contextBadge(props.row)}>
            {(badge) => <span class="subagent-badge">{badge()}</span>}
          </Show>
          <Show when={props.row.capabilityMode}>
            {(mode) => <span class="subagent-badge">{mode()}</span>}
          </Show>
          <span class="subagent-status">{props.row.status}</span>
        </div>

        <div class="subagent-desc">{label().description}</div>

        <Show when={running()}>
          <div class="subagent-activity">
            {/* The child has been spawned and has not said anything this
                client can read yet. It is not idle and it is not thinking;
                nothing on the wire says which, so neither does this. */}
            {props.row.activity ?? "unknown"}
          </div>
        </Show>

        <div class="subagent-chips">
          <Chip name="elapsed" value={duration(props.row, props.tick())} />
          <Chip name="turns" value={count(props.row.turns)} />
          <Chip name="tools" value={count(props.row.toolCalls)} />
          <Chip name="tokens" value={count(props.row.tokensUsed)} />
          <Chip name="context" value={percent(props.row.contextUsagePct)} />
          <Chip name="errors" value={count(props.row.errorCount)} />
        </div>

        <Show when={open()}>
          <dl class="subagent-detail">
            <dt>id</dt>
            <dd>{props.row.subagentId}</dd>
            <dt>session</dt>
            <dd>{props.row.childSessionId}</dd>
            <dt>type</dt>
            <dd>{props.row.subagentType}</dd>
            <dt>tools used</dt>
            <dd>{props.row.toolsUsed?.join(", ") || "unknown"}</dd>
            <Show when={props.row.resumedFrom}>
              {(from) => (
                <>
                  <dt>resumed from</dt>
                  <dd>{from()}</dd>
                </>
              )}
            </Show>
          </dl>
          {/* The child's answer, behind the same disclosure. A finished
              child's whole reply is several paragraphs, and a fan-out of six
              of them stops being a glance. What it ended as is on the row
              itself; what it said is one click away. */}
          <Show when={props.row.output}>
            {(output) => <pre class="subagent-output">{output()}</pre>}
          </Show>
        </Show>

        {/* A failure's reason is not optional detail: a row that says only
            "failed" sends the reader back to the terminal. */}
        <Show when={props.row.error}>
          {(error) => <pre class="subagent-error">{error()}</pre>}
        </Show>
      </div>

      {/* The same button the rail row carries, so the two surfaces cannot come
          to disagree about what a stop costs or how many presses it takes. */}
      <Show when={running()}>
        <StopButton
          tick={props.tick}
          subject={props.row.subagentId}
          pending={props.row.pendingKill}
          title="Stop this subagent. Its turn is cancelled where it stands and nothing is handed back."
          onConfirm={() => void props.gateway.cancelSubagent(props.row.subagentId)}
        />
      </Show>
    </li>
  );
}

function Chip(props: { name: string; value: string }): JSX.Element {
  return (
    <span class="subagent-chip">
      <span class="subagent-chip-name">{props.name}</span>
      <span class="subagent-chip-value">{props.value}</span>
    </span>
  );
}

/** The pager's marks: a spinner while it runs, then done or failed. */
function statusGlyph(row: Subagent, tick: number): string {
  if (row.status === "running") return spinnerFrame(tick);
  return row.status === "completed" ? CHECK_MARK : BALLOT_X;
}

function duration(row: Subagent, tick: number): string {
  void tick;
  const ms = elapsedMs(row, performance.now());
  return ms === undefined ? "—" : formatDuration(ms);
}

function count(value: number | undefined): string {
  return value === undefined ? "—" : String(value);
}

function percent(value: number | undefined): string {
  return value === undefined ? "—" : `${value}%`;
}
