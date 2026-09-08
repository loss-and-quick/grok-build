# @grok-build/web

A browser client for `grok agent gateway`. It lists the leader's sessions
**grouped by working directory**, attaches to one, streams its transcript, sends
prompts, answers permission prompts, and renders plugin panels.

It is a proof that the seam works, not a product. The TUI is still the complete
client; what this shows is that a second one needs no privileged path — every
byte it moves is a frame the pager also moves.

Why it lives in `sdk/`: it is the third consumer of the generated artifacts that
already live here. `sdk/theme` gives it every colour, `sdk/plugin` gives it the
panel types, `sdk/settings` gives it the settings catalog. Putting it anywhere
else would mean copying one of those, and copying is the failure this whole
arrangement exists to prevent.

## Running it against a live gateway

You need a `grok` built from this tree — `agent gateway` landed recently and a
released binary will reject the subcommand.

```sh
cargo build -p xai-grok-pager-bin       # produces target/debug/xai-grok-pager
```

**1. Start the gateway.** It prints its own secret and URL.

```sh
./target/debug/xai-grok-pager agent gateway
#   WebSocket URL: ws://127.0.0.1:2420/ws?server-key=<secret>
```

It binds loopback and attaches each browser connection to the leader the TUI is
already talking to, so a running `grok` and this page share sessions. Pass
`--secret` (or `GROK_AGENT_SECRET`) to pin the secret instead of generating one.

**2. Build and serve the page.**

```sh
cd sdk/web
bun install
bun run dev        # http://127.0.0.1:2421, with hot reload
# or
bun run build && bun run preview
```

The dev server never talks to the leader: the page opens its own WebSocket, so
the secret goes from the browser to the gateway and through nothing else. Both
servers are pinned to `127.0.0.1`.

**3. Connect.** Paste the gateway URL (`ws://127.0.0.1:2420/ws`, without the
query string) and the secret into the two fields, and press Connect. The
sidebar fills with directories; each holds its sessions. Click one to attach —
history replays, live updates follow — or press **+ session here** to start a
new session in that directory.

**New session…** opens a directory picker instead, so a session can start in a
root no client has opened before. It begins at the directory `initialize`
names, walks up and down, and takes a typed absolute path. Nothing about it is
new on the wire: `x.ai/fs/list` walks an absolute path as given and
`session/new` accepts any absolute `cwd`. The listing asks for git-ignored
entries explicitly — the agent hides them by default, which would drop
`target/` and anything a parent `.gitignore` names, and those are perfectly
good places to work.

Attaching navigates to `/s/<sessionId>`, so a session is a URL: reload it,
bookmark it, or open it in a second tab, and the page comes back attached. The
leader treats every tab as its own client, so two tabs on one session is an
ordinary multi-client case rather than a special one. `/d/<cwd>` opens a
directory's most recent session.

The theme picker offers the six palettes from `sdk/theme`. Nothing in this
client names a colour; see "Colour" below.

## Trying a plugin panel

`test/fixtures/panel-probe` is a plugin that publishes one panel using all five
`PanelBlock` kinds. It knows nothing about browsers — it is the same publish the
pager renders.

```sh
GROK_HOME=/tmp/…                                     # or your real ~/.grok
cp -rL sdk/web/test/fixtures/panel-probe "$GROK_HOME/plugins/panel-probe"
```

then add it to `config.toml`:

```toml
[plugins]
enabled = ["panel-probe"]
```

Start a session and the panel appears. Type into a field and press **Echo**: the
press routes back through `x.ai/plugins/panel_action`, the plugin republishes the
panel with what it received, and the new version replaces the old one.

## Folder trust

A session opened in a directory that is not in `trusted_folders.toml` resolves
**untrusted**, and that project's MCP servers, hooks, plugins, LSP and permission
rules are dropped without a word. The agent asks about it with
`x.ai/folder_trust/request`, and this client answers it.

Three outcomes, because the terminal has three and they differ. **Trust** grants
the workspace and hot-reloads it. **Leave untrusted** declines, and the agent
keeps its per-workspace key, so nothing asks again. **Ask me next time** leaves
it undecided: the pager expresses that by dropping the response channel, and a
browser reaches the same place by answering with a JSON-RPC error, which the
agent reads as "not a decision" — it stays gated and releases the key. Sending
`{"outcome": "dismiss"}` would be a *reject*, because the agent's enum decodes
anything but `"trust"` that way.

