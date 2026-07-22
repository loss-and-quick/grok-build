# @grok-build/plugin

TypeScript SDK for grok-build sidecar plugins: a plugin is a standalone
process that talks to the host over newline-delimited JSON-RPC 2.0 on
stdin/stdout. This package is the client side of that protocol — a hook
dispatcher, typed wire types, and a small `ctx` (log/storage/config) — so a
plugin author never touches JSON-RPC directly.

Source-first: there is no build step. `exports["."]` points straight at
`src/index.ts`; Bun and Deno run it natively, and modern Node runs it via
`--experimental-strip-types`.

## Minimal example

```ts
import { definePlugin, allow, deny } from "@grok-build/plugin";

definePlugin({
  name: "no-rm-rf",
  hooks: {
    pre_tool_use: (payload, ctx) => {
      const cmd = (payload as { command?: string }).command ?? "";
      if (cmd.includes("rm -rf /")) return deny("blocked by no-rm-rf");
      ctx.log.info("tool call allowed", { cmd });
      return allow();
    },
  },
});
```

That call *is* the program: it starts the stdio JSON-RPC loop, answers the
host's `initialize` handshake (subscriptions are derived from the `hooks`
keys), dispatches `hook_invoke`, and exits on `shutdown`.

## Model-visible tools (`tools`)

A plugin can serve tools the model calls like any other tool. Declare each
tool in **plugin.json** (`tools: [{ name, description, inputSchema,
timeoutMs? }]`) — that array is what the model-facing catalog is built from,
under the qualified name `<plugin>__<tool>` (so permission rules and
`pre/post_tool_use` hooks apply exactly as for MCP tools) — and provide the
handler in `definePlugin`:

```ts
definePlugin({
  tools: {
    echo: {
      description: "Echo text back",       // informational; catalog uses plugin.json
      inputSchema: { type: "object", properties: { text: { type: "string" } } },
      handler: async (input, ctx, call) => {
        // Full plugin context: ctx.storage / ctx.agents / ctx.log / ctx.config
        await ctx.storage.set(`last:${call.cwd}`, input);
        // Per-call context: who called, from where.
        return `echo ${JSON.stringify(input)} (cwd=${call.cwd}, agent=${call.agent})`;
      },
    },
  },
});
```

The handler runs in the sidecar via the `tool_invoke` RPC with
`call = { sessionId, cwd, agent }`: `cwd` is the calling session's working
directory **at call time** (key project-scoped state off it) and `agent` is
`"main"` for the root session or the subagent type label. Return a string
(success content), `{ content, isError }`, or nothing (empty success);
thrown errors become error tool results for the model, never a sidecar
crash. The host enforces a hard per-call deadline (default 120 s;
`timeoutMs` in the manifest overrides it per tool) — a slow or crashed
sidecar yields an error result, not a hang. Handlers may freely await
plugin→core calls (`ctx.storage`, `ctx.agents`, …) mid-invoke; the endpoint
serves both directions concurrently. The host warns at handshake when the
manifest's `tools` array and the `definePlugin` tools map drift.

## Subagent orchestration (`ctx.agents`)

Every hook and `setup()` receives `ctx.agents`, a typed wrapper over the
`agent_*` RPCs. Spawned subagents are **real children of the plugin's
session** — same coordinator, TUI visibility, and cancellation as the
model's `Task` tool:

```ts
const id = await ctx.agents.spawn({
  agent_type: "Explore",           // default: "general-purpose"
  prompt: "map the crate layout",
  description: "layout mapper",    // shown in the TUI
  model: null,                     // catalog-validated when set
  cwd: null,
  timeout_ms: 120_000,             // per-spawn budget: auto-cancel after
});

// Progress: cursor-based poll. Pass the last next_cursor (start at 0);
// timeoutMs long-polls until a new event arrives. Stop once done.
let cursor = 0;
for (;;) {
  const { events, next_cursor, done } = await ctx.agents.events(id, cursor, 10_000);
  for (const e of events) ctx.log.info(`agent ${e.kind}`, e.data);
  cursor = next_cursor;
  if (done) break;
}

const result = await ctx.agents.wait(id, 30_000); // "running" on timeout
if (result.status === "completed") ctx.log.info(result.output ?? "");

await ctx.agents.list();   // [{ name, description, model? }] per spawnable type
await ctx.agents.cancel(id);
```

Progress is delivered by **cursor-based polling rather than host→plugin
notifications**: the capability server is plain request/reply and keeps
this state host-side, so a poll cursor survives a sidecar crash-restart
where a notification subscription would be lost. Spec-level failures
(unknown agent type, bad model) surface as the spawn's terminal result,
not as an RPC error. In sessions without orchestration wiring every
`ctx.agents` call rejects with JSON-RPC `method_not_found` (-32601) —
catch it to feature-detect.

## UI panels (`ctx.ui`)

A plugin can publish a declarative panel the host renders in its pager.
Publishing is keyed by the panel's own `id` — re-publishing the same `id`
replaces the panel (latest-wins). Button presses come back as
`panel_action` notifications, delivered to the definition's
`onPanelAction` handler:

