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

And two **slash commands**, which is how a *person* reaches this code:

| Command  | Behavior                                                                    |
| -------- | --------------------------------------------------------------------------- |
| `/greet` | Answers the user directly from the handler; `/greet nobody` refuses instead. |
| `/ask`   | Composes a prompt in code and hands it to the model.                         |

Same two-sided declaration as tools: `plugin.json`'s `slashCommands` array is
what the `/` menu shows, `definePlugin({ commands })` provides the handler, and
the host warns on drift. Unlike a `commands/*.md` file — whose body is only ever
substituted into the user's message — these run the plugin's own code and decide
for themselves whether the model is called at all.

The deny reason and stop context are exported as constants from `index.ts`; the
Rust e2e test (`plugin_sidecar_e2e_tests.rs` in `xai-grok-shell`) asserts an
equivalent command hook produces byte-identical values.

## Layout

```
demo-hooks/
  plugin.json   # manifest: "exec" names the program that speaks the protocol
  index.ts      # definePlugin({ tools, commands, hooks })
  _sdk/         # the SDK, including the `run` launcher `exec` points at
  README.md
```

`plugin.json` declares:

- `"exec": ["${GROK_PLUGIN_ROOT}/_sdk/run", "index.ts"]` — the sidecar entry.
  A plugin is a program that speaks the protocol; the host runs this argv and
  knows nothing about JavaScript. `_sdk/run` is the SDK launcher: it probes
  `bun → node (>=22) → deno` and `exec`s the entry under the first found. No
  build step; the runtime executes the `.ts` source directly.
- `"network": false` — the sidecar child is denied the network by whatever this
  platform provides (a seccomp filter on Linux, a `sandbox-exec` Seatbelt
  profile on macOS), applied by the shell-injected spawn hardener. Nothing
  enforces it on Windows; see the README for what happens there.
- `"tools": [{ "name": "echo", ... }]` — the model-visible tool catalog entry
  (name, description, JSON input schema; optional `timeoutMs` per tool).
- `"slashCommands": [{ "name": "greet", ... }]` — the `/` menu entries (name,
  description, optional `argumentHint` and `timeoutMs`). Arguments are free
  text; the hint is only what the menu displays.

## Importing the SDK

A real, installed plugin imports the SDK by its published package name:

```ts
import { definePlugin, deny, observed } from "@grok-build/plugin";
```

This in-repo example has **no `node_modules`**, so `index.ts` instead imports the
SDK source directly by relative path (`_sdk/` is a symlink to the same tree,
which is what a packaged plugin gets as a real copy):

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
`demo-hooks__echo` to see the tool round trip. Type `/greet` (or `/greet Ada`,
or `/ask why is the sky blue`) to see a slash command run this plugin's code.
Sidecars start lazily on the first matching hook, tool, or command call — a
plugin that never fires an event it subscribed to never costs a process.
