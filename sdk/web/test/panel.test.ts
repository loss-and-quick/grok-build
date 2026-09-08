import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";
import type { PanelBlock } from "@grok-build/plugin/generated/PanelBlock.ts";
import type { PanelTone } from "@grok-build/plugin/generated/PanelTone.ts";
import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";

import { Panel } from "../src/components/Panel.tsx";
import { MARKDOWN_TAGS, parseMarkdown, safeHref } from "../src/markdown.ts";
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
  test("only http(s) keeps an href", () => {
    // Panel text is plugin-authored and reaches the page as markup input;
    // `javascript:` in an href is script execution in this page's origin.
    expect(safeHref("https://example.com")).toBe("https://example.com");
    expect(safeHref("http://example.com")).toBe("http://example.com");
    expect(safeHref("javascript:alert(1)")).toBeNull();
    expect(safeHref("mailto:a@b.com")).toBeNull();
    expect(safeHref(null)).toBeNull();
  });

  test("a refused scheme never reaches the DOM as a link", () => {
    const vm: PanelViewModel = {
      id: "p",
      title: "t",
      blocks: [{ kind: "markdown", text: "[click](javascript:alert(1))" }],
    };
    const { container } = render(() =>
      Panel({ plugin: "p", viewModel: vm, onAction: () => {} }),
    );
    // Two independent gates, and the parser's fires first: markdown-it's own
    // `validateLink` refuses the scheme, so there is no anchor at all and the
    // text survives as the plugin wrote it.
    expect(container.querySelectorAll("a")).toHaveLength(0);
    expect(container.textContent).toContain("javascript:alert(1)");
  });

  test("the second gate holds for a scheme the parser allows", () => {
    // `mailto:` passes markdown-it and is refused here, so this is the case
    // that proves `safeHref` is load-bearing rather than decorative. The
    // terminal is laxer — `SchemeFilter::Standard` passes `mailto:` — because
    // an unfiltered scheme there is inert text and here it is a navigation.
    const vm: PanelViewModel = {
      id: "p",
      title: "t",
      blocks: [{ kind: "markdown", text: "[write](mailto:a@b.com)" }],
    };
    const { container } = render(() =>
      Panel({ plugin: "p", viewModel: vm, onAction: () => {} }),
    );
    const link = container.querySelector("a.grok-md-a") as HTMLAnchorElement;
    expect(link.textContent).toBe("write");
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

  test("no element is built for a tag outside the reviewed list", () => {
    // The renderer hands `Dynamic` a tag straight off a node, so the set of
    // element names a plugin can reach has to be closed by the parser.
    for (const node of parseMarkdown("# h\n\n- a\n\n> q\n\n| a |\n| - |\n| 1 |\n\n---\n")) {
      if (node.kind === "element") expect(MARKDOWN_TAGS).toContain(node.tag);
    }
  });

  test("headings, fenced code and inline spans parse", () => {
    const nodes = parseMarkdown("## Head\n\ntext `c` and **b**\n\n```rust\nfenced\n```");
    expect(nodes.map((n) => (n.kind === "element" ? n.tag : n.kind))).toEqual([
      "h2",
      "p",
      "code",
    ]);
    const fence = nodes[2];
    expect(fence).toMatchObject({ kind: "code", text: "fenced\n", language: "rust" });
  });

  test("a single tilde is literal, as the terminal's parser makes it", () => {
    // `markdown-core`'s `offset_events` demotes `~text~` to a literal tilde so
    // model output like `~**10%**` is not struck through. The browser has to
    // agree, and this is the case that separates markdown-it from `marked`,
    // which strikes it.
    const { container } = render(() =>
      Panel({
        plugin: "p",
        viewModel: { id: "p", title: "t", blocks: [{ kind: "markdown", text: "a ~10%~ b" }] },
        onAction: () => {},
      }),
    );
    expect(container.querySelector("s")).toBeNull();
    expect(container.textContent).toContain("~10%~");
    expect(parseMarkdown("a ~~gone~~ b").length).toBeGreaterThan(0);
  });

  test("the constructs the panel protocol promises all render", () => {
    // `PanelBlock::Markdown`'s own doc comment says a panel gets "headings,
    // lists, tables, code — the same one used for model output". Every one of
    // these was raw text before the parser was a real one.
    const text =
      "- one\n- two\n\n1. first\n\n> quoted\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n" +
      "*it* **b** ~~s~~ and https://example.com\n\n---\n";
    const { container } = render(() =>
      Panel({
        plugin: "p",
        viewModel: { id: "p", title: "t", blocks: [{ kind: "markdown", text }] },
        onAction: () => {},
      }),
    );
    expect(container.querySelectorAll(".grok-md-ul .grok-md-li")).toHaveLength(2);
    expect(container.querySelector(".grok-md-ol")).not.toBeNull();
    expect(container.querySelector(".grok-md-blockquote")?.textContent).toContain("quoted");
    expect(container.querySelectorAll(".grok-md-table .grok-md-th")).toHaveLength(2);
    expect(container.querySelectorAll(".grok-md-table .grok-md-td")).toHaveLength(2);
    expect(container.querySelector("em")?.textContent).toBe("it");
    expect(container.querySelector("strong")?.textContent).toBe("b");
    expect(container.querySelector("s")?.textContent).toBe("s");
    expect(container.querySelector(".grok-md-hr")).not.toBeNull();
    // A bare URL is a link in the terminal too: GFM autolinks it and the pager
    // linkifies prose a second time.
    const auto = container.querySelector("a.grok-md-a") as HTMLAnchorElement;
    expect(auto.getAttribute("href")).toBe("https://example.com");
  });

  test("an image is text, not a fetch", () => {
    // The pager's pretty mode renders `![alt](src)` as `alt (src)`, and no
    // client here should reach out to a host a plugin named.
    const { container } = render(() =>
      Panel({
        plugin: "p",
        viewModel: {
          id: "p",
          title: "t",
          blocks: [{ kind: "markdown", text: "![alt](https://e.example/i.png)" }],
        },
        onAction: () => {},
      }),
    );
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toContain("alt (https://e.example/i.png)");
  });
});
