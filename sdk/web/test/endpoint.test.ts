// Changing which gateway this page is on.
//
// One live socket, switched in sequence — the client has never held two, and
// `connect` closes the previous one on its first line. What it did *not* do was
// let go of the session that socket was showing, so instance A's transcript,
// its subagents and its model catalog stayed on screen underneath instance B's
// roster, with nothing saying the two came from different machines. `disconnect`
// had always cleared it, so hanging up and then connecting elsewhere behaved
// differently from connecting elsewhere.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";

import type { SocketLike } from "../src/client.ts";
import { createGateway } from "../src/gateway.ts";

const A = "ws://127.0.0.1:2420/ws";
const B = "ws://127.0.0.1:2430/ws";

/** Which sessions each address answers with; the ids never overlap. */
const SESSIONS: Record<string, string> = { "2420": "sess-a", "2430": "sess-b" };

function row(sessionId: string) {
  return {
    sessionId,
    cwd: "/home/me/repo",
    isWorktree: false,
    yolo: false,
    activity: "idle",
    resident: true,
    lastChangeUnixMs: 1,
    origin: { kind: "local" },
  };
}

/** A leader that names its own sessions after the port it answers on. */
class FakeSocket implements SocketLike {
  closed = false;
  private readonly listeners = new Map<string, ((event: never) => void)[]>();
  private readonly sessionId: string;

  constructor(url: string) {
    this.sessionId = SESSIONS[new URL(url).port] ?? "sess-?";
  }

  send(data: string): void {
    const frame = JSON.parse(data) as { id?: number; method?: string };
    if (frame.id === undefined || frame.method === undefined) return;
    const results: Record<string, unknown> = {
      initialize: {
        protocolVersion: 1,
        authMethods: [],
        _meta: { currentWorkingDirectory: "/home/me/repo", restoredAuthMeta: {} },
      },
      "_x.ai/sessions/list": { result: { sessions: [row(this.sessionId)] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
      "session/load": {},
    };
    const result = results[frame.method];
    if (result !== undefined) {
      queueMicrotask(() =>
        this.receive(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result })),
      );
    }
  }

  close(): void {
    this.closed = true;
  }

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

const RealWebSocket = globalThis.WebSocket;
let opened: FakeSocket[] = [];

beforeEach(() => {
  opened = [];
  (globalThis as { WebSocket: unknown }).WebSocket = function (url: string) {
    const socket = new FakeSocket(url);
    opened.push(socket);
    return socket;
  };
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

const settle = (ms = 30): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

describe("connecting to a different gateway", () => {
  test("does not leave the previous instance's session on screen", async () => {
    const gateway = createGateway();
    await gateway.connect(A, "s");
    await gateway.attach(gateway.roster.get("sess-a")!);
    await settle();
    expect(gateway.attached()?.entry.sessionId).toBe("sess-a");

    await gateway.connect(B, "s");
    await settle();
    // The roster is B's, so the session must be too — and B has never heard of
    // `sess-a`. Anything still attached here is a transcript from another
    // machine sitting under this machine's list.
    expect(gateway.attached()).toBeNull();
    expect(gateway.roster.get("sess-a")).toBeUndefined();
    expect(gateway.roster.get("sess-b")).toBeDefined();
  });

  test("and the model catalog goes with it", async () => {
    // `models` is read off the `session/load` reply, so a stale one offers the
    // previous leader's model list for a session that is not on screen.
    const gateway = createGateway();
    await gateway.connect(A, "s");
    await gateway.attach(gateway.roster.get("sess-a")!);
    await settle();
    await gateway.connect(B, "s");
    await settle();
    expect(gateway.models()).toBeNull();
  });

  test("attaching on the new socket still works", async () => {
    // The claim `attach` keeps against a double `session/load` is keyed on the
    // socket as well as the session; clearing it must not leave the next
    // attach refused because a dead socket once held it.
    const gateway = createGateway();
    await gateway.connect(A, "s");
    await gateway.attach(gateway.roster.get("sess-a")!);
    await settle();
    await gateway.connect(B, "s");
    await gateway.attach(gateway.roster.get("sess-b")!);
    await settle();
    expect(gateway.attached()?.entry.sessionId).toBe("sess-b");
  });
});
