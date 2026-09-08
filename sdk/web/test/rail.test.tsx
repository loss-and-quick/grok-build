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

function mount(published: [string, PanelViewModel][]) {
  const transcript = createTranscript();
  const subagents = createSubagents(ENTRY.sessionId);
  for (const [plugin, viewModel] of published) {
    transcript.apply({ sessionUpdate: "plugin_panel", plugin, view_model: viewModel } as SessionUpdate);
  }
  const seen: PanelAction[] = [];
  const gateway = {
    attached: () => ({ entry: ENTRY, transcript, subagents }),
    panelAction: async (_plugin: string, action: PanelAction) => {
      seen.push(action);
    },
  } as unknown as Gateway;
  const { container } = render(() => Rail({ gateway }));
  return { container, seen, transcript };
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
