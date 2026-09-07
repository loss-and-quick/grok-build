import { describe, expect, test } from "bun:test";
import type { PanelBlock } from "@grok-build/plugin/generated/PanelBlock.ts";
import type { PanelTone } from "@grok-build/plugin/generated/PanelTone.ts";
import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";

import { renderMarkdown, renderPanel, TONE_ROLE, toneColor, type PanelAction } from "../src/panel.ts";
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

describe("plugin panels", () => {
  test("renders all five block kinds from the generated types", () => {
    const { element } = renderPanel("panel-probe", PANEL, () => {});
    expect(element.querySelector(".grok-panel-title")?.textContent).toBe("Probe");
    expect(element.querySelector(".grok-panel-source")?.textContent).toBe("panel-probe");
    expect(element.querySelectorAll(".grok-panel-chip")).toHaveLength(2);
    expect(element.querySelector(".grok-panel-markdown .grok-md-h2")?.textContent).toBe("Title");
    expect(element.querySelectorAll(".grok-panel-table tbody tr")).toHaveLength(1);
    expect(element.querySelectorAll(".grok-panel-input input")).toHaveLength(2);
    expect(element.querySelectorAll(".grok-panel-button")).toHaveLength(1);
  });

  test("a secret input is masked and a selectable row is focusable", () => {
    const { element } = renderPanel("p", PANEL, () => {});
    const inputs = [...element.querySelectorAll("input")] as HTMLInputElement[];
    expect(inputs[0]?.type).toBe("text");
    expect(inputs[0]?.value).toBe("seed");
    expect(inputs[0]?.placeholder).toBe("hint");
    expect(inputs[1]?.type).toBe("password");
    const row = element.querySelector(".grok-panel-table tbody tr") as HTMLElement;
    expect(row.classList.contains("grok-selectable")).toBe(true);
    expect(row.tabIndex).toBe(0);
  });

  test("a button press carries every input's current value, keyed by id", () => {
    // `PanelActionParams` says a press delivers the panel's inputs alongside
    // the button; that is what makes an OAuth-style panel work.
    const seen: PanelAction[] = [];
    const { element } = renderPanel("p", PANEL, (action) => {
      seen.push(action);
    });
    const inputs = [...element.querySelectorAll("input")] as HTMLInputElement[];
    inputs[0]!.value = "typed";
    inputs[1]!.value = "AQAB";
    (element.querySelector(".grok-panel-button") as HTMLButtonElement).click();
    expect(seen).toEqual([
      { panelId: "p1", buttonId: "go", inputs: { note: "typed", token: "AQAB" } },
    ]);
  });

  test("a keybind is shown, not bound", () => {
    // The pager binds `key` while the panel is focused. A browser page usually
    // has a text field focused, so binding a bare letter would eat typing.
    const { element } = renderPanel("p", PANEL, () => {});
    const button = element.querySelector(".grok-panel-button") as HTMLButtonElement;
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

  test("panel markdown never yields a script-bearing link", () => {
    // Panel text is plugin-authored and reaches the page as markup input.
    const frag = renderMarkdown("[click](javascript:alert(1)) and [ok](https://example.com)");
    const host = document.createElement("div");
    host.append(frag);
    const links = [...host.querySelectorAll("a")] as HTMLAnchorElement[];
    expect(links).toHaveLength(2);
    expect(links[0]?.getAttribute("href")).toBeNull();
    expect(links[1]?.getAttribute("href")).toBe("https://example.com");
  });

  test("markdown text is inserted as text, not parsed as HTML", () => {
    const frag = renderMarkdown("<img src=x onerror=boom>");
    const host = document.createElement("div");
    host.append(frag);
    expect(host.querySelector("img")).toBeNull();
    expect(host.textContent).toContain("<img");
  });
});
