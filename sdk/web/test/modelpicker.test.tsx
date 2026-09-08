// The `/model` screen, driven through the real gateway over a fake leader.
//
// The claim being tested is not "a list renders" but "the browser makes the two
// calls the terminal makes, and only those". The trap is that `/model <name>`
// in the pager emits *two* effects — `Effect::PersistSetting { key:
// "default_model" }` and `Effect::SwitchModel`
// (`pager/src/app/dispatch/settings/setters.rs:1776-1801`) — while the
// shell-side setter behind that key only writes the file
// (`xai-grok-shell/src/util/config/settings_apply.rs:233-240`). Send one half
// and the browser saves a preference without switching the session, or switches
// it and forgets; neither shows up anywhere but the next session.
//
// The other half of the trap is the opposite: `/model <name> <effort>` must NOT
// persist, because the effort is session-scoped and rides in `_meta` on the same
// `set_model` (`pager/src/app/effects/mod.rs:1846-1873`).
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { cleanup, render } from "@solidjs/testing-library";

import type { SocketLike } from "../src/client.ts";
import { ModelPicker } from "../src/components/ModelPicker.tsx";
import { createGateway, type Gateway } from "../src/gateway.ts";
import type { RosterEntry } from "../src/wire.ts";

const ENTRY: RosterEntry = {
  sessionId: "sess-1",
  cwd: "/home/me/repo",
  isWorktree: false,
  yolo: false,
  activity: "idle",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

/** Two models, one of each kind, plus a server-defined effort list. */
const MODELS = {
  currentModelId: "fast",
  availableModels: [
    {
      modelId: "thinker",
      name: "Thinker",
      description: "Reasons a lot",
      _meta: {
        supportsReasoningEffort: true,
        reasoningEffort: "high",
        reasoningEfforts: [
          { id: "deep", value: "xhigh", label: "Deep", description: "Slowest" },
          { id: "quick", value: "low", label: "Quick", description: "Fastest" },
        ],
      },
    },
    { modelId: "fast", name: "Fast", description: "No reasoning", _meta: {} },
  ],
};

class FakeSocket implements SocketLike {
  sent: Record<string, unknown>[] = [];
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    const frame = JSON.parse(data) as { id?: number; method?: string; params?: unknown };
    this.sent.push(frame as Record<string, unknown>);
    if (frame.id === undefined || frame.method === undefined) return;
    const results: Record<string, unknown> = {
      initialize: { protocolVersion: 1, _meta: { currentWorkingDirectory: ENTRY.cwd } },
      "_x.ai/sessions/list": { result: { sessions: [ENTRY] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
      // The reply this client used to throw away whole.
      "session/load": { models: MODELS, configOptions: [] },
      "session/set_model": { _meta: { model: "ok" } },
      "_x.ai/settings/set": { applied: true },
    };
    const result = results[frame.method];
    if (result !== undefined) {
      queueMicrotask(() => this.receive(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result })));
    }
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

  /** Outbound requests by method, in the order they went out. */
  calls(method: string): Record<string, unknown>[] {
    return this.sent.filter((f) => f["method"] === method);
  }
}

let socket: FakeSocket;
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  // Take the previous test's page down first. An open picker holds a *document*
  // keydown listener that swallows the arrows to walk its rows
  // (`ModelPicker.tsx`), so a suite that leaves one mounted goes on eating
  // arrow keys in whatever suite runs after it — the leak is here, not there.
  cleanup();
  socket = new FakeSocket();
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return socket;
  };
});

