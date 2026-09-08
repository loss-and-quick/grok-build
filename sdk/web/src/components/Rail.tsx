import { ErrorBoundary, For, Show, createEffect, createSignal, type JSX } from "solid-js";

import type { Gateway } from "../gateway.ts";
import type { PanelAction } from "../panel.ts";
import { itemKey, moveCursor, railItems, type RailSection } from "../rail.ts";
import type { PanelEntry } from "../transcript.ts";
import { Overlay } from "./Overlay.tsx";
import { Panel, createFields, type Fields } from "./Panel.tsx";
import { Widget } from "./Widget.tsx";

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
 * What the rail holds today is plugin panels. The dock's other sections —
 * Subagents, Tasks, Watchers, Queued — are data this client mostly already has,
 * and they belong here next; `rail.ts` already models a list section with the
 * dock's row cap, so arriving is a matter of supplying rows.
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

  const sections = (): RailSection[] =>
    panels().map((panel) => ({
      kind: "panel",
      key: sectionKey(panel),
      label: panel.viewModel.title,
      source: panel.plugin,
    }));

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
    <Show when={sections().length > 0}>
      {/* A region, not a dialog: the rail is beside the session, not over it,
          and nothing about it is modal. A rail with no sections renders nothing
          at all — `dock.rs`'s rule, and what keeps the third column from being
          a permanent strip of empty. */}
      <aside class="rail" role="region" aria-label="Session widgets" onKeyDown={onKeyDown}>
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
