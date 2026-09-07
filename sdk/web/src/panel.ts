// Renders a plugin's declarative panel.
//
// The point of this file is what is *not* in it: no plugin was changed to make
// its panel appear in a browser, and no panel type is restated here. The types
// are imported from `sdk/plugin/src/generated/`, where ts-rs put them, so a new
// `PanelBlock` variant is a TypeScript error in `renderBlock` rather than a
// block that silently renders as nothing.
//
// `PanelTone` is the reason this works at all: a plugin says `"error"`, and the
// renderer picks the colour. Had the tone been a hex the plugin would have been
// painting for a terminal, and a second client would be repainting its output.
import type { PanelBlock } from "@grok-build/plugin/generated/PanelBlock.ts";
import type { PanelTone } from "@grok-build/plugin/generated/PanelTone.ts";
import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";

import { cssVarName, type ThemeRole } from "./theme.ts";

/** Colour role each tone paints in. The pager's own vocabulary, not a new one. */
export const TONE_ROLE: Record<PanelTone, ThemeRole> = {
  neutral: "text_secondary",
  success: "accent_success",
  warning: "warning",
  error: "accent_error",
};

export function toneColor(tone: PanelTone): string {
  return `var(${cssVarName(TONE_ROLE[tone])})`;
}

/** What a button press sends back: the button, plus every input in the panel. */
export interface PanelAction {
  panelId: string;
  buttonId: string;
  inputs: Record<string, string>;
}

export interface PanelHandle {
  element: HTMLElement;
  /** Current value of every `input` block, keyed by the block's `id`. */
  inputs(): Record<string, string>;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  return node;
}

/**
 * Minimal markdown: headings, fenced code, inline code, bold, links, paragraphs.
 *
 * Deliberately small and escaping-first. The pager runs a real markdown parser;
 * matching it is a later job and a shared one, since duplicating a renderer per
 * client is exactly the drift this client exists to argue against.
 */
export function renderMarkdown(text: string): DocumentFragment {
  const frag = document.createDocumentFragment();
  const lines = text.split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? "";
    if (line.startsWith("```")) {
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !(lines[i] ?? "").startsWith("```")) {
        body.push(lines[i] ?? "");
        i += 1;
      }
      i += 1;
      const pre = el("pre", "grok-md-code");
      pre.textContent = body.join("\n");
      frag.append(pre);
      continue;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      const level = (heading[1] ?? "#").length;
      const h = el("div", `grok-md-h grok-md-h${level}`);
      h.append(renderInline(heading[2] ?? ""));
      frag.append(h);
      i += 1;
      continue;
    }
    if (line.trim() === "") {
      i += 1;
      continue;
    }
    const p = el("p", "grok-md-p");
    p.append(renderInline(line));
    frag.append(p);
    i += 1;
  }
  return frag;
}

function renderInline(text: string): DocumentFragment {
  const frag = document.createDocumentFragment();
  const pattern = /`([^`]+)`|\*\*([^*]+)\*\*|\[([^\]]+)\]\(([^)\s]+)\)/g;
  let last = 0;
  for (const m of text.matchAll(pattern)) {
    const at = m.index;
    if (at > last) frag.append(text.slice(last, at));
    if (m[1] !== undefined) {
      const code = el("code", "grok-md-inline-code");
      code.textContent = m[1];
      frag.append(code);
    } else if (m[2] !== undefined) {
      const strong = el("strong");
      strong.textContent = m[2];
      frag.append(strong);
    } else if (m[3] !== undefined && m[4] !== undefined) {
      const a = el("a", "grok-md-link");
      a.textContent = m[3];
      // Only http(s): a panel is plugin-authored text, and `javascript:` in an
      // href is script execution in this page's origin.
      const href = /^https?:\/\//i.test(m[4]) ? m[4] : "";
      if (href) {
        a.href = href;
        a.target = "_blank";
        a.rel = "noreferrer noopener";
      }
      frag.append(a);
    }
    last = at + m[0].length;
  }
  if (last < text.length) frag.append(text.slice(last));
  return frag;
}

function renderBlock(
  block: PanelBlock,
  inputs: Map<string, HTMLInputElement>,
  onAction: (buttonId: string) => void,
): HTMLElement {
  switch (block.kind) {
    case "status": {
      const wrap = el("div", "grok-panel-status");
      for (const item of block.items) {
        const chip = el("div", "grok-panel-chip");
        const label = el("span", "grok-panel-chip-label");
        label.textContent = item.label;
        const value = el("span", "grok-panel-chip-value");
        value.textContent = item.value;
        value.style.color = toneColor(item.tone);
        chip.append(label, value);
        wrap.append(chip);
      }
      return wrap;
    }
    case "markdown": {
      const wrap = el("div", "grok-panel-markdown");
      wrap.append(renderMarkdown(block.text));
      return wrap;
    }
    case "table": {
      const wrap = el("div", "grok-panel-table-wrap");
      const table = el("table", "grok-panel-table");
      const thead = el("thead");
      const hrow = el("tr");
      for (const column of block.columns) {
        const th = el("th");
        th.textContent = column;
        hrow.append(th);
      }
      thead.append(hrow);
      const tbody = el("tbody");
      for (const row of block.rows) {
        const tr = el("tr");
        if (block.selectable) {
          tr.tabIndex = 0;
          tr.classList.add("grok-selectable");
        }
        for (const cell of row) {
          const td = el("td");
          td.textContent = cell;
          tr.append(td);
        }
        tbody.append(tr);
      }
      table.append(thead, tbody);
      wrap.append(table);
      return wrap;
    }
    case "input": {
      const wrap = el("label", "grok-panel-input");
      const label = el("span", "grok-panel-input-label");
      label.textContent = block.label;
      const field = el("input");
      field.type = block.secret ? "password" : "text";
      if (block.placeholder) field.placeholder = block.placeholder;
      field.value = block.value ?? "";
      inputs.set(block.id, field);
      wrap.append(label, field);
      return wrap;
    }
    case "actions": {
      const wrap = el("div", "grok-panel-actions");
      for (const button of block.buttons) {
        const b = el("button", "grok-panel-button");
        b.textContent = button.label;
        // `key` is the pager's single-character keybind while the panel is
        // focused. Shown, not bound: a browser page has a focused text field
        // most of the time, and stealing a letter from it would be worse than
        // making the user click.
        if (button.key) b.title = `Keybind in the terminal: ${button.key}`;
        b.addEventListener("click", () => onAction(button.id));
        wrap.append(b);
      }
      return wrap;
    }
  }
}

export function renderPanel(
  plugin: string,
  vm: PanelViewModel,
  onAction: (action: PanelAction) => void,
): PanelHandle {
  const inputs = new Map<string, HTMLInputElement>();
  const collect = (): Record<string, string> =>
    Object.fromEntries([...inputs].map(([id, field]) => [id, field.value]));

  const root = el("section", "grok-panel");
  const header = el("header", "grok-panel-header");
  const title = el("span", "grok-panel-title");
  title.textContent = vm.title;
  const source = el("span", "grok-panel-source");
  source.textContent = plugin;
  header.append(title, source);
  root.append(header);

  const body = el("div", "grok-panel-body");
  for (const block of vm.blocks) {
    body.append(
      renderBlock(block, inputs, (buttonId) =>
        onAction({ panelId: vm.id, buttonId, inputs: collect() }),
      ),
    );
  }
  root.append(body);
  return { element: root, inputs: collect };
}