afterAll(() => {
  cleanup();
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

const settle = (ms = 20): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

async function attached(): Promise<Gateway> {
  const gateway = createGateway();
  await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
  await gateway.attach(ENTRY);
  await settle();
  return gateway;
}

function mount(gateway: Gateway) {
  const { container } = render(() => ModelPicker({ gateway }));
  const rows = (): HTMLButtonElement[] => [
      ...container.querySelectorAll<HTMLButtonElement>(".model-row"),
    ];
  return {
    container,
    header: () => container.querySelector(".model-current")?.textContent ?? "",
    open: () => (container.querySelector(".model-current") as HTMLButtonElement)?.click(),
    names: () => [...container.querySelectorAll(".model-row-name")].map((n) => n.textContent ?? ""),
    title: () => container.querySelector(".picker-title")?.textContent ?? "",
    note: () => container.querySelector(".model-note")?.textContent ?? "",
    pick: (name: string) => {
      const at = [...container.querySelectorAll(".model-row-name")].findIndex(
        (n) => (n.textContent ?? "").startsWith(name),
      );
      rows()[at]!.click();
    },
    filter: (text: string) => {
      const input = container.querySelector(".model-filter") as HTMLInputElement;
      input.value = text;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    },
  };
}

describe("the catalog was already on the wire", () => {
  test("`session/load`'s reply is what fills the picker", async () => {
    const gateway = await attached();
    expect(gateway.models()?.currentModelId).toBe("fast");
    const view = mount(gateway);
    expect(view.header()).toContain("Fast");
    view.open();
    expect(view.names()).toEqual(["Thinker", "Fast (current)"]);
  });

  test("attaching elsewhere clears it rather than showing the last session's", async () => {
    // A session whose reply carried no catalog and a session whose catalog is
    // the previous one's are the same picture, and only one of them is true.
    const gateway = await attached();
    const other = { ...ENTRY, sessionId: "sess-2" };
    const loading = gateway.attach(other);
    expect(gateway.models()).toBeNull();
    await loading;
  });
});

describe("applying a choice", () => {
  test("a model with no effort switches AND is remembered — both halves", async () => {
    const gateway = await attached();
    const view = mount(gateway);
    view.open();
    view.pick("Fast");
    await settle();

    expect(socket.calls("session/set_model")).toHaveLength(1);
    expect(socket.calls("session/set_model")[0]!["params"]).toEqual({
      sessionId: "sess-1",
      modelId: "fast",
    });
    expect(socket.calls("_x.ai/settings/set")[0]!["params"]).toEqual({
      sessionId: "sess-1",
      key: "default_model",
      value: "fast",
    });
  });

  test("the write comes second, and not at all when the switch fails", async () => {
    // Order matters: a remembered default for a model the agent refused would
    // start the *next* session on a model this one could not use.
    const gateway = await attached();
    const view = mount(gateway);
    // Answer `set_model` with an error this time.
    const original = socket.send.bind(socket);
    socket.send = (data: string) => {
      const frame = JSON.parse(data) as { id?: number; method?: string };
      socket.sent.push(frame as Record<string, unknown>);
      if (frame.method === "session/set_model") {
        queueMicrotask(() =>
          socket.receive(
            JSON.stringify({
              jsonrpc: "2.0",
              id: frame.id,
              error: { code: -32602, message: "model not allowed" },
            }),
          ),
        );
        return;
      }
      original(data);
    };
    view.open();
    view.pick("Fast");
    await settle();
    expect(socket.calls("_x.ai/settings/set")).toHaveLength(0);
    expect(gateway.status()).toContain("could not switch model");
  });

  test("a reasoning model opens the effort phase instead of applying", async () => {
    // The terminal's decision, not a keyboard affordance: `chains_to_effort`
    // (`pager/src/app/modals.rs:684-706`) replaces the rows rather than
    // dispatching. Nothing may go out until a level is chosen.
    const gateway = await attached();
    const view = mount(gateway);
    view.open();
    view.pick("Thinker");
    await settle();
    expect(socket.calls("session/set_model")).toHaveLength(0);
    expect(view.title()).toContain("Thinker");
    expect(view.names()).toEqual(["Deep", "Quick"]);
    expect(view.note()).toContain("this session only");
  });

  test("an effort switch sends `_meta` and does NOT persist", async () => {
    const gateway = await attached();
    const view = mount(gateway);
    view.open();
    view.pick("Thinker");
    view.pick("Deep");
    await settle();
    expect(socket.calls("session/set_model")[0]!["params"]).toEqual({
      sessionId: "sess-1",
      modelId: "thinker",
      // `xhigh`, the option's canonical `value` — never `deep`, its menu id.
      _meta: { reasoningEffort: "xhigh" },
    });
    expect(socket.calls("_x.ai/settings/set")).toHaveLength(0);
  });

  test("a refused write is reported as a refusal, not as a failure", async () => {
    // `update_config`'s `stat` protects a declaratively configured machine from
    // a browser exactly as it does from a terminal, and the sentence naming the
    // file is the shell's.
    const gateway = await attached();
    const view = mount(gateway);
    const original = socket.send.bind(socket);
    socket.send = (data: string) => {
      const frame = JSON.parse(data) as { id?: number; method?: string };
      if (frame.method === "_x.ai/settings/set") {
        socket.sent.push(frame as Record<string, unknown>);
        queueMicrotask(() =>
          socket.receive(
            JSON.stringify({
              jsonrpc: "2.0",
              id: frame.id,
              result: {
                applied: false,
                refusal: { kind: "readOnlyConfig", message: "config.toml is read-only" },
              },
            }),
          ),
        );
        return;
      }
      original(data);
    };
    view.open();
    view.pick("Fast");
    await settle();
    expect(gateway.status()).toContain("config.toml is read-only");
  });
});

describe("a switch made from another client", () => {
  test("`model_changed` moves this picker", async () => {
    // The leader broadcasts it to every subscriber, so a terminal pressing
    // Ctrl+M on the same session is what this test stands for.
    const gateway = await attached();
    const view = mount(gateway);
    socket.receive(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "_x.ai/session_notification",
        params: {
          sessionId: ENTRY.sessionId,
          update: { sessionUpdate: "model_changed", model_id: "thinker", reasoning_effort: "low" },
        },
      }),
    );
    await settle();
    expect(view.header()).toContain("Thinker");
    expect(view.header()).toContain("low");
  });
});
