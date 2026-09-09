// A note filed in the agent's memory.
//
// Two things are pinned. The first is what leaves this client: a session id, a
// note and a scope, and **no path or cwd** — the handler takes neither so that
// no caller can aim a write at a directory of its choosing, and a client that
// invented a parameter would be asking for a door the wire deliberately does
// not have. The second is what the surface says afterwards: the note was
// appended, an append racing a hand edit is a lost note, and the file is named
// because the file is the only place that claim can be checked.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";

import type { SocketLike } from "../src/client.ts";
import { Remember } from "../src/components/Remember.tsx";
import { createGateway, type Gateway } from "../src/gateway.ts";
import type { RosterEntry } from "../src/wire.ts";

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

const GLOBAL_FILE = "/home/me/.grok/memory/MEMORY.md";

interface Filed {
  text: string;
  scope: string;
}

function mount(save?: (text: string, scope: string) => Promise<string>) {
  const filed: Filed[] = [];
  const gateway = {
    saveMemoryNote: async (text: string, scope: string) => {
      filed.push({ text, scope });
      return save ? await save(text, scope) : GLOBAL_FILE;
    },
  } as unknown as Gateway;
  let closed = false;
  const { container } = render(() =>
    Remember({ gateway, onClose: () => { closed = true; } }),
  );
  const type = (text: string): void => {
    const note = container.querySelector<HTMLTextAreaElement>(".remember-note")!;
    note.value = text;
    note.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const press = (selector: string): void =>
    container.querySelector<HTMLButtonElement>(selector)!.click();
  return { container, filed, type, press, wasClosed: () => closed };
}

const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 5));

describe("filing a note", () => {
  test("an empty note is not a write the agent has to refuse", () => {
    // The handler answers `invalid_params` for a blank note, and reporting that
    // to a person as a failure of the agent would be this client's fault, not
    // the agent's.
    const { container, filed, press } = mount();
    expect(container.querySelector<HTMLButtonElement>(".remember-save")!.disabled).toBe(true);
    press(".remember-save");
    expect(filed).toEqual([]);
  });

  test("the note goes to the global memory unless the other one is chosen", async () => {
    // What `/remember` writes: the pager sends no scope at all and the field
    // defaults to global.
    const { filed, type, press } = mount();
    type("  the parser is generated, do not edit it  ");
    press(".remember-save");
    await settle();
    expect(filed).toEqual([
      { text: "the parser is generated, do not edit it", scope: "global" },
    ]);
  });

  test("the workspace memory is the wire's own second file, not an invented one", async () => {
    const { container, filed, type, press } = mount(async () => "/home/me/repo/MEMORY.md");
    type("this repo builds with nix");
    [...container.querySelectorAll<HTMLButtonElement>(".remember-scope")]
      .find((button) => button.textContent?.includes("This workspace"))!
      .click();
    press(".remember-save");
    await settle();
    expect(filed[0]?.scope).toBe("workspace");
  });

  test("it says where the note landed, and that a hand edit can still lose it", async () => {
    // The handler is candid: two appends each land whole, but an append racing
    // an editor saving the whole file is a lost note. A surface that said
    // "saved" and closed would be claiming something the agent does not.
    const { container, type, press } = mount();
    type("remember this");
    press(".remember-save");
    await settle();
    expect(container.querySelector(".remember-path")?.textContent).toBe(GLOBAL_FILE);
    expect(container.querySelector(".remember-caveat")?.textContent).toContain("write back");
    expect(container.querySelector(".remember-note")).toBeNull();
  });

  test("a refusal is shown where the note still is, not instead of it", async () => {
    const { container, type, press } = mount(async () => {
      throw new Error("memory write failed: permission denied");
    });
    type("remember this");
    press(".remember-save");
    await settle();
    expect(container.querySelector(".remember-failed")?.textContent).toContain("permission denied");
    expect(container.querySelector<HTMLTextAreaElement>(".remember-note")?.value).toBe(
      "remember this",
    );
  });
});

// ---------------------------------------------------------------------------
// Over the wire
// ---------------------------------------------------------------------------

class Leader implements SocketLike {
  sent: { method?: string; params?: Record<string, unknown> }[] = [];
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    const frame = JSON.parse(data) as {
      id?: number;
      method?: string;
      params?: Record<string, unknown>;
    };
    if (frame.method === undefined) return;
    this.sent.push({ method: frame.method, params: frame.params });
    if (frame.id === undefined) return;
    const results: Record<string, unknown> = {
      initialize: {
        protocolVersion: 1,
        authMethods: [{ id: "xai.api_key", name: "api key" }],
        _meta: { restoredAuthMeta: {} },
      },
      "session/load": {},
      "_x.ai/sessions/list": { result: { sessions: [ENTRY] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
      "_x.ai/session/info": { result: { sessionId: ENTRY.sessionId, cwd: ENTRY.cwd } },
      "_x.ai/session/plan": { result: {} },
      // Bare, not enveloped: `memory.rs` answers through `to_raw_response`,
      // unlike `session/plan` beside it, which wraps in `ExtMethodResult`.
      "_x.ai/memory/note": { path: GLOBAL_FILE },
    };
    const result = results[frame.method];
    if (result === undefined) return;
    queueMicrotask(() => this.receive(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result })));
  }

  close(): void {}

  addEventListener(type: string, listener: (event: never) => void): void {
    const bucket = this.listeners.get(type) ?? [];
    bucket.push(listener);
    this.listeners.set(type, bucket);
    if (type === "open") queueMicrotask(() => (listener as () => void)());
  }

  receive(text: string): void {
    for (const listener of this.listeners.get("message") ?? []) {
      (listener as (event: { data: unknown }) => void)({ data: text });
    }
  }
}

let leader: Leader;
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  leader = new Leader();
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return leader;
  };
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

describe("what a note puts on the wire", () => {
  test("a session, a text and a scope — and nothing that names a directory", async () => {
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
    await gateway.attach(ENTRY);
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(await gateway.saveMemoryNote("nix, not cargo", "global")).toBe(GLOBAL_FILE);
    const params = leader.sent.find((frame) => frame.method === "_x.ai/memory/note")?.params;
    expect(params).toEqual({
      sessionId: ENTRY.sessionId,
      text: "nix, not cargo",
      scope: "global",
    });
  });
});
