# demo-hooks

A minimal TypeScript **sidecar plugin** for grok-build, used as the reference
plugin and e2e-parity fixture: proof that plugin hooks reach the same dispatcher
outcomes as native command hooks.

## What it does

| Hook            | Gate    | Behavior                                                                 |
| --------------- | ------- | ------------------------------------------------------------------------ |
| `session_start` | Observe | Logs the session id via the host `log_emit` capability channel.          |
| `pre_tool_use`  | Tool    | Denies any tool call whose input JSON contains the marker `DEMO_DENY_MARKER`, with a fixed reason. |
| `stop`          | Stop    | Injects `additionalContext` for the next turn (does **not** block the stop). |

It also serves one model-visible **tool**:

| Tool   | Model-facing name  | Behavior                                                        |
| ------ | ------------------ | --------------------------------------------------------------- |
| `echo` | `demo-hooks__echo` | Echoes `text` back with the per-call `cwd` and calling `agent`. |

The tool is declared twice on purpose: `plugin.json`'s `tools` array is what
the session tool catalog (and therefore the model) sees, and
`definePlugin({ tools })` provides the handler that runs in this sidecar. The
host warns at handshake if the two drift.

The deny reason and stop context are exported as constants from `index.ts`; the
Rust e2e test (`plugin_sidecar_e2e_tests.rs` in `xai-grok-shell`) asserts an
equivalent command hook produces byte-identical values.

## Layout

```
demo-hooks/
  plugin.json   # manifest: "plugin": "./index.ts" marks it a TS sidecar
  index.ts      # definePlugin({ tools: { ... }, hooks: { ... } })
  README.md
```

`plugin.json` declares:

- `"plugin": "./index.ts"` — the sidecar entry (its presence is what makes this
  a TS sidecar plugin).
- `"runtime": "auto"` — the host probes `bun → node (>=22) → deno` and runs the
  first found. No build step; the runtime executes the `.ts` source directly.
- `"network": false` — the sidecar child is spawned under the per-child seccomp
  network filter on Linux (the host applies the shell-injected spawn hardener).
- `"tools": [{ "name": "echo", ... }]` — the model-visible tool catalog entry
  (name, description, JSON input schema; optional `timeoutMs` per tool).

## Importing the SDK

A real, installed plugin imports the SDK by its published package name:

```ts
import { definePlugin, deny, observed } from "@grok-build/plugin";
```

This in-repo example has **no `node_modules`**, so `index.ts` instead imports the
SDK source directly by relative path:

```ts
import { definePlugin, deny, observed } from "../../../sdk/plugin/src/index.ts";
```

That is the only difference from a distributed plugin. When packaging a real
plugin you would depend on `@grok-build/plugin` and use the bare import; the
hook code is otherwise identical.

## Running it

Point a session at this directory as a plugin dir (or install it), then trigger
a tool call whose input contains `DEMO_DENY_MARKER` to see the deny, or let a
turn end to see the injected stop context. Ask the model to call
`demo-hooks__echo` to see the tool round trip. Sidecars start lazily on the
first matching hook or tool call — a plugin that never fires an event it
subscribed to never costs a process.
