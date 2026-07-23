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

  test("provider_response replies with a toolCalls rename map", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        hooks: {
          provider_response: () =>
            replace({
              toolCalls: [
                { id: "call_a", name: "read_file" },
                { id: "call_b", name: "write_file" },
              ],
            }),
        },
      },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    const result = await invokeHook(reader, writer, "provider_response", "replace", 1);
    expect(result).toEqual({
      kind: "replace",
      payload: {
        toolCalls: [
          { id: "call_a", name: "read_file" },
          { id: "call_b", name: "write_file" },
        ],
      },
    });
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

describe("definePlugin — tools", () => {
  const TOOL_INVOKE_PARAMS = (tool: string, args: unknown) => ({
    jsonrpc: "2.0",
    id: 55,
    method: "tool_invoke",
    params: {
      invocation_id: "tinv-1",
      tool,
      arguments: args,
      context: { session_id: "sess-42", cwd: "/proj/dir", agent: "main" },
      timeout_ms: 120_000,
    },
  });

  test("handshake reports tool descriptors with defaulted schema", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        tools: {
          echo: {
            description: "Echo the input",
            inputSchema: {
              type: "object",
              properties: { text: { type: "string" } },
            },
            handler: (input) => JSON.stringify(input),
          },
          bare: { handler: () => "ok" },
        },
      },
      { reader, writer, exitOnShutdown: false },
    );

    const response = await initialize(reader, writer);
    const result = response.result as unknown as {
      tools: Array<{ name: string; description: string; input_schema: unknown }>;
    };
    expect(result.tools.map((t) => t.name).sort()).toEqual(["bare", "echo"]);
    const echo = result.tools.find((t) => t.name === "echo")!;
    expect(echo.description).toBe("Echo the input");
    expect(echo.input_schema).toEqual({
      type: "object",
      properties: { text: { type: "string" } },
    });
    const bare = result.tools.find((t) => t.name === "bare")!;
    expect(bare.description).toBe("");
    expect(bare.input_schema).toEqual({ type: "object", properties: {} });
  });

  test("tool_invoke dispatches with input and per-call context", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        tools: {
          echo: {
            handler: (input, ctx, call) =>
              `echo ${JSON.stringify(input)} session=${call.sessionId} ` +
              `cwd=${call.cwd} agent=${call.agent} ws=${ctx.workspaceRoot}`,
          },
        },
      },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    reader.pushLine(TOOL_INVOKE_PARAMS("echo", { text: "hi" }));
    await writer.waitForCount(2);
    const resp = writer.messages[1] as {
      result: { content: string; is_error: boolean };
    };
    expect(resp.result.is_error).toBe(false);
    expect(resp.result.content).toContain(`echo {"text":"hi"}`);
    expect(resp.result.content).toContain("session=sess-42");
    expect(resp.result.content).toContain("cwd=/proj/dir");
    expect(resp.result.content).toContain("agent=main");
    expect(resp.result.content).toContain("ws=/workspace");
  });

  test("tool_cancel aborts the in-flight handler's signal", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        tools: {
          waiter: {
            // Resolve only once the signal aborts, so the reply proves the
            // handler observed the cancellation.
            handler: (_input, _ctx, call) =>
              new Promise<string>((resolve) => {
                if (call.signal.aborted) resolve("aborted-immediately");
                call.signal.addEventListener("abort", () => resolve("saw-abort"));
              }),
          },
        },
      },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    // Start the call: the handler parks on its signal, no reply yet.
    reader.pushLine(TOOL_INVOKE_PARAMS("waiter", {}));
    // Host abandons the call (parent turn aborted) via the notification.
    reader.pushLine({
      jsonrpc: "2.0",
      method: "tool_cancel",
      params: { invocation_id: "tinv-1" },
    });

    await writer.waitForCount(2);
    const resp = writer.messages[1] as {
      result: { content: string; is_error: boolean };
    };
    expect(resp.result.content).toBe("saw-abort");
    expect(resp.result.is_error).toBe(false);
  });

  test("tool_cancel for an unknown/finished invocation is a harmless no-op", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { tools: { echo: { handler: () => "ok" } } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    // Complete a call, then cancel it: the entry is already gone.
    reader.pushLine(TOOL_INVOKE_PARAMS("echo", {}));
    await writer.waitForCount(2);
    reader.pushLine({
      jsonrpc: "2.0",
      method: "tool_cancel",
      params: { invocation_id: "tinv-1" },
    });

    // A fresh call still works (the endpoint did not choke on the stray cancel).
    reader.pushLine({ ...TOOL_INVOKE_PARAMS("echo", {}), id: 56 });
    await writer.waitForCount(3);
    const resp = writer.messages[2] as { result: { content: string } };
    expect(resp.result.content).toBe("ok");
  });

  test("a rich ToolResult maps isError onto the wire", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        tools: {
          failing: {
            handler: () => ({ content: "handler says no", isError: true }),
          },
        },
      },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    reader.pushLine(TOOL_INVOKE_PARAMS("failing", {}));
    await writer.waitForCount(2);
    const resp = writer.messages[1] as { result: { content: string; is_error: boolean } };
    expect(resp.result).toEqual({ content: "handler says no", is_error: true });
  });

  test("a void handler result is an empty success", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { tools: { quiet: { handler: () => undefined } } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    reader.pushLine(TOOL_INVOKE_PARAMS("quiet", {}));
    await writer.waitForCount(2);
    const resp = writer.messages[1] as { result: unknown };
    expect(resp.result).toEqual({ content: "", is_error: false });
  });

  test("an unknown tool is an error result, not a crash", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      { tools: { known: { handler: () => "ok" } } },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    reader.pushLine(TOOL_INVOKE_PARAMS("ghost", {}));
    await writer.waitForCount(2);
    const resp = writer.messages[1] as { result: { content: string; is_error: boolean } };
    expect(resp.result.is_error).toBe(true);
    expect(resp.result.content).toContain('"ghost"');
  });

  test("a throwing handler becomes an error result plus a log_emit", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        tools: {
          bomb: {
            handler: () => {
              throw new Error("tool bug");
            },
          },
        },
      },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    reader.pushLine(TOOL_INVOKE_PARAMS("bomb", {}));
    // log_emit notification + the tool_invoke response.
    await writer.waitForCount(3);
    const log = writer.messages[1] as {
      method: string;
      params: { level: string; message: string };
    };
    expect(log.method).toBe("log_emit");
    expect(log.params.level).toBe("error");
    expect(log.params.message).toContain("bomb");
    const resp = writer.messages[2] as { result: { content: string; is_error: boolean } };
    expect(resp.result).toEqual({ content: "tool bug", is_error: true });
  });

  test("a tool handler can await ctx.storage.get without deadlocking", async () => {
    const { reader, writer } = setUpEndpoint();
    definePlugin(
      {
        tools: {
          lookup: {
            handler: async (_input, ctx) => {
              const value = await ctx.storage.get("k");
              return `value=${JSON.stringify(value)}`;
            },
          },
        },
      },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    reader.pushLine(TOOL_INVOKE_PARAMS("lookup", {}));

    // The outgoing storage_get must be written (and answered) BEFORE the
    // tool_invoke response — the read loop keeps pumping while the handler
    // awaits its own plugin→core call on the same stream.
    await writer.waitForCount(2);
    const storageReq = writer.messages[1] as { id: number; method: string };
    expect(storageReq.method).toBe("storage_get");
    reader.pushLine({ jsonrpc: "2.0", id: storageReq.id, result: { value: 7 } });

    await writer.waitForCount(3);
    const resp = writer.messages[2] as { result: unknown };
    expect(resp.result).toEqual({ content: "value=7", is_error: false });
  });
});

describe("definePlugin — panel_action dispatch", () => {
  test("a panel_action notification fires onPanelAction with the ids, inputs, and ctx", async () => {
    const { reader, writer } = setUpEndpoint();
    let seenPanelId = "";
    let seenButtonId = "";
    let seenCode = "";
    let seenSessionId = "";
    definePlugin(
      {
        onPanelAction(panelId, buttonId, inputs, ctx) {
          seenPanelId = panelId;
          seenButtonId = buttonId;
          seenCode = inputs.code ?? "";
          seenSessionId = ctx.sessionId;
        },
      },
      { reader, writer, exitOnShutdown: false },
    );
    await initialize(reader, writer);

    reader.pushLine({
      jsonrpc: "2.0",
      method: "panel_action",
      params: { panel_id: "p1", button_id: "ok", inputs: { code: "abc-123" } },
    });

    // No reply is written for a notification; poll until the handler ran.
    for (let i = 0; i < 50 && seenPanelId === ""; i++) {
      await new Promise((resolve) => setTimeout(resolve, 1));
    }
    expect(seenPanelId).toBe("p1");
    expect(seenButtonId).toBe("ok");
    expect(seenCode).toBe("abc-123");
    expect(seenSessionId).toBe("session-123");
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
