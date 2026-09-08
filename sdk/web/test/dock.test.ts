// The gate the agent broadcasts, and what this client does with it.
//
// `dock_enabled` rides `x.ai/settings/update`, which the shell sends with
// `forward_fire_and_forget` — to every attached client, not to the terminal that
// caused the refresh. This client used to know four notification methods and
// none of them was this one, so a cohort the dock had been turned on for saw no
// sign of it in a browser while the terminal beside it drew it.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";

import type { SocketLike } from "../src/client.ts";
import { createGateway } from "../src/gateway.ts";

class FakeSocket implements SocketLike {
  private readonly listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    const frame = JSON.parse(data) as { id?: number; method?: string };
    if (frame.id === undefined || frame.method === undefined) return;
    const results: Record<string, unknown> = {
      initialize: { protocolVersion: 1, authMethods: [], _meta: { restoredAuthMeta: {} } },
      "_x.ai/sessions/list": { result: { sessions: [] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
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

  /** What the shell pushes after a remote settings refresh. */
  settingsUpdate(params: Record<string, unknown>): void {
    this.receive(JSON.stringify({ jsonrpc: "2.0", method: "_x.ai/settings/update", params }));
  }
}

const RealWebSocket = globalThis.WebSocket;
let socket: FakeSocket;

beforeEach(() => {
  socket = new FakeSocket();
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return socket;
  };
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

describe("the dock gate on the wire", () => {
  test("starts as `not told`, which is not the same as `told no`", async () => {
    // The notification is sent when remote settings are refreshed, and that may
    // never happen while a page is open. A client that read the silence as
    // `false` would be inventing an answer the agent did not give — and the
    // browser's own layer sits above it, so the difference is load-bearing.
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "s");
    expect(gateway.dockEnabled()).toBeNull();
  });

  test("takes the flag the agent broadcasts to every client", async () => {
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "s");
    socket.settingsUpdate({ dock_enabled: true, tips: ["ignored"] });
    expect(gateway.dockEnabled()).toBe(true);
    socket.settingsUpdate({ dock_enabled: false });
    expect(gateway.dockEnabled()).toBe(false);
  });

  test("an update that says nothing about the dock leaves it alone", async () => {
    // Every field of that notification is an `Option`, and a push about
    // announcements carries `dock_enabled: null`. Reading that as "off" would
    // turn an unrelated refresh into a layout change.
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "s");
    socket.settingsUpdate({ dock_enabled: true });
    socket.settingsUpdate({ announcements: [], dock_enabled: null });
    expect(gateway.dockEnabled()).toBe(true);
  });

  test("and it does not survive a move to another agent", async () => {
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "s");
    socket.settingsUpdate({ dock_enabled: true });
    socket = new FakeSocket();
    await gateway.connect("ws://127.0.0.1:2430/ws", "s");
    expect(gateway.dockEnabled()).toBeNull();
  });
});
