// The widget rail, and the dock rules it is a port of.
//
// The pure half first, because that is where divergence would live: which lines
// exist, which of them the cursor can land on, and how it moves. `dock.rs`
// derives all three from one walk and says why — "so the cursor, mouse
// hit-testing, height, and paint can't drift" — so the tests below are written
// against that walk rather than against the DOM it produces.
import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";
import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";

import { Rail } from "../src/components/Rail.tsx";
import type { Gateway } from "../src/gateway.ts";
import {
  MAX_SECTION_ROWS,
  itemKey,
  moveCursor,
  railEnabled,
  railItems,
  visualRows,
  type RailSection,
} from "../src/rail.ts";
import { createSubagents } from "../src/subagents.ts";
import { createTasks } from "../src/tasks.ts";
import { createTranscript } from "../src/transcript.ts";
import type { PanelAction } from "../src/panel.ts";
import type { RosterEntry, SessionUpdate } from "../src/wire.ts";

const NONE = new Set<string>();

function list(key: string, rows: number): RailSection {
  return {
    kind: "list",
    key,
    label: key,
    rows: Array.from({ length: rows }, (_, at) => ({ key: `${at}`, label: `row ${at}` })),
  };
}

const panelSection: RailSection = { kind: "panel", key: "p", label: "Panel", source: "acme" };

describe("the rail's line walk", () => {
  test("hides a section with nothing in it, and an empty rail draws nothing", () => {
    // `dock.rs`: "Sections with a zero count are hidden; an all-zero dock
    // renders nothing." Without it the third column is a permanent strip of
    // headings for things that are not happening.
    expect(visualRows([list("a", 0)], NONE)).toEqual([]);
    expect(visualRows([list("a", 0), list("b", 1)], NONE)).toHaveLength(2);
  });

  test("a published panel is never hidden that way", () => {
    // A panel has no count to be zero: publishing one *is* the plugin asking
    // for the space, so the emptiness rule does not reach it.
    expect(visualRows([panelSection], NONE)).toEqual([
      { kind: "header", section: "p" },
      { kind: "body", section: "p" },
    ]);
  });

  test("caps a list at the pager's own row count and says how many are left", () => {
    const rows = visualRows([list("a", 5)], NONE);
    expect(rows.filter((row) => row.kind === "row")).toHaveLength(MAX_SECTION_ROWS);
    expect(rows.at(-1)).toEqual({ kind: "more", section: "a", hidden: 5 - MAX_SECTION_ROWS });
  });

  test("a collapsed section keeps its header and loses everything under it", () => {
    expect(visualRows([list("a", 5)], new Set(["a"]))).toEqual([{ kind: "header", section: "a" }]);
    expect(visualRows([panelSection], new Set(["p"]))).toEqual([
      { kind: "header", section: "p" },
    ]);
  });

  test("the cursor walks headers and rows together, and nothing else", () => {
    // Interleaved, as `dock.rs` has it: Down from a section's last row lands on
    // the next section's header rather than skipping to its first row. The "N
    // more" line is not selectable there and a panel's body is not selectable
    // here — what is inside it has its own focusable elements, and Tab is what
    // reaches those.
    const items = railItems([list("a", 5), panelSection], NONE);
    expect(items.map(itemKey)).toEqual(["h:a", "r:a:0", "r:a:1", "h:p"]);
  });

  test("moving clamps at both ends rather than wrapping", () => {
    // The pager's `saturating_sub` and `min`. Wrapping would turn "press Down
    // until it stops" into a loop with no end.
    expect(moveCursor(4, 0, -1)).toBe(0);
    expect(moveCursor(4, 3, 1)).toBe(3);
    expect(moveCursor(4, 1, -Infinity)).toBe(0);
    expect(moveCursor(4, 1, Infinity)).toBe(3);
    expect(moveCursor(0, 0, 1)).toBe(0);
  });
});

describe("who decides the rail is drawn", () => {
  test("the agent, unless this browser has said otherwise", () => {
    // The pager's ladder with the layers a browser can see: a local answer
    // outranks the cohort flag, and with neither the answer is off — the
    // feature registry's own default.
    expect(railEnabled(null, null)).toBe(false);
    expect(railEnabled(null, true)).toBe(true);
    expect(railEnabled(null, false)).toBe(false);
    expect(railEnabled(true, false)).toBe(true);
    expect(railEnabled(false, true)).toBe(false);
  });
});

