import { describe, expect, test } from "bun:test";

import { createPluginContext } from "../src/context.ts";
import { HostClient } from "../src/rpc.ts";
import { JsonRpcEndpoint } from "../src/stdio.ts";
import type { InitializeParams } from "../src/generated/InitializeParams.ts";
import { MemoryByteReader, MemoryByteWriter } from "./helpers/memory-stream.ts";

const INIT_PARAMS: InitializeParams = {
  protocol_version: 1,
  plugin_name: "test-plugin",
  plugin_config: { greeting: "hi" },
  workspace_root: "/workspace",
  session_id: "session-abc",
  capabilities: { storage: true, leader_socket: null },
};

/**
 * Waits for the next outgoing request beyond `writer.messages` current
 * length and replies to it with `result`. Returns the new total message
 * count, so callers can chain calls across a sequence of round trips.
 */
async function respondToNext(
  writer: MemoryByteWriter,
  reader: MemoryByteReader,
  expectedTotal: number,
  result: unknown,
): Promise<void> {
  await writer.waitForCount(expectedTotal);
  const req = writer.messages[expectedTotal - 1] as { id: number };
  reader.pushLine({ jsonrpc: "2.0", id: req.id, result });
}

describe("PluginContext", () => {
  test("exposes workspaceRoot/sessionId straight from InitializeParams", () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    const ctx = createPluginContext(new HostClient(endpoint), INIT_PARAMS);

    expect(ctx.workspaceRoot).toBe("/workspace");
    expect(ctx.sessionId).toBe("session-abc");
  });

  test("log.* sends a log_emit notification with the right level", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();
    const ctx = createPluginContext(new HostClient(endpoint), INIT_PARAMS);

    ctx.log.warn("careful", { code: 7 });
    await writer.waitForCount(1);

    expect(writer.messages[0]).toEqual({
      jsonrpc: "2.0",
      method: "log_emit",
      params: { level: "warn", message: "careful", fields: { code: 7 } },
    });
    await endpoint.stop();
  });

  test("storage.get/set/delete/list round-trip through the host RPCs", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();
    const ctx = createPluginContext(new HostClient(endpoint), INIT_PARAMS);

    const getP = ctx.storage.get("k");
    await respondToNext(writer, reader, 1, { value: 123 });
    expect(await getP).toBe(123);

    const setP = ctx.storage.set("k", 456);
    await respondToNext(writer, reader, 2, {});
    await setP;
    const setReq = writer.messages[1] as { method: string; params: unknown };
    expect(setReq.method).toBe("storage_set");
    expect(setReq.params).toEqual({ key: "k", value: 456 });

    const delP = ctx.storage.delete("k");
    await respondToNext(writer, reader, 3, { existed: true });
    expect(await delP).toBe(true);

    const listP = ctx.storage.list("prefix-");
    await respondToNext(writer, reader, 4, { keys: ["a", "b"] });
    expect(await listP).toEqual(["a", "b"]);

    await endpoint.stop();
  });

  test("config<T>() calls config_get and returns its value", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();
    const ctx = createPluginContext(new HostClient(endpoint), INIT_PARAMS);

    const configP = ctx.config<{ greeting: string }>();
    await respondToNext(writer, reader, 1, { value: { greeting: "hi" } });
    expect(await configP).toEqual({ greeting: "hi" });

    await endpoint.stop();
  });
});
