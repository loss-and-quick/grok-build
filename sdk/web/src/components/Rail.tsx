import {
  ErrorBoundary,
  For,
  Show,
  createEffect,
  createSignal,
  untrack,
  type Accessor,
  type JSX,
} from "solid-js";

import { createTick } from "../animation.ts";
import { usageChip } from "../context.ts";
import type { Gateway } from "../gateway.ts";
import type { PanelAction } from "../panel.ts";
import {
  itemKey,
  moveCursor,
  railItems,
  visualRows,
  type RailRow,
  type RailSection,
} from "../rail.ts";
import { dockSubagents, subagentRailRow } from "../subagents.ts";
import {
  backgroundWork,
  taskRailRows,
  watcherRailRows,
  type BackgroundWork,
} from "../tasks.ts";
import type { QueueRow } from "../queue.ts";
import type { PanelEntry } from "../transcript.ts";
import type { ContextFacts } from "../wire.ts";
import { ContextWidget } from "./ContextWidget.tsx";
import { Overlay } from "./Overlay.tsx";
import { Panel, createFields, type Fields } from "./Panel.tsx";
import { BackgroundPane } from "./Background.tsx";
import { Queue } from "./Queue.tsx";
import { RailList } from "./RailList.tsx";
import { StopButton } from "./StopButton.tsx";
import { Subagents } from "./Subagents.tsx";
import { Widget } from "./Widget.tsx";

/**
 * The context widget's section key.
 *
 * A fixed string rather than a generated one, and it cannot collide with a
 * plugin's: those are `panel:…`, percent-encoded, and a plugin does not get to
 * choose the prefix.
 */
const CONTEXT_KEY = "context";

/**
 * The dock's own three, in the dock's own order.
 *
 * `dock.rs:60-67` lists Subagents, Tasks, Watchers, Queued and paints them in
 * that order; Queued belongs to the prompt queue and arrives with it. The keys
 * are fixed strings for the same reason `CONTEXT_KEY` is, and a plugin cannot
 * collide with them.
 */
const SUBAGENTS_KEY = "subagents";
const TASKS_KEY = "tasks";
const WATCHERS_KEY = "watchers";
const QUEUED_KEY = "queued";

/**
 * The widget rail.
 *
 * The plugin protocol has promised this since it was written: a published
 * `PanelViewModel` is rendered "both as a full-screen overlay … and as a compact
 * sidebar widget" (`plugin-protocol/src/lib.rs`). The overlay exists in the
 * terminal; the sidebar never did, in any client — the pager's second surface is
 * a one-line chip in the status bar. So this is the first build of a surface the
 * product designed and did not have, and its shape is taken from the place the
 * product did work it out: `views/dock.rs`, the Figma "Exploration" layout.
 *
 * Three of the dock's four sections are here — Subagents, Tasks, Watchers —
 * and none of them needed anything added to the wire. Tasks and Watchers are
 * not stored things the agent could be asked for; they are two filters over
 * background commands and scheduled loops (`panes.rs:338-412`), and every frame
 * either filter reads has been on the session's own stream all along. Queued is
 * the fourth and arrives with the prompt queue.
 *
 * **Order is fixed and not configurable.** The pager keeps its panels in an
 * `IndexMap` and removes with `shift_remove`, so publication order survives a
 * close, and a browser reading the same map gets that order for nothing.
 * Dragging widgets about would be a client-only preference with no terminal
 * counterpart and no keyboard story, so there is none.
 */
