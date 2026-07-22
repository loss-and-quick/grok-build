import { describe, expect, test } from "bun:test";

import { createPluginContext } from "../src/context.ts";
import { HostClient } from "../src/rpc.ts";
import { JsonRpcEndpoint } from "../src/stdio.ts";
import type { InitializeParams } from "../src/generated/InitializeParams.ts";
import type { PanelViewModel } from "../src/generated/PanelViewModel.ts";
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

  test("agents.spawn/wait/events/list/cancel round-trip through the agent_* RPCs", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();
    const ctx = createPluginContext(new HostClient(endpoint), INIT_PARAMS);

    const spawnP = ctx.agents.spawn({
      agent_type: "Explore",
      prompt: "map the repo",
      description: null,
      model: null,
      cwd: null,
      timeout_ms: 60_000,
    });
    await respondToNext(writer, reader, 1, { id: "agent-1" });
    expect(await spawnP).toBe("agent-1");
    const spawnReq = writer.messages[0] as { method: string; params: unknown };
    expect(spawnReq.method).toBe("agent_spawn");
    expect(spawnReq.params).toEqual({
      agent_type: "Explore",
      prompt: "map the repo",
      description: null,
      model: null,
      cwd: null,
      timeout_ms: 60_000,
    });

    const waitP = ctx.agents.wait("agent-1", 1_000);
    await respondToNext(writer, reader, 2, {
      status: "completed",
      output: "done",
      error: null,
      tokens_used: 12,
      duration_ms: 34,
      tool_calls: 2,
      turns: 1,
    });
    expect((await waitP).status).toBe("completed");
    const waitReq = writer.messages[1] as { method: string; params: unknown };
    expect(waitReq.method).toBe("agent_wait");
    expect(waitReq.params).toEqual({ id: "agent-1", timeout_ms: 1_000 });

    const eventsP = ctx.agents.events("agent-1", 1);
    await respondToNext(writer, reader, 3, {
      events: [{ seq: 1, kind: "completed", data: {} }],
      next_cursor: 2,
      done: true,
    });
    expect((await eventsP).done).toBe(true);
    const eventsReq = writer.messages[2] as { method: string; params: unknown };
    expect(eventsReq.method).toBe("agent_events");
    expect(eventsReq.params).toEqual({ id: "agent-1", cursor: 1, timeout_ms: 0 });

    const listP = ctx.agents.list();
    await respondToNext(writer, reader, 4, {
      agents: [
        { name: "Explore", description: "search the repo", model: "grok-code-fast-1" },
        { name: "general-purpose", description: "" },
      ],
    });
    expect(await listP).toEqual([
      { name: "Explore", description: "search the repo", model: "grok-code-fast-1" },
      { name: "general-purpose", description: "" },
    ]);

    const cancelP = ctx.agents.cancel("agent-1");
    await respondToNext(writer, reader, 5, { outcome: "already_finished" });
    expect(await cancelP).toBe("already_finished");

    await endpoint.stop();
  });

  test("agents.send continues a prior subagent and resolves the new id", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();
    const ctx = createPluginContext(new HostClient(endpoint), INIT_PARAMS);

    const sendP = ctx.agents.send("agent-1", "now review it", 30_000);
    await respondToNext(writer, reader, 1, { id: "agent-2" });
    expect(await sendP).toBe("agent-2");
    const sendReq = writer.messages[0] as { method: string; params: unknown };
    expect(sendReq.method).toBe("agent_send");
    expect(sendReq.params).toEqual({
      id: "agent-1",
      prompt: "now review it",
      timeout_ms: 30_000,
    });

    // Omitted timeout serializes as null.
    const sendP2 = ctx.agents.send("agent-2", "and again");
    await respondToNext(writer, reader, 2, { id: "agent-3" });
    expect(await sendP2).toBe("agent-3");
    const sendReq2 = writer.messages[1] as { params: unknown };
    expect(sendReq2.params).toEqual({
      id: "agent-2",
      prompt: "and again",
      timeout_ms: null,
    });

    await endpoint.stop();
  });

  test("ui.publishPanel/closePanel round-trip through the ui_* RPCs", async () => {
    const reader = new MemoryByteReader();
    const writer = new MemoryByteWriter();
    const endpoint = new JsonRpcEndpoint({ reader, writer });
    endpoint.start();
    const ctx = createPluginContext(new HostClient(endpoint), INIT_PARAMS);

    const vm: PanelViewModel = {
      id: "panel-1",
      title: "Status",
      blocks: [{ kind: "markdown", text: "hello" }],
    };
    const publishP = ctx.ui.publishPanel(vm);
    await respondToNext(writer, reader, 1, {});
    await publishP;
    const publishReq = writer.messages[0] as { method: string; params: unknown };
    expect(publishReq.method).toBe("ui_publish_panel");
    expect(publishReq.params).toEqual(vm);

    const closeP = ctx.ui.closePanel("panel-1");
    await respondToNext(writer, reader, 2, {});
    await closeP;
    const closeReq = writer.messages[1] as { method: string; params: unknown };
    expect(closeReq.method).toBe("ui_close_panel");
    expect(closeReq.params).toEqual({ id: "panel-1" });

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