const ENTRY: RosterEntry = {
  sessionId: "s1",
  cwd: "/home/me/repo",
  isWorktree: false,
  yolo: false,
  activity: "idle",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

function panel(id: string, title: string): PanelViewModel {
  return {
    id,
    title,
    blocks: [
      { kind: "status", items: [{ label: "state", value: "waiting", tone: "neutral" }] },
      { kind: "input", id: "code", label: "Code", placeholder: null, value: null, secret: false },
      { kind: "actions", buttons: [{ id: "go", label: "Go", key: null }] },
    ],
  };
}

function mount(published: [string, PanelViewModel][] = [], seed?: (into: Fixture) => void) {
  const transcript = createTranscript();
  const subagents = createSubagents(ENTRY.sessionId);
  const tasks = createTasks();
  for (const [plugin, viewModel] of published) {
    transcript.apply({ sessionUpdate: "plugin_panel", plugin, view_model: viewModel } as SessionUpdate);
  }
  const stopped: string[] = [];
  const removed: string[] = [];
  seed?.({ subagents, tasks });
  const seen: PanelAction[] = [];
  const gateway = {
    attached: () => ({ entry: ENTRY, transcript, subagents, tasks }),
    // Nobody has asked for the window here, so the rail holds plugin panels
    // and nothing else — which is what these tests are about. The context
    // section has its own file.
    sessionInfo: () => null,
    panelAction: async (_plugin: string, action: PanelAction) => {
      seen.push(action);
    },
    cancelSubagent: async (id: string) => {
      stopped.push(id);
    },
    // The two the seam looks for. A gateway without them carries no background
    // work at all, which is the state this client actually ships in today —
    // `tasks.test.ts` covers that half.
    killTask: (id: string) => stopped.push(id),
    cancelScheduledLoop: (id: string) => removed.push(id),
  } as unknown as Gateway;
  const { container } = render(() => Rail({ gateway }));
  return { container, seen, transcript, stopped, removed };
}

interface Fixture {
  subagents: ReturnType<typeof createSubagents>;
  tasks: ReturnType<typeof createTasks>;
}

/** The section headers on screen, in the order they are drawn. */
function labels(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".rail-label")].map((node) => node.textContent ?? "");
}

/** One section's row text, meta included. */
function rowsOf(container: HTMLElement, section: string): string[] {
  return [...container.querySelectorAll(`[data-rail-item^="r:${section}:"]`)].map((node) =>
    (node.textContent ?? "").replace(/\s+/g, " ").trim(),
  );
}

