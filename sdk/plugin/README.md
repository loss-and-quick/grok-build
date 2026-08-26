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

## Telling the model something (`injectContext`)

`ctx.log` is the plugin's own log channel — the model never sees it. To hand
the model something a hook computed, return `injectContext(text)`:

```ts
import { definePlugin, injectContext } from "@grok-build/plugin";

definePlugin({
  name: "scratchpad",
  hooks: {
    session_start: async (payload) => {
      const dir = `${payload.workspaceRoot}/.scratch/${payload.sessionId}`;
      await mkdir(dir, { recursive: true });   // node:fs/promises
      return injectContext(
        `A scratch directory for this session exists at ${dir}. ` +
          `Use it for throwaway files instead of /tmp.`,
      );
    },
  },
});
```

The host folds the text into the session's opening `<user_info>` context, so
it is in front of the model from turn one and stays there for the whole
session — including after a compaction, which rebuilds that context verbatim.
Several plugins may contribute; their entries are concatenated in dispatch
order into one `<system-reminder>` block. On a **resumed** session the loaded
transcript already carries the previous run's block, so a fresh contribution
arrives as a standalone reminder and is folded into the prefix at the next
compaction.

Three things to know:

- **Keep it short.** The block is billed on every request for the rest of the
  session. The host clips the rendered block at 4 000 characters, marking the
  cut (`… [+N chars]`) rather than dropping the text.
- **`session_start` is the consuming event.** Other observe events accept
  `additional_context` on the wire and record it, but have no injection point.
- **Don't stall startup.** `session_start` runs before the first turn and the
  session waits for it, bounded by the hook timeout (5 s by default). Do the
  slow part in the background and return promptly.

The same text can also come from a plain `hooks.json` command hook, via
`{"hookSpecificOutput": {"additionalContext": "…"}}` on stdout — the spelling
the `Stop` gate already uses.

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

Two different ways to say something more to a subagent, and they are not
interchangeable:

```ts
// Still running: steer it. Lands in the live child's conversation before
// its next inference request; same id, no new subagent.
const outcome = await ctx.agents.message(id, "skip the tests, just the parser");
if (outcome !== "delivered") { /* "not_delivered" | "not_started" | ... */ }

// Already finished: continue it. Resumes that conversation into a FRESH
// child — key wait/events/cancel on the returned id, not the old one.
const nextId = await ctx.agents.send(id, "now write the tests", 60_000);
```

`message` never queues: if the child's turn ended before the text landed
you get `"not_delivered"` and nothing was added to its conversation, so
re-send if the correction still matters. It is text only — no attachments,
and no slash-command expansion.

The model reaches the same mechanism from the other side, through the
`message_subagent` tool: same coordinator, same six outcomes, same injection
point in the child. A plugin and the parent model can both steer the same
child; the messages arrive in the order the coordinator took them. What the
model does *not* have is an equivalent of `send` — it continues a terminal
subagent with `task`'s `resume_from`, which likewise mints a new id.

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

## Interactive sign-in (`ctx.auth`)

A `start_oauth_flow` handler runs *before* any session exists — the user is
signing in — so there is no session to publish a panel into. What is on
screen is the core's own login screen, and `ctx.auth` drives it: hand it the
authorize URL, then read back the code the user submits there.

```ts
definePlugin({
  hooks: {
    async start_oauth_flow(_payload, ctx) {
      const { url, verifier } = buildAuthorizeUrl();
      if (!(await ctx.auth.publishUrl(url))) {
        // No sign-in screen is waiting (the flow was not started from
        // `/login`) — fall back to whatever UI suits the plugin.
        return null;
      }
      const code = await ctx.auth.awaitCode();
      if (code === null) return null; // cancelled or timed out
      return replace(await exchangeCode(code, verifier)); // → the credential
    },
  },
});
```

`awaitCode()` may be called again after a rejected code: the screen's input
stays wired for the whole sign-in. Hosts without the sign-in wiring reject
both calls with JSON-RPC `method_not_found` (-32601), so feature-detect the
same way as `ctx.agents`.

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

## Launcher (`_sdk/run`)

A plugin manifest launches a sidecar either as `plugin` + `runtime` (the host
finds a JS runtime and builds the argv for a TypeScript entry) or as `exec` (the
host runs a program verbatim). `src/run` lets a TypeScript plugin use the
generic `exec` form: it is the runtime-finding step, moved to the SDK.

```json
{ "exec": ["${GROK_PLUGIN_ROOT}/_sdk/run", "index.ts"] }
```

