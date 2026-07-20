import { describe, expect, test } from "bun:test";

import {
  allow,
  definePlugin,
  deny,
  forceStop,
  observed,
  replace,
  stopBlock,
} from "../src/define.ts";
import type { InitializeParams } from "../src/generated/InitializeParams.ts";
import { MemoryByteReader, MemoryByteWriter } from "./helpers/memory-stream.ts";

const INIT_PARAMS: InitializeParams = {
  protocol_version: 1,
  plugin_name: "test-plugin",
  plugin_config: { greeting: "hi" },
  workspace_root: "/workspace",
  session_id: "session-123",
  capabilities: { storage: true, leader_socket: null },
};

function setUpEndpoint() {
  const reader = new MemoryByteReader();
  const writer = new MemoryByteWriter();
  return { reader, writer };
}

async function initialize(
  reader: MemoryByteReader,
  writer: MemoryByteWriter,
  params: InitializeParams = INIT_PARAMS,
) {
  reader.pushLine({ jsonrpc: "2.0", id: 1, method: "initialize", params });
  await writer.waitForCount(1);
  return writer.messages[0] as {
    jsonrpc: "2.0";
    id: number;
    result: { protocol_version: number; subscriptions: string[] };
  };
}

describe("definePlugin — handshake", () => {
  test("derives subscriptions from the hooks map and echoes protocol_version", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        name: "test-plugin",
        hooks: {
          pre_tool_use: () => allow(),
          post_tool_use: () => observed(),
        },
      },
      { reader, writer, exitOnShutdown: false },
    );

    const response = await initialize(reader, writer);
    expect(response.result.protocol_version).toBe(1);
    expect(response.result.subscriptions.sort()).toEqual(
      ["post_tool_use", "pre_tool_use"].sort(),
    );
  });

  test("a plugin with no hooks reports an empty subscription list", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin({ hooks: {} }, { reader, writer, exitOnShutdown: false });

    const response = await initialize(reader, writer);
    expect(response.result.subscriptions).toEqual([]);
  });

  test("runs setup() with a context built from InitializeParams", async () => {
    const { reader, writer } = setUpEndpoint();
    let seenWorkspaceRoot = "";
    let seenSessionId = "";
    const handle = definePlugin(
      {
        setup(ctx) {
          seenWorkspaceRoot = ctx.workspaceRoot;
          seenSessionId = ctx.sessionId;
        },
      },
      { reader, writer, exitOnShutdown: false },
    );

    await initialize(reader, writer);
    await handle.whenReady;
    expect(seenWorkspaceRoot).toBe("/workspace");
    expect(seenSessionId).toBe("session-123");
  });

  test("setup() can await ctx.config() without deadlocking the handshake", async () => {
    const { reader, writer } = setUpEndpoint();
    let seenConfig: unknown;
    definePlugin(
      {
        async setup(ctx) {
          seenConfig = await ctx.config();
        },
      },
      { reader, writer, exitOnShutdown: false },
    );

    reader.pushLine({ jsonrpc: "2.0", id: 1, method: "initialize", params: INIT_PARAMS });

    // The outgoing config_get request must be written (and answered) BEFORE
    // initialize's own response — proof the read loop kept pumping incoming
    // lines while setup() awaited an outgoing call, instead of deadlocking.
    await writer.waitForCount(1);
    const configReq = writer.messages[0] as { id: number; method: string };
    expect(configReq.method).toBe("config_get");
    reader.pushLine({
      jsonrpc: "2.0",
      id: configReq.id,
      result: { value: { greeting: "hi" } },
    });

    await writer.waitForCount(2);
    const initResp = writer.messages[1] as { result: { subscriptions: string[] } };
    expect(initResp.result.subscriptions).toEqual([]);
    expect(seenConfig).toEqual({ greeting: "hi" });
  });
});