describe("the rail on screen", () => {
  test("draws one section per published panel, in publication order", () => {
    // The pager keeps its panels in an `IndexMap` and removes with
    // `shift_remove`, so publication order is the order in both clients — for
    // nothing, on this side.
    const { container } = mount([
      ["acme", panel("oauth", "acme: OAuth")],
      ["ci", panel("status", "ci: status")],
    ]);
    expect([...container.querySelectorAll(".rail-label")].map((n) => n.textContent)).toEqual([
      "acme: OAuth",
      "ci: status",
    ]);
    expect([...container.querySelectorAll(".rail-source")].map((n) => n.textContent)).toEqual([
      "acme",
      "ci",
    ]);
  });

  test("renders nothing at all when no plugin has published one", () => {
    const { container } = mount([]);
    expect(container.querySelector(".rail")).toBeNull();
  });

  test("a header folds its section, and says so where a reader can hear it", () => {
    const { container } = mount([["acme", panel("oauth", "acme: OAuth")]]);
    const header = container.querySelector<HTMLButtonElement>(".rail-disclosure")!;
    expect(header.getAttribute("aria-expanded")).toBe("true");
    expect(container.querySelector(".rail-body")).not.toBeNull();
    header.click();
    expect(header.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector(".rail-body")).toBeNull();
    // `aria-controls` has to name something that exists while it is expanded.
    header.click();
    expect(document.getElementById(header.getAttribute("aria-controls")!)).not.toBeNull();
  });

  test("arrows walk between sections without leaving the rail", () => {
    const { container } = mount([
      ["acme", panel("oauth", "acme: OAuth")],
      ["ci", panel("status", "ci: status")],
    ]);
    const headers = [...container.querySelectorAll<HTMLButtonElement>(".rail-disclosure")];
    headers[0]!.focus();
    const press = (key: string): void => {
      document.activeElement!.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
      );
    };
    press("ArrowDown");
    expect(document.activeElement).toBe(headers[1]!);
    press("ArrowDown");
    expect(document.activeElement).toBe(headers[1]!);
    press("Home");
    expect(document.activeElement).toBe(headers[0]!);
    press("End");
    expect(document.activeElement).toBe(headers[1]!);
  });

  test("a key the rail does not own is left for the field it was typed into", () => {
    // The rail's own listener sits on the whole column, and a plugin's text
    // field lives inside it. Arrows in that field belong to the field.
    const { container } = mount([["acme", panel("oauth", "acme: OAuth")]]);
    const field = container.querySelector<HTMLInputElement>(".grok-panel-input input")!;
    field.focus();
    const event = new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true });
    field.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
    expect(document.activeElement).toBe(field);
  });

  test("Open raises the panel whole, and Escape puts it away", () => {
    // The F6 overlay without taking F6, which Firefox and Chrome use to move
    // focus between browser regions. This is where a table too wide for a
    // column is read.
    const { container } = mount([["acme", panel("oauth", "acme: OAuth")]]);
    container.querySelector<HTMLButtonElement>(".rail-open")!.click();
    const dialog = document.querySelector(".overlay")!;
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-label")).toBe("acme: OAuth");
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(document.querySelector(".overlay")).toBeNull();
  });

  test("a button in the rail still reaches the plugin that drew it", () => {
    const { container, seen } = mount([["acme", panel("oauth", "acme: OAuth")]]);
    const field = container.querySelector<HTMLInputElement>(".grok-panel-input input")!;
    field.value = "typed";
    container.querySelector<HTMLButtonElement>(".grok-panel-button")!.click();
    expect(seen).toEqual([{ panelId: "oauth", buttonId: "go", inputs: { code: "typed" } }]);
  });
});