It is a POSIX `sh` script — the job is to *find* a JavaScript runtime, so it
cannot be written in JavaScript. It needs no build step, no `node_modules`, and
no external command: `command -v`, `cd` and `pwd` are shell builtins, so it runs
under whatever `PATH` the host has. Ship it wherever the SDK lands in a deployed
plugin (`<plugin>/_sdk/` for a `cp -R` of `src/`), with the executable bit set —
the manifest layer rejects a non-executable `exec` program.

```
run [--runtime=auto|bun|node|deno] [--net] [--] <entry.ts> [args...]
```

- **Discovery** is `bun → node (>=22) → deno`, first found wins, matching the
  host. For node it probes `node --version` to decide whether
  `--experimental-strip-types` is needed (unflagged from 23.6) and to reject a
  node too old to strip types at all; in the `auto` chain a too-old node is
  skipped so deno still gets a turn, and only an explicit `--runtime=node` makes
  it fatal. An unparseable or failed probe keeps the flag, which is safe on the
  22 line.
- **`--runtime=`** pins a runtime, standing in for the manifest's `runtime`
  field, which the `exec` form has no room for. `$GROK_PLUGIN_RUNTIME` is the
  fallback; the flag wins.
- **The entry is resolved against the plugin root**, i.e. the parent of the
  launcher's own directory — *not* the working directory. A sidecar's cwd is the
  workspace root, so a bare `index.ts` would otherwise be looked up in the
  user's project. Absolute entries pass through. Everything after the entry is
  forwarded to it.
- **Deno** gets `--no-prompt` and `--allow-read=`/`--allow-write=` scoped to the
  workspace root, which the launcher reads from its own working directory —
  that is the one place the host's `workspace_root` reaches this side of `exec`.
  `--allow-net` is added only for `--net`.
- **Failure is loud**: no usable runtime, or a missing entry, exits 127 with a
  diagnostic on stderr naming what was looked for and the `PATH` it searched.
  A usage error exits 2. Nothing is ever written to stdout, which is the
  JSON-RPC channel.
- **`exec`, not fork**: the runtime replaces the launcher process, so the host
  supervises and signals the plugin's own pid with no shell in between.

### `--net` is a second declaration, unavoidably

Whether a plugin may reach the network is a manifest field the host reads, and
it reaches the child in no form at all — not argv, not the environment, and not
the `initialize` handshake, which happens long after argv is fixed. A launcher
on this side of `exec` therefore cannot know it, and deno's `--allow-net` has to
be restated with `--net` alongside the manifest's `"network": true`. Without the
flag it fails closed, matching the manifest default. On Linux the enforcement
that actually matters for `"network": false` is the host's per-child seccomp
filter, which applies to the launcher and every descendant regardless of what
deno was told.

### Cost

No discovery cache. `command -v` walks `PATH` in-process, so the bun and deno
paths fork nothing extra and only node costs one `node --version` per spawn
(~20 ms, against node's own ~50 ms startup). The host caches process-wide
because it outlives a spawn; a launcher process does not, and the only writable
places to persist to are the user's workspace or state directory — not
somewhere a launcher should be writing, and a cache stale after a toolchain
upgrade fails worse than the probe costs. `--runtime=` skips discovery for
anyone who disagrees.

## Layout

- `src/run` — the launcher above: finds a JS runtime, `exec`s the entry under
  it. POSIX `sh`, not TypeScript, and not part of the module graph.
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
  `log_emit`/`storage_*`/`config_get`/`agent_*`/`ui_publish_panel`/`ui_close_panel`/
  `auth_publish_url`/`auth_await_code`.
- `src/context.ts` — `PluginContext` (`log`, `storage`, `agents`, `ui`, `auth`,
  `config()`, `workspaceRoot`, `sessionId`) and the per-call
  `ToolCallContext`.
- `src/define.ts` — `definePlugin()` (hooks + tools) and the gate-aware
  result helpers (`allow`, `deny`, `stopBlock`, `forceStop`, `observed`,
  `injectContext`, `replace`).
- `src/generated/*.ts` — **read-only**, generated from the Rust side via
  `ts-rs`. Do not edit; do not redefine these shapes elsewhere. `src/index.ts`
  re-exports them.
- `test/` — `bun test` suite exercising the frame codec, request/response
  correlation (including out-of-order ids and concurrent-dispatch
  deadlock avoidance), the `definePlugin` handshake/dispatch/shutdown paths,
  and the `PluginContext`/`HostClient` RPCs. `test/run.test.ts` drives
  `src/run` against a fabricated `PATH` of fake bun/node/deno, so its
  assertions hold on a machine with none of them installed.
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