The card is drawn above the session rather than inside it. The leader routes
this request to the client that opened the session and never replays it, so it
can arrive before that session is attached, and a card dropped for that reason
is a project silently running without its own configuration.

`browser_capabilities()` in
`crates/codegen/xai-grok-shell/src/agent/web_gateway.rs` registers
`interactive_trust: Some(true)`, which is what makes the agent send the request
to a browser at all. It declares that for every authenticated WebSocket client,
not only this one, and it can afford to: a client that answers with an error, an
undecodable payload or nothing at all leaves the workspace gated exactly as a
refusal to declare would. Only an explicit `"trust"` unblocks.

## Tests

```sh
bun test          # unit tests; no gateway needed
bun run typecheck
```

`test/setup.ts` registers two things Bun needs and Vite provides on its own:
`babel-preset-solid`, because Solid's reactivity is a compile-time transform,
and a rewrite of the `solid-js`, `solid-js/store` and `solid-js/web` specifiers,
because Bun resolves with the `node` condition and every one of those points it
at a *server* build. All three matter: on the server core `createEffect` and
`onMount` are empty functions, so redirecting only the renderer left components
rendering once and standing still, with nothing to say so.
`test/environment.test.tsx` is what says so now.

`test/live.test.tsx` drives the real `App` against a running gateway and is
skipped unless you point it at one:

```sh
GROK_WEB_LIVE_URL=ws://127.0.0.1:2420/ws \
GROK_WEB_LIVE_SECRET=<secret> \
bun test test/live.test.tsx
```

It needs the `panel-probe` fixture installed and a model provider configured,
because it asserts on a real panel and a real streamed reply.

## Colour

Every colour comes from `sdk/theme/src/generated/themes.ts`, which is serialized
from the pager's own Rust `Theme` constructors. `src/theme.ts` turns each role
into a CSS custom property and `src/styles.css` only ever names those
properties — there is not one literal colour in this package. If a colour you
want is missing from the generated set, that is a finding, not a licence to
invent one.

Two translations are this client's own, because only a browser needs them:

- `"reset"` becomes `canvas` for a background role and `canvastext` for a
  foreground one, as the generated file's own doc comment prescribes. Which
  roles are backgrounds is spelled out in `BACKGROUND_ROLES`, and a test fails
  when a new generated role arrives unclassified.
- `muted_uses_dim` becomes opacity, reproducing the SGR dim that `Theme::muted`
  uses when a palette's gray is `"reset"`.

`"idx:N"` — the 256-colour palette — is expressible in `ThemeColor` but the
generated artifact exports no table beyond ANSI 0–15. No shipped theme uses it,
and a test fails the day one does.

## Layout

Vite + SolidJS + TypeScript. Solid because of streaming: an assistant reply
arrives as many small chunks that grow one message in place, so a virtual DOM
would reconcile the whole transcript to update a single text node. Bun stays
the package manager and test runner; Vite is here only for Solid's JSX
transform, which the reactivity depends on.

| file | what it holds |
| --- | --- |
| `src/wire.ts` | the slice of the protocol this client speaks, and nothing else |
| `src/client.ts` | JSON-RPC 2.0 over the gateway's WebSocket |
| `src/gateway.ts` | the live connection, as reactive state |
| `src/roster.ts` | the roster, grouped by `cwd` |
| `src/directory.ts` | absolute-path arithmetic, and the listing params a picker needs |
| `src/transcript.ts` | folding `session/update` into a store |
| `src/markdown.ts` | the panel markdown parser, and which links keep an href |
| `src/panel.ts` | tone-to-role, and the block-kind exhaustiveness guard |
| `src/theme.ts` | generated palette to CSS custom properties |
| `src/App.tsx` | the screen and its routes |
| `src/components/` | Roster, Session, Panel, Markdown, Settings, PermissionCard, FolderTrustCard, DirectoryPicker |

`src/wire.ts` is hand-written on purpose and the reasoning is at the top of the
file: the panel types and the palette *are* generated and are imported, never
restated, but the conversational protocol has no generated form to import.

No name in this package appears on the wire. If this client ever needs something
the protocol cannot say, the answer is a change to the protocol, not a
client-prefixed method only one client understands.