describe("the dock's own sections in the rail", () => {
  const spawn = (id: string, over: Record<string, unknown> = {}): SessionUpdate =>
    ({
      sessionUpdate: "subagent_spawned",
      subagent_id: id,
      parent_session_id: ENTRY.sessionId,
      child_session_id: `child-${id}`,
      subagent_type: "explore",
      description: "find the render path",
      ...over,
    }) as unknown as SessionUpdate;

  const bg = (id: string, over: Record<string, unknown> = {}): SessionUpdate =>
    ({
      sessionUpdate: "task_backgrounded",
      tool_call_id: `tc-${id}`,
      task_id: id,
      command: "cargo test",
      cwd: "/repo",
      output_file: "/tmp/out",
      ...over,
    }) as unknown as SessionUpdate;

  test("a section with nothing in it is not drawn at all", () => {
    // `dock.rs`: "Sections with a zero count are hidden; an all-zero dock
    // renders nothing." Three empty sections must not become three headings
    // for things that are not happening — nor an empty third column.
    const { container } = mount();
    expect(container.querySelector(".rail")).toBeNull();
  });

  test("they sit between the context window and the plugins, in the dock's order", () => {
    const { container } = mount([["acme", panel("oauth", "acme: OAuth")]], ({ subagents, tasks }) => {
      subagents.apply(ENTRY.sessionId, spawn("sa-1"));
      tasks.apply(bg("run-1"), false);
      tasks.apply(bg("mon-1", { monitor_description: "watch the build" }), false);
    });
    // No context window has been resolved in this fixture, so the three dock
    // sections lead — the order among themselves is what is asserted here, and
    // it is `dock.rs:60-67`'s.
    expect(labels(container)).toEqual(["Subagents", "Tasks", "Watchers", "acme: OAuth"]);
  });

  test("a row is the dock's line: kind, label, activity, then the meta column", () => {
    const { container } = mount([], ({ subagents }) => {
      subagents.apply(ENTRY.sessionId, spawn("sa-1", { model: "grok-4.5" }));
      // The child's own frames arrive on the child's session id and feed its
      // activity label, which is the one thing on a dock row that comes from a
      // session other than the attached one.
      subagents.applyChild("child-sa-1", {
        sessionUpdate: "agent_thought_chunk",
        content: { type: "text", text: "…" },
      } as unknown as SessionUpdate);
    });
    const [row] = rowsOf(container, "subagents");
    expect(row).toContain("Explore");
    expect(row).toContain("find the render path");
    expect(row).toContain("Thinking");
    expect(row).toContain("grok-4.5");
  });

  test("a child's elapsed time is read on the clock it was measured on", () => {
    // This row once read `grok-4.5 29815187m13s`. The fan-out measures the wait
    // since the last progress frame with `performance.now()` and adds it to the
    // agent's own `duration_ms`; the rail was handing it a wall clock instead,
    // and the two differ by the epoch. Any such mix lands in the tens of
    // millions of minutes, so the guard is an upper bound rather than a value.
    const { container } = mount([], ({ subagents }) => {
      subagents.apply(ENTRY.sessionId, spawn("sa-1", { model: "grok-4.5" }));
      subagents.apply(ENTRY.sessionId, {
        sessionUpdate: "subagent_progress",
        subagent_id: "sa-1",
        parent_session_id: ENTRY.sessionId,
        child_session_id: "child-sa-1",
        duration_ms: 134_000,
        turn_count: 1,
        tool_call_count: 1,
        tokens_used: 1,
        context_window_tokens: 2,
        context_usage_pct: 1,
        tools_used: [],
        error_count: 0,
      } as unknown as SessionUpdate);
    });
    const meta = container.querySelector(".rail-row-meta")!.textContent!;
    expect(meta).toMatch(/^grok-4\.5 \d+m\d\ds$/);
    expect(Number(/(\d+)m/.exec(meta)![1])).toBeLessThan(10);
  });

  test("the count on the header is the number of rows, not the number shown", () => {
    const { container } = mount([], ({ tasks }) => {
      for (const id of ["a", "b", "c", "d"]) tasks.apply(bg(id), false);
    });
    expect(container.querySelector(".rail-count")?.textContent).toBe("4");
    expect(rowsOf(container, "tasks")).toHaveLength(MAX_SECTION_ROWS);
    expect(container.querySelector(".rail-more")?.textContent).toContain(
      `${4 - MAX_SECTION_ROWS} more`,
    );
  });

  test("arrows walk from a header into its rows and on to the next header", () => {
    // The dock's cursor is one sequence over headers *and* rows, so Down from a
    // section's last shown row lands on the next section's header rather than
    // on its first row — and the "N more" line is not a stop on the way.
    const { container } = mount([], ({ tasks }) => {
      for (const id of ["a", "b", "c"]) tasks.apply(bg(id), false);
      tasks.apply(bg("mon", { monitor_description: "watch" }), false);
    });
    const walk = [...container.querySelectorAll("[data-rail-item]")].map((node) =>
      node.getAttribute("data-rail-item"),
    );
    expect(walk).toEqual(["h:tasks", "r:tasks:0", "r:tasks:1", "h:watchers", "r:watchers:0"]);
  });

  test("Watchers holds monitors and loops, and the stop each one needs differs", () => {
    // Two actions behind one button: a monitor is a process to kill, a loop is
    // a schedule to delete. The terminal splits the same way, on a
    // `DockWatcherId` that remembers which kind the row came from.
    const { container, stopped, removed } = mount([], ({ tasks }) => {
      tasks.apply(bg("mon", { monitor_description: "watch the build" }), false);
      tasks.apply(
        {
          sessionUpdate: "scheduled_task_created",
          task_id: "loop-1",
          prompt: "check CI",
          human_schedule: "every 5m",
        } as unknown as SessionUpdate,
        false,
      );
    });
    const rows = rowsOf(container, "watchers");
    expect(rows[0]).toContain("Monitor");
    expect(rows[1]).toContain("Loop");
    expect(rows[1]).toContain("every 5m");

    const buttons = [...container.querySelectorAll<HTMLButtonElement>(".stop-button")];
    expect(buttons[1]!.textContent).toContain("remove");
    // Twice, because the first press only arms it.
    buttons[0]!.click();
    buttons[0]!.click();
    buttons[1]!.click();
    buttons[1]!.click();
    expect(stopped).toEqual(["mon"]);
    expect(removed).toEqual(["loop-1"]);
  });

  test("the first press of a stop does not send it", () => {
    const { container, stopped } = mount([], ({ tasks }) => {
      tasks.apply(bg("run-1"), false);
    });
    const button = container.querySelector<HTMLButtonElement>(".stop-button")!;
    button.click();
    expect(stopped).toEqual([]);
    expect(button.textContent).toContain("confirm");
    button.click();
    expect(stopped).toEqual(["run-1"]);
  });
});
