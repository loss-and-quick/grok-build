// The menu as it is actually driven: a real `Session` over a stub gateway, so
// the keys land on the textarea the way they do in a browser and the reactivity
// is the browser build's (see `test/environment.test.tsx` for why that is worth
// saying out loud).
import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";

import { Session } from "../src/components/Session.tsx";
import type { Gateway } from "../src/gateway.ts";
import { createSubagents } from "../src/subagents.ts";
import { createTranscript } from "../src/transcript.ts";
import type { AvailableCommand, RosterEntry } from "../src/wire.ts";

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

function catalog(): AvailableCommand[] {
  return [
    { name: "compact", description: "Compact the conversation", input: null },
    { name: "flush", description: "Flush memory", input: null },
    {
      name: "model",
      description: "Switch the active model",
      input: { hint: "<model id>" },
    },
    {
      name: "ship",
      description: "Ship the build",
      input: null,
      _meta: { pluginCommand: true, pluginName: "deployer" },
    },
    {
      name: "commit",
      description: "Write a commit message",
      input: null,
      _meta: { scope: "user", path: "/home/me/.grok/skills/commit/SKILL.md" },
    },
  ];
}

function stub(over: Partial<Gateway> = {}): { gateway: Gateway; sent: string[] } {
  const sent: string[] = [];
  const transcript = createTranscript();
  // The real `attach` builds one per session, so a stub that omits it is a
  // gateway shape this client never produces.
  const subagents = createSubagents(ENTRY.sessionId);
  const gateway = {
    attached: () => ({ entry: ENTRY, transcript, subagents }),
    permissions: [],
    folderTrusts: [],
    status: () => "connected",
    commands: () => catalog(),
    // No catalog on the wire is a real state (a session whose reply carried
    // none), and the one this stub is in: the picker draws nothing.
    models: () => null,
    prompt: async (text: string) => {
      sent.push(text);
    },
    panelAction: async () => {},
    ...over,
  } as unknown as Gateway;
  return { gateway, sent };
}

function mount(gateway: Gateway) {
  const { container } = render(() => Session({ gateway, rail: false }));
  const input = container.querySelector<HTMLTextAreaElement>(".prompt-input")!;
  const type = (text: string, caret = text.length): void => {
    input.value = text;
    input.setSelectionRange(caret, caret);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const key = (name: string): void => {
    input.dispatchEvent(new KeyboardEvent("keydown", { key: name, bubbles: true, cancelable: true }));
  };
  const rows = (): string[] =>
    [...container.querySelectorAll(".slash-row .slash-name")].map((n) => n.textContent ?? "");
  const badges = (): string[] =>
    [...container.querySelectorAll(".slash-badge")].map((n) => n.textContent ?? "");
  return { container, input, type, key, rows, badges };
}

describe("discovering a command in the browser", () => {
  test("a bare slash lists the catalog the shell advertised", () => {
    // The gap this closes: before it, a browser could dispatch `/ship` and had
    // no way to learn `/ship` existed.
    const { gateway } = stub();
    const { rows, type } = mount(gateway);
    expect(rows()).toEqual([]);
    type("/");
    expect(rows()).toEqual(["/compact", "/flush", "/model", "/ship", "/commit"]);
  });

  test("nothing is listed while there is no catalog", () => {
    const { gateway } = stub({ commands: () => [] } as unknown as Partial<Gateway>);
    const { rows, type } = mount(gateway);
    type("/");
    expect(rows()).toEqual([]);
  });

  test("typing filters, and the matched characters are marked", () => {
    const { gateway } = stub();
    const { rows, type, container } = mount(gateway);
    type("/co");
    // Equal on every criterion the wire supports, so the pager's last tiebreak
    // decides: alphabetically by display, which puts `commit` before `compact`.
    expect(rows()).toEqual(["/commit", "/compact"]);
    const marked = [...container.querySelectorAll(".slash-row .slash-match")].map(
      (n) => n.textContent,
    );
    expect(marked[0]).toBe("co");
  });

  test("a plugin's command is badged, and a builtin is not mistaken for one", () => {
    // `4df5f45e` added the badge precisely so these two cannot read alike, and
    // the browser is the client with no other place to learn the difference.
    const { gateway } = stub();
    const { rows, badges, type } = mount(gateway);
    type("/");
    const at = rows().indexOf("/ship");
    expect(badges()[at]).toBe("plugin · deployer");
    expect(badges()[rows().indexOf("/compact")]).toBe("built-in");
    expect(badges()[rows().indexOf("/commit")]).toBe("skill · user");
  });

  test("arrows walk the list and wrap at both ends", () => {
    const { gateway } = stub();
    const { type, key, container } = mount(gateway);
    type("/co");
    const selected = (): string | null =>
      container.querySelector(".slash-row.selected .slash-name")?.textContent ?? null;
    expect(selected()).toBe("/commit");
    key("ArrowDown");
    expect(selected()).toBe("/compact");
    key("ArrowDown");
    expect(selected()).toBe("/commit");
    key("ArrowUp");
    expect(selected()).toBe("/compact");
  });

  test("Enter takes the highlighted row instead of sending", () => {
    const { gateway, sent } = stub();
    const { type, key, input } = mount(gateway);
    type("/mod");
    key("Enter");
    expect(input.value).toBe("/model ");
    expect(input.selectionStart).toBe("/model ".length);
    expect(sent).toEqual([]);
  });

  test("Tab does the same, and the menu closes once the name is complete", () => {
    const { gateway } = stub();
    const { type, key, input, rows } = mount(gateway);
    type("/fl");
    key("Tab");
    expect(input.value).toBe("/flush ");
    // The caret is now past the command token, which is the argument phase.
    expect(rows()).toEqual([]);
  });

  test("the argument hint stands in as the placeholder", () => {
    const { gateway } = stub();
    const { type, key, input } = mount(gateway);
    type("/mod");
    key("Enter");
    expect(input.placeholder).toBe("<model id>");
    type("/model grok-4");
    expect(input.placeholder).toBe("Message this session…");
  });

  test("Escape closes it, and typing on brings it back", () => {
    const { gateway } = stub();
    const { type, key, rows } = mount(gateway);
    type("/co");
    expect(rows()).not.toEqual([]);
    key("Escape");
    expect(rows()).toEqual([]);
    type("/com");
    expect(rows()).not.toEqual([]);
  });

  test("a chosen command is sent as ordinary prompt text", () => {
    // That is not a shortcut: sending `/name args` *is* the dispatch path, and a
    // plugin's command reaches the plugin's own code from it.
    const { gateway, sent } = stub();
    const { type, key, input } = mount(gateway);
    type("/sh");
    key("Enter");
    expect(input.value).toBe("/ship ");
    input.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true, cancelable: true }),
    );
    expect(sent).toEqual(["/ship"]);
  });

  test("clicking a row inserts it", () => {
    const { gateway } = stub();
    const { type, container, input } = mount(gateway);
    type("/co");
    const row = [...container.querySelectorAll<HTMLElement>(".slash-name")]
      .find((n) => n.textContent === "/commit")
      ?.closest(".slash-row");
    row!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
    expect(input.value).toBe("/commit ");
  });

  test("ordinary prose never opens it", () => {
    const { gateway } = stub();
    const { type, rows } = mount(gateway);
    type("what does /compact do?");
    expect(rows()).toEqual([]);
  });
});