export function Rail(props: { gateway: Gateway }): JSX.Element {
  // Which sections are folded shut. Held here rather than in storage: the
  // rail's persistence belongs to an instance, and this client has no instance
  // identity yet — inventing a key now would mean migrating it the moment
  // there is one.
  const [collapsed, setCollapsed] = createSignal<ReadonlySet<string>>(new Set());
  const [opened, setOpened] = createSignal<string | null>(null);

  const panels = (): PanelEntry[] =>
    Object.values(props.gateway.attached()?.transcript.panels ?? {});

  /**
   * A section key that is also a DOM id and an attribute selector.
   *
   * Percent-encoded for the reason `panelKey` joins with NUL: every printable
   * character is legal in a plugin name and in a plugin-local panel id, so a
   * key built by concatenation can be forged into a collision — and this one
   * has to survive being written into markup as well.
   */
  const sectionKey = (panel: PanelEntry): string =>
    `panel:${encodeURIComponent(panel.plugin)}:${encodeURIComponent(panel.viewModel.id)}`;

  /**
   * The resolved context window, or `null` while nobody has asked yet.
   *
   * Nothing is derived from it here. `x.ai/session/info` carries the whole
   * `/context` picture already resolved, which is the reason this section could
   * be built at all: the numbers used to be a pure function inside the pager,
   * and a second client could only have reimplemented them.
   */
  const facts = (): ContextFacts | null => props.gateway.sessionInfo()?.contextFacts ?? null;

  /**
   * The wall clock every elapsed time on the rail is read against.
   *
   * One signal for the whole column, so three sections of rows count in step
   * rather than each drifting by however long its own timer has been alive —
   * the reason the pager animates everything off a single frame counter.
   */
  const tick = createTick();

  /**
   * Two clocks, because the two things being timed were measured on two.
   *
   * A child's elapsed time is the agent's own `duration_ms` plus the wait since
   * that frame arrived, and that wait is measured with `performance.now()`
   * (`subagents.ts`). A background task's is the difference between now and a
   * start time the agent stated as an epoch. Reading either against the other's
   * clock produces a number in the tens of millions of minutes, which is what
   * it did.
   */
  const sinceFrame = (): number => {
    void tick();
    return performance.now();
  };
  const wallNow = (): number => {
    void tick();
    return Date.now();
  };

  /**
   * The two sections that are wired only as far as this client's own half.
   *
   * `undefined` while the gateway carries no background work — see
   * {@link backgroundWork}, which says exactly what it would take — and both
   * sections are then empty, which `dock.rs` draws as nothing at all.
   */
  const work = (): BackgroundWork | undefined => backgroundWork(props.gateway);

  /**
   * Stops that were sent and never answered, released on the shared tick.
   *
   * The terminal does exactly this and in the same place — its render pass
   * clears `pending_kill` once `PENDING_KILL_TIMEOUT_SECS` has passed, for
   * background tasks and subagents alike (`agent_view/render.rs:1267-1285`) —
   * because a stop whose reply was lost otherwise leaves a row marked
   * "stopping…" with no way back. `untrack` keeps the sweep from being its own
   * trigger: it writes to the same stores it reads.
   */
  createEffect(() => {
    void tick();
    const current = props.gateway.attached();
    if (!current) return;
    untrack(() => {
      const at = performance.now();
      current.subagents.expireKills(at);
      work()?.tasks.expireKills(at);
    });
  });

  /** Running children, as the dock lists them. */
  const subagentRows = (): RailRow[] => {
    const at = sinceFrame();
    const current = props.gateway.attached();
    if (!current) return [];
    return dockSubagents(current.subagents.rows).map((row) => subagentRailRow(row, at));
  };

  /** Running background commands that are not monitors. */
  const taskRows = (): RailRow[] => {
    const at = wallNow();
    const held = work();
    return held ? taskRailRows(held.tasks.tasks, at) : [];
  };

  /**
   * The prompts this session is holding but has not started.
   *
   * Read for its length only. The queue is drawn by its own component, which is
   * what "embeds the queue pane as its body" means here — the rail contributes
   * the header and the count, and nothing about the rows is the rail's business.
   */
  const queued = (): readonly QueueRow[] => props.gateway.attached()?.queue.rows ?? [];

  /** Running monitors, then scheduled loops. */
  const watcherRows = (): RailRow[] => {
    const at = wallNow();
    const held = work();
    return held ? watcherRailRows(held.tasks.tasks, held.tasks.loops, at) : [];
  };

  /**
   * Every section, in the order they are drawn.
   *
   * **Built-ins above plugins, and the order fixed.** Publishing a panel is a
   * plugin asking for the space, not taking it, so nothing a plugin does can
   * push the context window down the column. The render below walks the same
   * order; this list is what the keyboard walks.
   *
   * The dock's three sit between the context window and the plugins, in the
   * dock's own order. The context window is above them because it is the one
   * section that is always there: the other three obey `dock.rs`'s emptiness
   * rule and are absent whenever nothing is running, so putting them first
   * would move the permanent thing every time a subagent spawned.
   */
  const sections = (): RailSection[] => {
    const all: RailSection[] = [];
    const context = facts();
    if (context) {
      all.push({ kind: "widget", key: CONTEXT_KEY, label: "Context", note: usageChip(context) });
    }
    all.push({ kind: "list", key: SUBAGENTS_KEY, label: "Subagents", rows: subagentRows() });
    all.push({ kind: "list", key: TASKS_KEY, label: "Tasks", rows: taskRows() });
    all.push({ kind: "list", key: WATCHERS_KEY, label: "Watchers", rows: watcherRows() });
    // The dock's fourth, and the one that is not shaped like the other three.
    // It contributes no rows to the walk: `DockData::rows(Queued)` is `&[]`,
    // the section has no expanded flag, and `visual_rows` pushes its header
    // alone — "the Queued section embeds the queue pane as its body". So it is
    // a widget with a count, and the count is what makes it appear and go away
    // exactly as `dock.rs`'s emptiness rule says a counted section should.
    all.push({ kind: "widget", key: QUEUED_KEY, label: "Queued", count: queued().length });
    for (const panel of panels()) {
      all.push({
        kind: "panel",
        key: sectionKey(panel),
        label: panel.viewModel.title,
        source: panel.plugin,
      });
    }
    return all;
  };

  const isOpen = (key: string): boolean => !collapsed().has(key);
  const toggle = (key: string): void => {
    const next = new Set(collapsed());
    if (!next.delete(key)) next.add(key);
    setCollapsed(next);
  };

  const openedPanel = (): PanelEntry | undefined =>
    panels().find((panel) => sectionKey(panel) === opened());

  const act = (panel: PanelEntry) => (action: PanelAction) =>
    void props.gateway.panelAction(panel.plugin, action);

  /** Whether a Watchers row is a schedule rather than a running process. */
  const isLoop = (key: string): boolean =>
    work()?.tasks.loops.some((loop) => loop.taskId === key) === true;

  /**
   * Stop what a Watchers row names.
   *
   * Two different actions behind one button, resolved by looking the row up
   * rather than by reading the word the row happens to display: the terminal
   * splits the same way, on a `DockWatcherId` that remembers which of the two
   * kinds the row came from (`panes.rs:9-15`, `:515-522`).
   */
  const stopWatcher = (key: string): void => {
    const held = work();
    if (!held) return;
    if (isLoop(key)) {
      held.cancelScheduledLoop(key);
      return;
    }
    held.killTask(key);
  };

  /**
   * One list section, drawn from the same row array the walk was given.
   *
   * `dock.rs`'s emptiness rule is the `Show`: a section with a zero count is
   * not drawn, and `sectionShown` already tells the walk the same thing, so the
   * cursor never has an item the screen does not.
   */
  const listSection = (
    key: string,
    label: string,
    rows: () => RailRow[],
    action: (row: Accessor<RailRow>) => JSX.Element,
  ): JSX.Element => (
    <Show when={rows().length > 0}>
      <Widget
        itemKey={`h:${key}`}
        label={label}
        count={rows().length}
        open={isOpen(key)}
        onToggle={() => toggle(key)}
        onOpenFully={() => setOpened(key)}
      >
        <RailList section={key} rows={rows()} action={action} />
      </Widget>
    </Show>
  );

  /**
   * One editor per panel, not one per surface.
   *
   * A panel here has two places it can be drawn — the widget and the dialog its
   * **Open** button raises — and the terminal has one `LineEditor` per input id
   * regardless of what is drawing it. Sharing the map keeps that true: a code
   * typed into the widget is still there in the dialog, and a panel closed by
   * its plugin takes its editors with it.
   */
  const editors = new Map<string, Fields>();
  const fieldsFor = (key: string): Fields => {
    const existing = editors.get(key);
    if (existing) return existing;
    const made = createFields();
    editors.set(key, made);
    return made;
  };
  createEffect(() => {
    const live = new Set(sections().map((section) => section.key));
    for (const key of [...editors.keys()]) if (!live.has(key)) editors.delete(key);
  });

  /**
   * Arrows walk the sections, and Tab still does what Tab does.
   *
   * The walk is the pager's: one sequence over headers *and* rows, so Down from
   * a section's last row lands on the next section's header rather than jumping
   * over it (`dock.rs:189-201`), and the cursor clamps at the ends instead of
   * wrapping. What is deliberately not ported is a roving `tabindex` over the
   * whole rail: the terminal's dock has no interactive bodies and this one does,
   * and a plugin's text field that Tab cannot reach is a field nobody can fill.
   */
  const onKeyDown = (event: KeyboardEvent): void => {
    const target = event.target;
    if (!(target instanceof HTMLElement)) return;
    const from = target.getAttribute("data-rail-item");
    if (from === null) return;
    const items = railItems(sections(), collapsed());
    const at = items.findIndex((item) => itemKey(item) === from);
    if (at < 0) return;
    const step = STEP[event.key];
    if (step === undefined) return;
    event.preventDefault();
    const next = items[moveCursor(items.length, at, step)];
    if (!next) return;
    const selector = `[data-rail-item="${itemKey(next)}"]`;
    (event.currentTarget as HTMLElement).querySelector<HTMLElement>(selector)?.focus();
  };

  return (
    <Show when={visualRows(sections(), collapsed()).length > 0}>
      {/* A region, not a dialog: the rail is beside the session, not over it,
          and nothing about it is modal. A rail with no sections renders nothing
          at all — `dock.rs`'s rule, and what keeps the third column from being
          a permanent strip of empty. */}
      <aside class="rail" role="region" aria-label="Session widgets" onKeyDown={onKeyDown}>
        {/* First in the column, and outside the `<For>` below it on purpose.
            Solid keys a `<For>` by reference, and a panel entry is a stable
            object that survives a republish — building one array of freshly
            made section objects for both the walk and the render would remount
            every widget whenever the context window moved, which is the DOM
            rebuild `Panel.tsx` exists to prevent. So the walk gets the objects
            and the render gets the entries, in the same order. */}
        <Show when={facts()}>
          {(context) => (
            <Widget
              itemKey={`h:${CONTEXT_KEY}`}
              label="Context"
              note={usageChip(context())}
              open={isOpen(CONTEXT_KEY)}
              onToggle={() => toggle(CONTEXT_KEY)}
              onOpenFully={() => setOpened(CONTEXT_KEY)}
            >
              <ContextWidget
                facts={context()}
                model={props.gateway.sessionInfo()?.modelDisplayName ?? props.gateway.sessionInfo()?.model}
                onRefresh={() => void props.gateway.refreshSessionInfo()}
              />
            </Widget>
          )}
        </Show>
        {/* The dock's own three, in the dock's order. Each is a plain call
            rather than a component so the rows the walk was handed and the rows
            drawn here are the same array, which is `dock.rs`'s one-walk rule
            expressed the only way a browser can express it. */}
        {listSection(SUBAGENTS_KEY, "Subagents", subagentRows, (row) => (
          <StopButton
            tick={tick}
            subject={row().key}
            pending={row().killable === false}
            title="Stop this subagent. Its turn is cancelled where it stands and nothing is handed back."
            onConfirm={() => void props.gateway.cancelSubagent(row().key)}
          />
        ))}
        {listSection(TASKS_KEY, "Tasks", taskRows, (row) => (
          <StopButton
            tick={tick}
            subject={row().key}
            pending={row().killable === false}
            title="Kill this background command. Whatever it had not finished is not finished."
            onConfirm={() => work()?.killTask(row().key)}
          />
        ))}
        {listSection(WATCHERS_KEY, "Watchers", watcherRows, (row) => (
          <StopButton
            tick={tick}
            subject={row().key}
            pending={row().killable === false}
            verb={isLoop(row().key) ? "remove" : "stop"}
            pendingLabel={isLoop(row().key) ? "removing…" : "stopping…"}
            title={
              isLoop(row().key)
                ? "Delete this schedule. It will not run again."
                : "Kill this monitor. The agent stops being told what it was watching."
            }
            onConfirm={() => stopWatcher(row().key)}
          />
        ))}
        {/* Queued, whose body is the queue itself. No **Open**: unlike the
            three above it, nothing is being held back for want of room — the
            column shows every queued row, because a queue you can see two of is
            one you cannot reorder. */}
        <Show when={queued().length > 0}>
          <Widget
            itemKey={`h:${QUEUED_KEY}`}
            label="Queued"
            count={queued().length}
            open={isOpen(QUEUED_KEY)}
            onToggle={() => toggle(QUEUED_KEY)}
          >
            <Queue gateway={props.gateway} />
          </Widget>
        </Show>
        <For each={panels()}>
          {(panel) => (
            <Widget
              itemKey={`h:${sectionKey(panel)}`}
              label={panel.viewModel.title}
              source={panel.plugin}
              open={isOpen(sectionKey(panel))}
              onToggle={() => toggle(sectionKey(panel))}
              onOpenFully={() => setOpened(sectionKey(panel))}
            >
              {/* One plugin's bad frame must not take the rail with it. In the
                  transcript a panel was a card that came and went; here it is a
                  permanent resident of the screen, and an exception thrown
                  while drawing one would otherwise unmount every widget beside
                  it. */}
              <ErrorBoundary fallback={(error: unknown) => <Broken plugin={panel.plugin} error={error} />}>
                <Panel
                  plugin={panel.plugin}
                  viewModel={panel.viewModel}
                  onAction={act(panel)}
                  fields={fieldsFor(sectionKey(panel))}
                />
              </ErrorBoundary>
            </Widget>
          )}
        </For>
      </aside>

      {/* The same dialog for the built-in: the column has room for the bar and
          the legend, and not for the text every injected block was measured
          over. That is the pager's split too — a compact tab, and a separate
          view for the injected text. */}
      <Show when={opened() === CONTEXT_KEY && facts()}>
        {(context) => (
          <Overlay label="Context" onClose={() => setOpened(null)}>
            <ContextWidget
              facts={context()}
              model={props.gateway.sessionInfo()?.modelDisplayName ?? props.gateway.sessionInfo()?.model}
              full
              onRefresh={() => void props.gateway.refreshSessionInfo()}
            />
          </Overlay>
        )}
      </Show>

      {/* The fan-out, whole. The rail row is `dock.rs`'s line — a kind, a
          description, what it is doing and how long it has been — and this is
          the pane that line stands for, with the counters, the child's answer
          and the finished rows the column has no room for. The terminal makes
          the same split: Enter on a dock subagent row opens the child
          fullscreen (`panes.rs:464-472`). */}
      <Show when={opened() === SUBAGENTS_KEY}>
        <Overlay label="Subagents" onClose={() => setOpened(null)}>
          <Subagents gateway={props.gateway} />
        </Overlay>
      </Show>

      {/* Tasks and Watchers past the row cap, with the command each row is
          hiding behind its label and the directory it runs in. */}
      <Show when={opened() === TASKS_KEY}>
        <Overlay label="Tasks" onClose={() => setOpened(null)}>
          <BackgroundPane gateway={props.gateway} section="tasks" />
        </Overlay>
      </Show>
      <Show when={opened() === WATCHERS_KEY}>
        <Overlay label="Watchers" onClose={() => setOpened(null)}>
          <BackgroundPane gateway={props.gateway} section="watchers" />
        </Overlay>
      </Show>

      {/* The F6 overlay, as a button rather than a key. A four-column table is
          not readable in a 300px column — measured, not assumed — and this is
          where it is read. */}
      <Show when={openedPanel()}>
        {(panel) => (
          <Overlay label={panel().viewModel.title} onClose={() => setOpened(null)}>
            <ErrorBoundary
              fallback={(error: unknown) => <Broken plugin={panel().plugin} error={error} />}
            >
              <Panel
                plugin={panel().plugin}
                viewModel={panel().viewModel}
                onAction={act(panel())}
                fields={fieldsFor(sectionKey(panel()))}
              />
            </ErrorBoundary>
          </Overlay>
        )}
      </Show>
    </Show>
  );
}

/**
 * How far each key moves the cursor.
 *
 * `Home` and `End` are an unbounded step in either direction, because
 * `moveCursor` clamps: overshooting deliberately *is* how "go to the end" is
 * said, rather than a second code path that could disagree with the first about
 * where the end is.
 */
const STEP: Record<string, number | undefined> = {
  ArrowDown: 1,
  ArrowUp: -1,
  Home: -Infinity,
  End: Infinity,
};

/** A widget whose plugin threw while it was being drawn. */
function Broken(props: { plugin: string; error: unknown }): JSX.Element {
  return (
    <p class="rail-broken">
      {props.plugin} could not be drawn: {String(props.error)}
    </p>
  );
}