Panels can also be interactive: an `input` block renders an editable field,
and its current value is delivered to `onPanelAction` in the `inputs` map
(keyed by the field's `id`) whenever a button is pressed. This is enough to
build an OAuth-style flow — show instructions, collect the authorization
code, exchange it on submit:

```ts
definePlugin({
  async setup(ctx) {
    await ctx.ui.publishPanel({
      id: "connect-account",
      title: "Connect account",
      blocks: [
        {
          kind: "markdown",
          text: "Open the authorization URL, then paste the code below.",
        },
        { kind: "input", id: "code", label: "Authorization code" },
        { kind: "actions", buttons: [{ id: "submit", label: "Submit" }] },
      ],
    });
  },
  async onPanelAction(panelId, buttonId, inputs, ctx) {
    if (buttonId === "submit") {
      const code = inputs.code ?? "";
      // exchange the code with an external identity provider
      ctx.log.info(`exchanging authorization code from ${panelId}`);
      await ctx.storage.set("oauth:code", code);
      await ctx.ui.closePanel(panelId);
    }
  },
});
```

`publishPanel`/`closePanel` are plugin→core requests (the SDK awaits the
host's ack and discards the empty result). `onPanelAction` receives the
`inputs` map (every Input block's current value, keyed by field id) as its
third argument and the `PluginContext` as its fourth. It is best-effort,
like the host's notification: a throw is logged and swallowed, never
crashing the sidecar.

## Leader socket (headless ACP access)

When the host process runs in leader mode, each sidecar is told where the
session leader's Unix socket lives, twice: as
`capabilities.leader_socket` in the `initialize` params (surfaced on
`ctx` via the raw init object) and as the `GROK_LEADER_SOCKET` env var —
the same variable the built-in leader clients honor. A plugin can open
that socket and speak ACP over it as one more headless client: create
its own sessions, drive prompts, observe notifications — everything a
TUI or IDE client can do. The SDK deliberately ships no ACP client
wrapper (yet); bring any newline-delimited JSON-RPC client, e.g.:

```ts
import { connect } from "node:net";

const path = process.env.GROK_LEADER_SOCKET;
if (path) {
  const sock = connect(path); // then speak ACP JSON-RPC over `sock`
}
```

Outside leader mode the capability is `null` and the env var is unset —
feature-detect and degrade gracefully.

## Runtime support

| Runtime   | Status | Notes |
|-----------|--------|-------|
| Bun 1.3+  | Supported | `process.stdin`/`process.stdout` async I/O. |
| Node 22+  | Supported | Run with `node --experimental-strip-types plugin.ts`. Same `process.stdin`/`process.stdout` path as Bun. |
| Deno 2+   | Supported | Uses `Deno.stdin.readable` / `Deno.stdout.writable` directly (Deno's node-compat stdin async iteration has had EOF/backpressure gaps, so this path avoids it). |

The runtime is feature-detected at import time in `src/stdio.ts` — the only
module with any runtime-specific code. Everything else is plain
Web-standard APIs (`TextEncoder`/`TextDecoder`, `Uint8Array`) plus
`node:process`, which Bun and Deno both implement.

No npm dependencies at runtime. `typescript` (plus `@types/node`/`@types/bun`
for editor/typecheck support) are devDependencies only.

## Layout

- `src/stdio.ts` — newline-delimited JSON-RPC 2.0 endpoint over injectable
  `ByteReader`/`ByteWriter` (defaults to real stdin/stdout). Handles both
  directions on one stream: serves incoming requests/notifications and
  issues outgoing requests (id→resolver map, per-call timeout). Read and
  handler-dispatch are decoupled on purpose — a hook that itself makes an
  outgoing call (e.g. `ctx.storage.get`) must not block the loop that would
  deliver its response.
- `src/rpc.ts` — typed wrappers over the wire methods: `initialize` /
  `hook_invoke` / `tool_invoke` / `tool_cancel` / `panel_action` / `shutdown`
  handlers, and `HostClient` for
  `log_emit`/`storage_*`/`config_get`/`agent_*`/`ui_publish_panel`/`ui_close_panel`.
- `src/context.ts` — `PluginContext` (`log`, `storage`, `agents`, `ui`,
  `config()`, `workspaceRoot`, `sessionId`) and the per-call
  `ToolCallContext`.
- `src/define.ts` — `definePlugin()` (hooks + tools) and the gate-aware
  result helpers (`allow`, `deny`, `stopBlock`, `forceStop`, `observed`,
  `replace`).
- `src/generated/*.ts` — **read-only**, generated from the Rust side via
  `ts-rs`. Do not edit; do not redefine these shapes elsewhere. `src/index.ts`
  re-exports them.
- `test/` — `bun test` suite exercising the frame codec, request/response
  correlation (including out-of-order ids and concurrent-dispatch
  deadlock avoidance), the `definePlugin` handshake/dispatch/shutdown paths,
  and the `PluginContext`/`HostClient` RPCs.
- `test/smoke.node.ts` — not part of `bun test`; run directly with
  `node --experimental-strip-types test/smoke.node.ts` to verify the module
  graph resolves and loads under Node's type-stripping (explicit `.ts`
  import extensions, no non-erasable TS syntax).

## Verification

```sh
bun install
bun test                                     # unit tests
bun x tsc --noEmit                           # strict typecheck (src + test)
node --experimental-strip-types test/smoke.node.ts   # Node ESM/strip-types smoke
```
