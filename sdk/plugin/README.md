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
  `hook_invoke` / `shutdown` handlers, and `HostClient` for
  `log_emit`/`storage_*`/`config_get`.
- `src/context.ts` — `PluginContext` (`log`, `storage`, `config()`,
  `workspaceRoot`, `sessionId`).
- `src/define.ts` — `definePlugin()` and the gate-aware result helpers
  (`allow`, `deny`, `stopBlock`, `forceStop`, `observed`, `replace`).
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
