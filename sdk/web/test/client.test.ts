import { describe, expect, test } from "bun:test";

import {
  GatewayClient,
  gatewayUrl,
  METHOD_NOT_FOUND,
  unprefix,
  unwrapExt,
  watchLink,
  type SocketLike,
} from "../src/client.ts";

class FakeSocket implements SocketLike {
  sent: string[] = [];
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {}

  addEventListener(type: string, listener: (event: never) => void): void {
    const bucket = this.listeners.get(type) ?? [];
    bucket.push(listener);
    this.listeners.set(type, bucket);
  }

  fire(type: "open" | "close" | "error"): void {
    for (const listener of this.listeners.get(type) ?? []) (listener as () => void)();
  }

  frames(): Record<string, unknown>[] {
    return this.sent.map((s) => JSON.parse(s) as Record<string, unknown>);
  }
}

async function connected(): Promise<{ socket: FakeSocket; client: GatewayClient }> {
  const socket = new FakeSocket();
  const client = new GatewayClient(() => socket);
  const opening = client.connect();
  socket.fire("open");
  await opening;
  return { socket, client };
}

describe("gateway url", () => {
  test("the secret rides the query string, because a browser cannot set headers", () => {
    expect(gatewayUrl("ws://127.0.0.1:2420/ws", "s3cret")).toBe(
      "ws://127.0.0.1:2420/ws?server-key=s3cret",
    );
  });

  test("an existing query is preserved, and a stale key replaced", () => {
    expect(gatewayUrl("ws://127.0.0.1:2420/ws?server-key=old", "new")).toContain("server-key=new");
  });
});

describe("extension framing", () => {
  test("an ext request goes out as a plain `_`-prefixed method with inline params", async () => {
    // The ACP crate encodes `format!(\"_{}\", method)`; there is no `ext_method`
    // envelope on the wire. Getting this wrong is a silent method-not-found.
    const { socket, client } = await connected();
    void client.ext("x.ai/sessions/list", {});
    const frame = socket.frames()[0]!;
    expect(frame["method"]).toBe("_x.ai/sessions/list");
    expect(frame["params"]).toEqual({});
    expect(frame["jsonrpc"]).toBe("2.0");
  });

  test("params are always sent, because the decoder rejects a missing params", async () => {
    const { socket, client } = await connected();
    void client.ext("x.ai/settings/list", {});
    expect(Object.keys(socket.frames()[0]!)).toContain("params");
  });

  test("incoming notification methods are unprefixed for dispatch", async () => {
    const { client } = await connected();
    const seen: string[] = [];
    client.onNotification((method) => seen.push(method));
    client.receive('{"jsonrpc":"2.0","method":"_x.ai/sessions/changed","params":{}}');
    client.receive('{"jsonrpc":"2.0","method":"session/update","params":{}}');
    expect(seen).toEqual(["x.ai/sessions/changed", "session/update"]);
    expect(unprefix("_x.ai/foo")).toBe("x.ai/foo");
    expect(unprefix("session/update")).toBe("session/update");
  });
});

describe("extension response unwrapping", () => {
  test("the wrapped form is unwrapped and the raw form passed through", () => {
    // `x.ai/sessions/list` wraps; `x.ai/settings/list` does not. Both are real.
    expect(unwrapExt({ result: { sessions: [] } })).toEqual({ sessions: [] });
    expect(unwrapExt({ catalog: { rows: [] } })).toEqual({ catalog: { rows: [] } });
  });

  test("an error inside the wrapper is raised, not returned as data", () => {
    expect(() => unwrapExt({ result: null, error: { message: "nope" } })).toThrow();
  });

  test("`ext` unwraps the roster to its payload", async () => {
    const { socket, client } = await connected();
    const pending = client.ext("x.ai/sessions/list", {});
    const id = socket.frames()[0]!["id"];
    client.receive(JSON.stringify({ jsonrpc: "2.0", id, result: { result: { sessions: [1] } } }));
    expect(await pending).toEqual({ sessions: [1] });
  });
});

