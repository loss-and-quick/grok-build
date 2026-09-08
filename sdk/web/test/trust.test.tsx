import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";

import { INTERNAL_ERROR, type SocketLike } from "../src/client.ts";
import { FolderTrustCard } from "../src/components/FolderTrustCard.tsx";
import { createGateway, type Gateway, type PendingFolderTrust } from "../src/gateway.ts";
import { FOLDER_TRUST_DISMISSED } from "../src/wire.ts";

const REQUEST = {
  sessionId: "sess-1",
  cwd: "/home/me/repo/crates/thing",
  workspace: "/home/me/repo",
  configKinds: ["mcp", "hooks", "lsp"],
};

/**
 * A socket that answers the three calls `connect` makes, so a real
 * {@link createGateway} can be driven without a leader.
 *
 * The point of going through the real gateway rather than a hand-built handler
 * is that the bytes this test asserts on are the bytes the agent parses — the
 * `_` prefix, the bare (unwrapped) response body, and the error that stands for
 * a dismissal all come from the code under test rather than from the test.
 */
class FakeSocket implements SocketLike {
  sent: string[] = [];
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    this.sent.push(data);
    const frame = JSON.parse(data) as { id?: number; method?: string };
    if (frame.id === undefined || frame.method === undefined) return;
    const results: Record<string, unknown> = {
      initialize: { protocolVersion: 1, _meta: { currentWorkingDirectory: "/home/me/repo" } },
      "_x.ai/sessions/list": { result: { sessions: [] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
    };
    const result = results[frame.method];
    if (result !== undefined) {
      queueMicrotask(() =>
        this.receive(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result })),
      );
    }
  }

  close(): void {}

  addEventListener(type: string, listener: (event: never) => void): void {
    const bucket = this.listeners.get(type) ?? [];
    bucket.push(listener);
    this.listeners.set(type, bucket);
    // A real socket is already opening by the time a listener is attached, so
    // `open` arrives after registration rather than before it.
    if (type === "open") queueMicrotask(() => (listener as () => void)());
  }

  /** Deliver a frame as if the gateway had sent it. */
  receive(text: string): void {
    for (const listener of this.listeners.get("message") ?? []) {
      (listener as (event: { data: unknown }) => void)({ data: text });
    }
  }

  replies(): Record<string, unknown>[] {
    return this.sent
      .map((s) => JSON.parse(s) as Record<string, unknown>)
      .filter((f) => f["method"] === undefined);
  }
}

let socket: FakeSocket;
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  socket = new FakeSocket();
  // `createGateway` opens its own socket, which is the wiring under test.
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return socket;
  };
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

async function connected(): Promise<Gateway> {
  const gateway = createGateway();
  await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
  socket.receive(
    JSON.stringify({
      jsonrpc: "2.0",
      id: 99,
      method: "_x.ai/folder_trust/request",
      params: REQUEST,
    }),
  );
  await new Promise((resolve) => setTimeout(resolve, 10));
  return gateway;
}

function lastReply(): Record<string, unknown> {
  return socket.replies().at(-1)!;
}

describe("answering the folder-trust request", () => {
  test("the card carries what the agent sent, and waits", async () => {
    const gateway = await connected();
    expect(gateway.folderTrusts).toHaveLength(1);
    expect(gateway.folderTrusts[0]!.workspace).toBe("/home/me/repo");
    expect(gateway.folderTrusts[0]!.configKinds).toEqual(["mcp", "hooks", "lsp"]);
    // Nothing has been answered: no reply frame has gone out at all.
    expect(socket.replies()).toHaveLength(0);
  });

  test("trust replies with the bare object the agent parses", async () => {
    const gateway = await connected();
    gateway.folderTrusts[0]!.decide("trust");
    await new Promise((resolve) => setTimeout(resolve, 10));
    // Bare, not an `ExtMethodResult` envelope: this response is read with
    // `serde_json::from_str::<FolderTrustResponse>` straight off the payload,
    // so a `{result: …}` wrapper here would decode as a reject.
    expect(lastReply()).toEqual({ jsonrpc: "2.0", id: 99, result: { outcome: "trust" } });
    expect(gateway.folderTrusts).toHaveLength(0);
  });

  test("reject is an explicit answer, not a silence", async () => {
    const gateway = await connected();
    gateway.folderTrusts[0]!.decide("reject");
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(lastReply()).toEqual({ jsonrpc: "2.0", id: 99, result: { outcome: "reject" } });
  });

  test("dismissal is an error reply, because the wire has no third outcome", async () => {
    // The agent's enum is `#[serde(other)] Reject`, so `{"outcome":"dismiss"}`
    // would decode as a reject — and a reject keeps the dedup key, which is the
    // one thing dismissal is meant not to do. An error reaches the agent as
    // "not a decision": it stays gated and releases the key, so the next
    // session in this workspace asks again.
    const gateway = await connected();
    gateway.folderTrusts[0]!.dismiss();
    await new Promise((resolve) => setTimeout(resolve, 10));
    const reply = lastReply();
    expect(reply["result"]).toBeUndefined();
    const error = reply["error"] as { code: number; message: string };
    expect(error.code).toBe(INTERNAL_ERROR);
    expect(error.message).toContain(FOLDER_TRUST_DISMISSED);
    expect(gateway.folderTrusts).toHaveLength(0);
  });
});

describe("the card", () => {
  function mount(over: Partial<PendingFolderTrust> = {}) {
    const chosen: string[] = [];
    const pending: PendingFolderTrust = {
      ...REQUEST,
      decide: (outcome) => chosen.push(outcome),
      dismiss: () => chosen.push("dismiss"),
      ...over,
    };
    return { chosen, ...render(() => <FolderTrustCard pending={pending} />) };
  }

  test("names the workspace, which is what the grant covers", () => {
    const { container } = mount();
    expect(container.querySelector(".trust-title")?.textContent).toContain("/home/me/repo");
  });

  test("says when the grant is wider than the session's own root", () => {
    // The workspace key can be an ancestor of `cwd`. Agreeing then trusts more
    // than the directory this session sits in, and the card must not hide that.
    const { container } = mount();
    expect(container.querySelector(".trust-path")?.textContent).toBe(
      "/home/me/repo/crates/thing",
    );
  });

  test("stays quiet about scope when the two are the same", () => {
    const { container } = mount({ cwd: "/home/me/repo" });
    expect(container.querySelector(".trust-scope")).toBeNull();
  });

  test("lists the config kinds the agent found, as sent", () => {
    const { container } = mount();
    expect([...container.querySelectorAll(".trust-kind")].map((n) => n.textContent)).toEqual([
      "mcp",
      "hooks",
      "lsp",
    ]);
  });

  test("all three outcomes are offered, and each does its own thing", () => {
    for (const [selector, expected] of [
      [".trust-trust", "trust"],
      [".trust-reject", "reject"],
      [".trust-dismiss", "dismiss"],
    ] as const) {
      const { container, chosen } = mount();
      container.querySelector<HTMLButtonElement>(selector)!.click();
      expect(chosen).toEqual([expected]);
    }
  });
});
