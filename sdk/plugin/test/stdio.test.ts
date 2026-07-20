import { describe, expect, test } from "bun:test";

import {
  JsonRpcEndpoint,
  JsonRpcRemoteError,
  LineReader,
  RpcTimeoutError,
  encodeLine,
} from "../src/stdio.ts";
import { MemoryByteReader, MemoryByteWriter } from "./helpers/memory-stream.ts";

describe("frame codec", () => {
  test("round-trips a JSON value through encodeLine + LineReader", async () => {
    const reader = new MemoryByteReader();
    const lineReader = new LineReader(reader);
    const value = { hello: "world", n: 42, nested: { a: [1, 2, 3] } };
    reader.push(encodeLine(value));

    const line = await lineReader.nextLine();
    expect(line).not.toBeNull();
    expect(JSON.parse(line as string)).toEqual(value);
  });

  test("handles a line split across multiple chunks", async () => {
    const reader = new MemoryByteReader();
    const lineReader = new LineReader(reader);
    const bytes = encodeLine({ a: 1 });
    reader.push(bytes.slice(0, 3));
    reader.push(bytes.slice(3));

    const line = await lineReader.nextLine();
    expect(JSON.parse(line as string)).toEqual({ a: 1 });
  });

  test("yields multiple lines delivered in a single chunk", async () => {
    const reader = new MemoryByteReader();
    const lineReader = new LineReader(reader);
    reader.push(
      new TextEncoder().encode(
        `${JSON.stringify({ n: 1 })}\n${JSON.stringify({ n: 2 })}\n`,
      ),
    );

    expect(JSON.parse((await lineReader.nextLine()) as string)).toEqual({ n: 1 });
    expect(JSON.parse((await lineReader.nextLine()) as string)).toEqual({ n: 2 });
  });

  test("returns null at EOF once the buffer is drained", async () => {
    const reader = new MemoryByteReader();
    const lineReader = new LineReader(reader);
    reader.close();

    expect(await lineReader.nextLine()).toBeNull();
  });

  test("delivers a final line without a trailing newline before EOF", async () => {
    const reader = new MemoryByteReader();
    const lineReader = new LineReader(reader);
    reader.pushRaw(JSON.stringify({ x: 1 }));
    reader.close();

    expect(JSON.parse((await lineReader.nextLine()) as string)).toEqual({ x: 1 });
    expect(await lineReader.nextLine()).toBeNull();
  });

  test("rejects a line that exceeds the configured max length", async () => {
    const reader = new MemoryByteReader();
    const lineReader = new LineReader(reader, 16);
    reader.push(new TextEncoder().encode("x".repeat(100)));

    await expect(lineReader.nextLine()).rejects.toThrow(/exceeds max length/);
  });
});

describe("JsonRpcEndpoint — incoming requests/notifications", () => {
  test("serves a registered request handler and writes a result response", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.setRequestHandler("double", (params) => {
      const { n } = params as { n: number };
      return { n: n * 2 };
    });
    endpoint.start();

    reader.pushLine({ jsonrpc: "2.0", id: 7, method: "double", params: { n: 21 } });
    await writer.waitForCount(1);

    expect(writer.messages[0]).toEqual({
      jsonrpc: "2.0",
      id: 7,
      result: { n: 42 },
    });
    await endpoint.stop();
  });

  test("responds method_not_found for an unregistered method", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();

    reader.pushLine({ jsonrpc: "2.0", id: 1, method: "nope", params: {} });
    await writer.waitForCount(1);

    const msg = writer.messages[0] as { error?: { code: number } };
    expect(msg.error?.code).toBe(-32601);
    await endpoint.stop();
  });

  test("responds internal_error when a request handler throws", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.setRequestHandler("boom", () => {
      throw new Error("kaboom");
    });
    endpoint.start();

    reader.pushLine({ jsonrpc: "2.0", id: 2, method: "boom", params: {} });
    await writer.waitForCount(1);

    const msg = writer.messages[0] as { error?: { code: number; message: string } };
    expect(msg.error?.code).toBe(-32603);
    expect(msg.error?.message).toContain("kaboom");
    await endpoint.stop();
  });

  test("dispatches notifications without writing a response", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    let seen: unknown;
    endpoint.setNotificationHandler("ping", (params) => {
      seen = params;
    });
    endpoint.start();

    reader.pushLine({ jsonrpc: "2.0", method: "ping", params: { ok: true } });
    // Give the handler a tick to run; then prove nothing was written back.
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(seen).toEqual({ ok: true });
    expect(writer.messages.length).toBe(0);
    await endpoint.stop();
  });
});

describe("JsonRpcEndpoint — outgoing requests", () => {
  test("resolves out-of-order responses to the correct caller", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();

    const p1 = endpoint.request<{ who: string }>("ping", { seq: 1 });
    const p2 = endpoint.request<{ who: string }>("ping", { seq: 2 });

    await writer.waitForCount(2);
    const [req1, req2] = writer.messages as [
      { id: number; params: { seq: number } },
      { id: number; params: { seq: number } },
    ];
    expect(req1.params.seq).toBe(1);
    expect(req2.params.seq).toBe(2);
    expect(req1.id).not.toBe(req2.id);

    // Reply to the SECOND call first — the endpoint must still route each
    // response to its own caller by id, not by arrival order.
    reader.pushLine({ jsonrpc: "2.0", id: req2.id, result: { who: "second" } });
    reader.pushLine({ jsonrpc: "2.0", id: req1.id, result: { who: "first" } });

    expect(await p2).toEqual({ who: "second" });
    expect(await p1).toEqual({ who: "first" });
    await endpoint.stop();
  });

  test("rejects with JsonRpcRemoteError on an error reply", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();

    const p = endpoint.request("boom", {});
    await writer.waitForCount(1);
    const [req] = writer.messages as [{ id: number }];
    reader.pushLine({
      jsonrpc: "2.0",
      id: req.id,
      error: { code: -32601, message: "nope" },
    });

    await expect(p).rejects.toBeInstanceOf(JsonRpcRemoteError);
    await endpoint.stop();
  });

  test("times out when no response arrives in time", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();

    await expect(
      endpoint.request("slow", {}, { timeoutMs: 20 }),
    ).rejects.toBeInstanceOf(RpcTimeoutError);
    await endpoint.stop();
  });

  test("sends notifications with no id and expects no reply", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();

    await endpoint.notify("log_emit", { level: "info", message: "hi" });
    await writer.waitForCount(1);

    const msg = writer.messages[0] as Record<string, unknown>;
    expect(msg.method).toBe("log_emit");
    expect("id" in msg).toBe(false);
    await endpoint.stop();
  });
});
