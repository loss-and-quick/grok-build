import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";
import type { PanelBlock } from "@grok-build/plugin/generated/PanelBlock.ts";
import type { PanelTone } from "@grok-build/plugin/generated/PanelTone.ts";
import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";

import { Panel } from "../src/components/Panel.tsx";
import { parseInline, parseMarkdown } from "../src/markdown.ts";
import { PANEL_BLOCK_KINDS, TONE_ROLE, toneColor, type PanelAction } from "../src/panel.ts";
import { BACKGROUND_ROLES, cssVarName } from "../src/theme.ts";

/** Every block kind the generated union declares, in one panel. */
const ALL_BLOCKS: PanelBlock[] = [
  {
    kind: "status",
    items: [
      { label: "ok", value: "yes", tone: "success" },
      { label: "bad", value: "no", tone: "error" },
    ],
  },
  { kind: "markdown", text: "## Title\n\nBody with `code`." },
  { kind: "table", columns: ["a", "b"], rows: [["1", "2"]], selectable: true },
  { kind: "input", id: "note", label: "Note", placeholder: "hint", value: "seed", secret: false },
  { kind: "input", id: "token", label: "Token", placeholder: null, value: null, secret: true },
  { kind: "actions", buttons: [{ id: "go", label: "Go", key: "g" }] },
];

const PANEL: PanelViewModel = { id: "p1", title: "Probe", blocks: ALL_BLOCKS };

function mount(onAction: (action: PanelAction) => void = () => {}) {
  return render(() => Panel({ plugin: "panel-probe", viewModel: PANEL, onAction }));
}

describe("plugin panels", () => {
  test("renders all five block kinds from the generated types", () => {
    const { container } = mount();
    expect(container.querySelector(".grok-panel-title")?.textContent).toBe("Probe");
    expect(container.querySelector(".grok-panel-source")?.textContent).toBe("panel-probe");
    expect(container.querySelectorAll(".grok-panel-chip")).toHaveLength(2);
    expect(container.querySelector(".grok-panel-markdown .grok-md-h2")?.textContent).toBe("Title");
    expect(container.querySelectorAll(".grok-panel-table tbody tr")).toHaveLength(1);
    expect(container.querySelectorAll(".grok-panel-input input")).toHaveLength(2);
    expect(container.querySelectorAll(".grok-panel-button")).toHaveLength(1);
    // Nothing fell through to the "kind I cannot draw" marker.
    expect(container.querySelector(".grok-panel-unknown")).toBeNull();
  });

  test("every generated block kind is one this renderer draws", () => {
    // `PANEL_BLOCK_KINDS` fails to compile when the generated union grows a
    // variant; this ties that guard to the renderer, so the two cannot drift.
    const drawn = new Set(ALL_BLOCKS.map((block) => block.kind));
    expect([...drawn].sort()).toEqual(
      (Object.keys(PANEL_BLOCK_KINDS) as PanelBlock["kind"][]).sort(),
    );
  });

  test("a secret input is masked and a selectable row is focusable", () => {
    const { container } = mount();
    const inputs = [...container.querySelectorAll("input")] as HTMLInputElement[];
    expect(inputs[0]?.type).toBe("text");
    expect(inputs[0]?.value).toBe("seed");
    expect(inputs[0]?.placeholder).toBe("hint");
    expect(inputs[1]?.type).toBe("password");
    const row = container.querySelector(".grok-panel-table tbody tr") as HTMLElement;
    expect(row.classList.contains("grok-selectable")).toBe(true);
    expect(row.tabIndex).toBe(0);
  });

  test("a button press carries every input's current value, keyed by id", () => {
    // `PanelActionParams` says a press delivers the panel's inputs alongside
    // the button; that is what makes an OAuth-style panel work.
    const seen: PanelAction[] = [];
    const { container } = mount((action) => seen.push(action));
    const inputs = [...container.querySelectorAll("input")] as HTMLInputElement[];
    inputs[0]!.value = "typed";
    inputs[1]!.value = "AQAB";
    (container.querySelector(".grok-panel-button") as HTMLButtonElement).click();
    expect(seen).toEqual([
      { panelId: "p1", buttonId: "go", inputs: { note: "typed", token: "AQAB" } },
    ]);
  });

  test("a keybind is shown, not bound", () => {
    // The pager binds `key` while the panel is focused. A browser page usually
    // has a text field focused, so binding a bare letter would eat typing.
    const { container } = mount();
    const button = container.querySelector(".grok-panel-button") as HTMLButtonElement;
    expect(button.title).toContain("g");
  });

  test("every tone maps to a colour role, and none of them is a background", () => {
    const tones: PanelTone[] = ["neutral", "success", "warning", "error"];
    for (const tone of tones) {
      const role = TONE_ROLE[tone];
      expect(role).toBeString();
      expect(BACKGROUND_ROLES.has(role)).toBe(false);
      expect(toneColor(tone)).toBe(`var(${cssVarName(role)})`);
    }
    // Exhaustive: a new tone in the generated union has no role here and this
    // fails rather than painting it as `neutral`.
    expect(Object.keys(TONE_ROLE).sort()).toEqual([...tones].sort());
  });
});

describe("panel markdown", () => {
  test("only http(s) links keep an href", () => {
    // Panel text is plugin-authored and reaches the page as markup input;
    // `javascript:` in an href is script execution in this page's origin.
    const spans = parseInline("[click](javascript:alert(1)) and [ok](https://example.com)");
    const links = spans.filter((s) => s.kind === "link");
    expect(links).toHaveLength(2);
    expect(links[0]).toMatchObject({ text: "click", href: null });
    expect(links[1]).toMatchObject({ text: "ok", href: "https://example.com" });
  });

  test("a refused link renders as text with no href attribute", () => {
    const vm: PanelViewModel = {
      id: "p",
      title: "t",
      blocks: [{ kind: "markdown", text: "[click](javascript:alert(1))" }],
    };
    const { container } = render(() =>
      Panel({ plugin: "p", viewModel: vm, onAction: () => {} }),
    );
    const link = container.querySelector("a.grok-md-link") as HTMLAnchorElement;
    expect(link.textContent).toBe("click");
    expect(link.getAttribute("href")).toBeNull();
  });

  test("markdown text is data, never markup", () => {
    const vm: PanelViewModel = {
      id: "p",
      title: "t",
      blocks: [{ kind: "markdown", text: "<img src=x onerror=boom>" }],
    };
    const { container } = render(() =>
      Panel({ plugin: "p", viewModel: vm, onAction: () => {} }),
    );
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toContain("<img");
  });

  test("headings, fenced code and inline spans parse", () => {
    const blocks = parseMarkdown("## Head\n\ntext `c` and **b**\n\n```\nfenced\n```");
    expect(blocks.map((b) => b.kind)).toEqual(["heading", "paragraph", "code"]);
    expect(blocks[0]).toMatchObject({ level: 2 });
    expect(blocks[2]).toMatchObject({ text: "fenced" });
    const spans = blocks[1]!.kind === "paragraph" ? blocks[1]!.spans : [];
    expect(spans.map((s) => s.kind)).toEqual(["text", "code", "text", "strong"]);
  });
});