describe("definePlugin — hook_invoke dispatch", () => {
  async function invokeHook(
    reader: MemoryByteReader,
    writer: MemoryByteWriter,
    event: string,
    gate: string,
    responseIndex: number,
  ) {
    reader.pushLine({
      jsonrpc: "2.0",
      id: 42,
      method: "hook_invoke",
      params: {
        invocation_id: "inv-1",
        event,
        gate,
        payload: { some: "payload" },
        timeout_ms: 5_000,
      },
    });
    await writer.waitForCount(responseIndex + 1);
    return (writer.messages[responseIndex] as { result: unknown }).result;
  }

  test("allow() produces the exact decision/allow wire shape", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { hooks: { pre_tool_use: () => allow("looks fine") } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    const result = await invokeHook(reader, writer, "pre_tool_use", "tool", 1);
    expect(result).toEqual({ kind: "decision", decision: "allow", reason: "looks fine" });
  });

  test("deny() produces the exact decision/deny wire shape", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { hooks: { pre_tool_use: () => deny("nope") } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    const result = await invokeHook(reader, writer, "pre_tool_use", "tool", 1);
    expect(result).toEqual({ kind: "decision", decision: "deny", reason: "nope" });
  });

  test("stopBlock() blocks but leaves continue true, with additional_context", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { hooks: { stop: () => stopBlock("keep going", "extra info") } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    const result = await invokeHook(reader, writer, "stop", "stop", 1);
    expect(result).toEqual({
      kind: "stop",
      block: true,
      reason: "keep going",
      continue: true,
      additional_context: "extra info",
    });
  });

  test("forceStop() blocks with continue false", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { hooks: { stop: () => forceStop("fatal") } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    const result = await invokeHook(reader, writer, "stop", "stop", 1);
    expect(result).toEqual({ kind: "stop", block: true, reason: "fatal", continue: false });
  });

  test("observed() produces the bare observed wire shape", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { hooks: { notification: () => observed() } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    const result = await invokeHook(reader, writer, "notification", "observe", 1);
    expect(result).toEqual({ kind: "observed" });
  });

  test("replace() carries a payload", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { hooks: { provider_request: () => replace({ patched: true }) } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    const result = await invokeHook(reader, writer, "provider_request", "replace", 1);
    expect(result).toEqual({ kind: "replace", payload: { patched: true } });
  });

  test("a hook returning undefined is treated as passthrough (observed)", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { hooks: { session_start: () => undefined } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    const result = await invokeHook(reader, writer, "session_start", "observe", 1);
    expect(result).toEqual({ kind: "observed" });
  });

  test("an uncaught hook error fails open: observed reply + log_emit error", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        hooks: {
          pre_tool_use: () => {
            throw new Error("plugin bug");
          },
        },
      },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    // initialize's result is messages[0]; hook_invoke triggers a log_emit
    // notification AND the hook_invoke result — two more messages.
    reader.pushLine({
      jsonrpc: "2.0",
      id: 99,
      method: "hook_invoke",
      params: {
        invocation_id: "inv-2",
        event: "pre_tool_use",
        gate: "tool",
        payload: {},
        timeout_ms: 5_000,
      },
    });
    await writer.waitForCount(3);

    const logMessage = writer.messages[1] as {
      method: string;
      params: { level: string; message: string };
    };
    expect(logMessage.method).toBe("log_emit");
    expect(logMessage.params.level).toBe("error");
    expect(logMessage.params.message).toContain("pre_tool_use");

    const hookResponse = writer.messages[2] as { result: unknown };
    expect(hookResponse.result).toEqual({ kind: "observed" });
  });

  test("a hook can await ctx.storage.get without deadlocking hook_invoke", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        hooks: {
          pre_tool_use: async (_payload, ctx) => {
            const flag = await ctx.storage.get("flag");
            return flag ? allow() : deny("flag not set");
          },
        },
      },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    reader.pushLine({
      jsonrpc: "2.0",
      id: 7,
      method: "hook_invoke",
      params: {
        invocation_id: "inv-x",
        event: "pre_tool_use",
        gate: "tool",
        payload: {},
        timeout_ms: 5_000,
      },
    });

    // The outgoing storage_get request must be written (and answered) BEFORE
    // the hook_invoke response — proof reading kept pumping while the hook
    // awaited an outgoing call.
    await writer.waitForCount(2);
    const storageReq = writer.messages[1] as {
      id: number;
      method: string;
      params: { key: string };
    };
    expect(storageReq.method).toBe("storage_get");
    expect(storageReq.params.key).toBe("flag");
    reader.pushLine({ jsonrpc: "2.0", id: storageReq.id, result: { value: true } });

    await writer.waitForCount(3);
    const hookResp = writer.messages[2] as { result: unknown };
    expect(hookResp.result).toEqual({ kind: "decision", decision: "allow" });
  });

  test("an event with no registered handler fails open to observed", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin({ hooks: {} }, { reader, writer, exitOnShutdown: false });
    await initialize(reader, writer);

    const result = await invokeHook(reader, writer, "post_compact", "observe", 1);
    expect(result).toEqual({ kind: "observed" });
  });
});

describe("definePlugin — shutdown", () => {
  test("runs the teardown callback returned by setup() and resolves whenShutdown", async () => {
    const { reader, writer } = setUpEndpoint();
    let tornDown = false;
    const handle = definePlugin(
      {
        setup() {
          return () => {
            tornDown = true;
          };
        },
      },
      { reader, writer, exitOnShutdown: false },
    );

    await initialize(reader, writer);
    await handle.whenReady;

    reader.pushLine({ jsonrpc: "2.0", method: "shutdown", params: { reason: "host requested" } });
    await handle.whenShutdown;

    expect(tornDown).toBe(true);
  });

  test("shutdown does not hang when there is no teardown", async () => {
    const { reader, writer } = setUpEndpoint();
    const handle = definePlugin({}, { reader, writer, exitOnShutdown: false });

    await initialize(reader, writer);
    await handle.whenReady;

    reader.pushLine({ jsonrpc: "2.0", method: "shutdown", params: { reason: "bye" } });
    await handle.whenShutdown;
  });
});
