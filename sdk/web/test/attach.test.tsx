// Attaching once, however many things ask for it.
//
// `session/load` is the one request in this client that is not idempotent: it
// makes the agent replay the whole transcript to the asking client
// (`xai-grok-shell/src/agent/mvp_agent/replay.rs:190`, unicast to that client by
// `_meta["x.ai/leaderClientId"]`, `leader/server.rs:2135-2176`). Two of them on
// one socket therefore put every turn on the page twice — and because the second
// `attach` installs a fresh transcript while the first load's replay is still in
// flight, *both* replays land in it, so nothing about the doubling is visible in
// either request on its own.
//
// It had two askers and they overlapped exactly once: a page opened straight at
// `/s/<id>` fires `SessionRoute`'s effect from inside `connect` (the roster
// arrives there), and then `createLink` calls `resume` because the connect
// succeeded. So the case under test is a *route*, not a function call, and these
// tests drive the real `App` through a `MemoryRouter` rather than calling
// `attach` twice by hand — the latter would pass against the bug.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { cleanup, render } from "@solidjs/testing-library";

import { App, Home, SessionRoute } from "../src/App.tsx";
import type { SocketLike } from "../src/client.ts";
import { remember } from "../src/gateway.ts";

const SESSION = "sess-1";
const CWD = "/home/me/repo";

/** How long the fake leader takes to answer, so a reply is never same-tick. */
const REPLY_MS = 15;

const ROSTER_ROW = {
  sessionId: SESSION,
  cwd: CWD,
  isWorktree: false,
  yolo: false,
  activity: "idle",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

/**
 * A leader that answers the handshake and replays one turn per `session/load`.
 *
 * The replay is the whole point: a socket that only acknowledged the load would
 * pass whether the client asked once or twice.
 */
class FakeSocket implements SocketLike {
  sent: string[] = [];
  loads = 0;
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    this.sent.push(data);
    const frame = JSON.parse(data) as { id?: number; method?: string };
    if (frame.id === undefined || frame.method === undefined) return;
    if (frame.method === "session/load") {
      this.loads += 1;
      // On a timer, not a microtask: a real replay lands after a round trip,
      // i.e. after the *second* attach has already installed its transcript.
      // That is what made both copies land in one view, and a fake that answers
      // synchronously reproduces the extra request without the visible fault.
      setTimeout(() => this.replayOneTurn(frame.id!), REPLY_MS);
      return;
    }
    const results: Record<string, unknown> = {
      initialize: {
        protocolVersion: 1,
        authMethods: [{ id: "xai.api_key", name: "xai.api_key" }],
        _meta: { currentWorkingDirectory: CWD, restoredAuthMeta: {} },
      },
      "_x.ai/sessions/list": { result: { sessions: [ROSTER_ROW] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
    };
    const result = results[frame.method];
    if (result !== undefined) {
      queueMicrotask(() => this.receive(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result })));
    }
  }

  /** One user turn and one reply, then the response — the order the leader keeps. */
  private replayOneTurn(id: number): void {
    for (const [role, text] of [
      ["user_message_chunk", "ping"],
      ["agent_message_chunk", "pong"],
    ] as const) {
      this.receive(
        JSON.stringify({
          jsonrpc: "2.0",
          method: "session/update",
          params: {
            sessionId: SESSION,
            update: { sessionUpdate: role, content: { type: "text", text } },
            _meta: { isReplay: true },
          },
        }),
      );
    }
    this.receive(JSON.stringify({ jsonrpc: "2.0", id, result: {} }));
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

let socket: FakeSocket;
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  // Unmount the previous test's page first. `App` reads the remembered
  // instances at mount and writes them back as it connects, and two of them
  // alive at once means the one being torn down saves its list over the store
  // this one has just cleared — a browser has one page, and so does this.
  cleanup();
  socket = new FakeSocket();
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return socket;
  };
  localStorage.clear();
  // What a reload looks like: the address and secret are remembered, so `App`'s
  // `onMount` connects on its own — which is the path that used to attach twice.
  remember("url", "ws://127.0.0.1:2420/ws");
  remember("secret", "secret");
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

/**
 * The page as a browser opens it: already at `path`, with nothing having
 * navigated there.
 *
 * A memory history rather than the DOM's, because arriving at a URL and
 * clicking to it are the two cases and only the first one had the bug — and
 * because happy-dom's document sits at `about:blank`, where the router's own
 * anchor handling throws.
 */
function openAt(path: string) {
  const history = createMemoryHistory();
  history.set({ value: path, replace: true });
  const { container } = render(() => (
    <MemoryRouter root={App} history={history}>
      <Route path="/" component={Home} />
      <Route path="/s/:sessionId" component={SessionRoute} />
    </MemoryRouter>
  ));
  return container;
}

const settle = (ms = 120): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

describe("a page opened at a session URL", () => {
  test("asks the leader to load it exactly once", async () => {
    // Two askers, one ask. Before the guard this was two, and the second one
    // is not visible anywhere in the client's own state — only in what the
    // leader sends back.
    openAt(`/s/${SESSION}`);
    await settle();
    expect(socket.loads).toBe(1);
  });

  test("and shows the turn once, not twice", async () => {
    // The assertion the report was written from. Both replays would land in the
    // transcript the *second* attach installed, so this is what a person sees:
    // the whole conversation duplicated, the user's own turn included.
    const root = openAt(`/s/${SESSION}`);
    await settle();
    expect([...root.querySelectorAll(".message-user .message-text")].map((n) => n.textContent)).toEqual([
      "ping",
    ]);
    expect(
      [...root.querySelectorAll(".message-assistant .message-text")].map((n) => n.textContent),
    ).toEqual(["pong"]);
  });

  test("a roster upsert for the attached session does not re-ask", async () => {
    // The route effect tracks the roster row, so every `x.ai/sessions/changed`
    // for this session re-runs it. That is fine and must stay a no-op.
    openAt(`/s/${SESSION}`);
    await settle();
    socket.receive(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "_x.ai/sessions/changed",
        params: { upserted: [{ ...ROSTER_ROW, activity: "working", lastChangeUnixMs: 2 }] },
      }),
    );
    await settle(40);
    expect(socket.loads).toBe(1);
  });
});