describe("request and response plumbing", () => {
  test("a response resolves the matching id only", async () => {
    const { socket, client } = await connected();
    const first = client.request("initialize", {});
    const second = client.request("session/load", {});
    const ids = socket.frames().map((f) => f["id"]);
    client.receive(JSON.stringify({ jsonrpc: "2.0", id: ids[1], result: { b: 2 } }));
    expect(await second).toEqual({ b: 2 });
    client.receive(JSON.stringify({ jsonrpc: "2.0", id: ids[0], result: { a: 1 } }));
    expect(await first).toEqual({ a: 1 });
  });

  test("a JSON-RPC error rejects rather than resolving with an error object", async () => {
    const { socket, client } = await connected();
    const pending = client.request("session/new", {});
    const id = socket.frames()[0]!["id"];
    client.receive(
      JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32000, message: "Authentication required" } }),
    );
    expect(pending).rejects.toThrow("Authentication required");
  });

  test("an unhandled agent request is refused, never left silent", async () => {
    // Silence parks the session actor: the permission round-trip has no timeout
    // on the agent side, so an ignored request hangs the turn indefinitely.
    const { socket, client } = await connected();
    client.onRequest(() => undefined);
    client.receive('{"jsonrpc":"2.0","id":99,"method":"terminal/create","params":{}}');
    await Promise.resolve();
    const reply = socket.frames().at(-1)!;
    expect(reply["id"]).toBe(99);
    expect((reply["error"] as { code: number }).code).toBe(METHOD_NOT_FOUND);
  });

  test("a handled agent request is answered with its result", async () => {
    const { socket, client } = await connected();
    client.onRequest((method) =>
      method === "session/request_permission"
        ? Promise.resolve({ outcome: { outcome: "selected", optionId: "allow-once" } })
        : undefined,
    );
    client.receive(
      '{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{"options":[]}}',
    );
    await Promise.resolve();
    await Promise.resolve();
    const reply = socket.frames().at(-1)!;
    expect(reply["id"]).toBe(7);
    expect(reply["result"]).toEqual({ outcome: { outcome: "selected", optionId: "allow-once" } });
  });

  test("a closed socket rejects everything still in flight", async () => {
    const { socket, client } = await connected();
    const pending = client.request("session/prompt", {});
    socket.fire("close");
    expect(pending).rejects.toThrow("closed");
  });

  test("malformed frames are dropped, not thrown", async () => {
    const { client } = await connected();
    expect(() => client.receive("not json")).not.toThrow();
    expect(() => client.receive("null")).not.toThrow();
    expect(() => client.receive('{"jsonrpc":"2.0"}')).not.toThrow();
  });
});

describe("what the socket says about itself", () => {
  test("a link that closes reports closed, so nothing above it can keep claiming connected", async () => {
    // The bug this stands against: the reactive layer's `connection()` stayed
    // `"connected"` after the socket died, because nothing told it otherwise.
    const seen: string[] = [];
    const stop = watchLink((_, state) => seen.push(state));
    const { socket, client } = await connected();
    expect(client.linkState()).toBe("open");
    socket.fire("close");
    expect(client.linkState()).toBe("closed");
    expect(seen).toEqual(["opening", "open", "closed"]);
    stop();
  });

  test("each state is announced once, however many times the socket repeats itself", async () => {
    const seen: string[] = [];
    const stop = watchLink((_, state) => seen.push(state));
    const { socket } = await connected();
    socket.fire("open");
    socket.fire("close");
    socket.fire("close");
    expect(seen).toEqual(["opening", "open", "closed"]);
    stop();
  });

  test("the client is named, because a replaced socket's close arrives after its replacement", async () => {
    // A reconnect hangs up the old socket before opening the new one, and the
    // old `close` lands afterwards. A watcher that could not tell them apart
    // would read the new link as dead the instant it came up.
    const events: [GatewayClient, string][] = [];
    const stop = watchLink((client, state) => events.push([client, state]));
    const old = await connected();
    const fresh = await connected();
    old.socket.fire("close");
    expect(events.at(-1)![0]).toBe(old.client);
    expect(events.at(-1)![0]).not.toBe(fresh.client);
    stop();
  });
});
