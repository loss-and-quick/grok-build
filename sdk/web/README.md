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

## Signing in

Until this existed, a browser could not authenticate at all. With no credential
on disk the agent installs no auth method, and every `session/new` and
`session/load` is refused with `no auth method id provided`
(`agent_ops.rs:4494`) — so the page worked only if a terminal had already logged
in to the same leader. That made it a companion to a terminal rather than a
client.

Now the page signs the agent in itself, through exactly the calls the terminal
uses. `initialize` carries `authMethods` and `defaultAuthMethodId`; this client
authenticates on the agent's own choice, or shows a card when the agent says it
has nothing. A login is `authenticate` plus a concurrent poll of
`x.ai/auth/get_url` — concurrent because `authenticate` does not return until
the whole login is over, so the URL a person needs to finish it can only be
collected alongside. `x.ai/auth/submit_code` hands back a pasted code and
`x.ai/auth/cancel` abandons the attempt, both scoped by the same `request_seq`
the pager sends.

**No credential passes through the browser.** The agent runs the flow, mints the
token and writes it to `~/.grok/auth.json` itself — the same
`run_auth_flow_steps` a `grok login` runs. Two methods on this wire would change
that and neither is called anywhere in this package: `x.ai/auth/getBearerToken`
hands a client the live bearer, and `x.ai/setApiKey` lets a client install one,
writing `auth.json` and the agent's process environment for every client on the
leader. A test walks `src/` and fails if either name appears. The only
secret-shaped thing that ever crosses is an authorization *code* on the paste
path, and it is single-use and PKCE-bound: the verifier is generated inside the
agent and never leaves it.

### Which flows work here, and which do not

Every method the agent ships can be completed in a browser, and that is
structural rather than lucky — the agent does the authenticating, so a client's
whole part is a link, sometimes a code, sometimes a pasted one back, and a
cancel button. None of those four is a terminal capability.

| the agent reports | the card shows |
| --- | --- |
| `loopback` | the sign-in address, and a paste box |
| `device` | the user code, the address, and no paste box |
| `command` | a waiting status while the provider's own browser runs |

The paste box is drawn for `loopback` and nowhere else. Only that flow races a
pasted code against the callback listener (`oidc/login.rs`,
`race_callback_and_client_ui`); `device` polls the token endpoint and `command`
waits on a subprocess, and neither reads `code_rx` at all — a box there would
swallow whatever was typed into it.

It is worth knowing where the callback lands: on the *agent's* loopback address,
not the browser's. When the page and the agent share a machine — the default,
since the gateway binds loopback — it completes itself. Reached through a tunnel
it cannot, and the dead page's own address is the answer; that is what the paste
box is for, and it is the same limit a terminal has over SSH.

**Two things are deliberately refused, each with the place it can be done
instead.**

*A method this build does not recognise.* An unknown id may want a device code,
a paste box, a second round trip or none of those, and nothing on the wire says
which — so the card offers no button and names `grok login` in a terminal on the
agent's machine. The advertised list is the contract in both directions: the
agent's `authenticate` rejects an id that is not on it, and this client refuses
to drive one it cannot finish.

*Typing an API key into the browser.* `x.ai/setApiKey` exists and would work,
and it is not offered. The advertised list is the reason: `xai.api_key` is
advertised only when a key already exists (`auth_method.rs`,
`should_advertise_xai_api_key`), so offering a box to create one is offering a
method the agent did not advertise. The terminal has no such box either — keys
come from the environment or `config.toml`, which is also the one place they
carry provenance. So when the agent advertises nothing at all — a
`[auth] preferred_method = "api_key"` pin with no key, which builds an empty
list and a `None` default — the card says no client can sign in from here and
names `XAI_API_KEY` and `config.toml` on the agent's machine.

### Is the shared auth method the folder-trust bug again?

No, and the difference is worth stating because the shapes look alike.
`auth_method_id` is one `ArcSwapOption` for the whole agent, and every client's
`initialize` rewrites it — which is exactly what `interactive_trust` did before
`54cc4c3d`. But that flag named a *client* property, whether this client can
draw a trust card, and two clients honestly differ on it; the fix was to carry
each client's own answer in its session request. `auth_method_id` names which
credential the one `AuthManager`, over the one `~/.grok/auth.json`, is currently
serving. There is no per-client answer to carry: every session's turns are
signed with the same bearer. A browser login is the same act as a terminal
login, for the same user, on the same store.

What is true is that a login is machine-wide. Signing in here signs in the
terminal sharing this leader, and the agent's single flight means starting one
login cancels another in flight — from either side, exactly as two terminals
already do to each other.

## Slash commands

Press `/` in the composer and the shell's own catalog opens: its builtins, the
skills it found on disk, the workflows it knows, and the commands a plugin
declared in its manifest. Arrows move, Tab or Enter inserts the name with a
trailing space, Escape closes it, and the command's argument hint stands in as
the composer's placeholder until arguments are typed.

Nothing is asked of the wire. The catalog arrives as `available_commands_update`
— a standard ACP session update on a carrier this client already listens to —
and `session/load` asks the session for one on every attach, so attaching is
what fills the menu. Choosing a row types `/name args` and sending it is the
dispatch: the shell resolves the leading token against the catalog it
advertised, and a plugin's command reaches that plugin's own code.

Each row carries the terminal's provenance badge — `built-in`, `skill · user`,
`plugin · acme` — because the wire carries provenance: a markdown skill rides
with `scope` and `path`, and a plugin's manifest command with `pluginCommand`
and `pluginName`. The terminal shows the badge only on rows in a name collision,
since it has a `/plugins` screen to ask instead; the browser shows it on every
row, because this menu is the only place it can say where a command came from.

The terminal hides seven shell-advertised names because it has its own UI for
them (`BLOCKED_ACP_NAMES`). This client shows all seven, and the reason is in
`PAGER_BLOCKED` in `src/commands.ts`, per command: every one of them reports
through `send_host_turn_slash_command_output`, which is an ordinary agent
message chunk that a browser already draws, and there is no `/hooks` or
`/plugins` screen here to reach them by instead. `/help` is on that list as a
name *reservation* — the shell never advertises it — so there is nothing to
hide. A test reads the pager's list and fails when a name lands there without a
verdict here.

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

## Watching a fan-out

A session that spawns subagents shows them in their own pane above the
transcript: one row per child, with what it is, what it is doing right now,
how long it has been at it, and how it ended.

The pane is separate from the transcript because a fan-out is *state*, not
events. Three children each rewrite their own line several times a minute, and
interleaving that with a streaming reply scrolls the thing you are watching off
the screen. The terminal reached the same conclusion from the other side: it
writes one scrollback line per child and keeps the live view in a docked pane
(`views/tasks_pane.rs`).

Nothing was added to the wire for this. The three updates already ride the
parent session's own stream — `subagent_spawned`, `subagent_progress`,
`subagent_finished` (`extensions/notification.rs:654`) — and the leader
subscribes a client to each child at spawn by copying the parent's subscriber
set (`leader/server.rs:2315`), which is how the browser sees a child's own
tool calls without asking for them.

### What each row is allowed to say

Every field is something the wire carried. A counter that has not arrived reads
as **—**, and a child that has not said anything yet reads as **unknown** —
never as a zero. "It has made no tool calls" and "the agent has not told this
client yet" are different facts, and the second one is the normal state for the
first two seconds of every child's life.

That is not a hypothetical. A child that finishes before its first progress tick
ends with `context —` and `errors —` for good: those two fields exist only on
the tick, and there never was one.

| what the row shows | where it comes from |
| --- | --- |
| the label, the model, `resumed` / `forked` | `subagent_spawned` |
| turns, tools, tokens, context %, errors | `subagent_progress`, every ~2s (8s heartbeat) |
| elapsed | the agent's own `duration_ms`, plus the wait since that frame |
| what it is doing now | the child session's own chunks and tool calls |
| completed / failed / cancelled, and the answer | `subagent_finished` |

Labelling is the terminal's, not this client's invention: persona, then role,
then type, then a `[tag]` prefix, then `general`; `general-purpose` displays as
`general`; the `[tag]` is stripped from the description either way
(`app/subagent.rs:840`). So is the order — running first, then agent type, then
newest — and hiding finished rows behind a toggle (`views/tasks_pane.rs:941`,
`:900`).

**Attaching in the middle of a fan-out works**, and takes one extra call.
`session/load` replays every `subagent_spawned` and `subagent_finished` this
session ever wrote, so which children exist is known immediately; progress ticks
are deliberately never persisted (`agent/subagent/mod.rs:2049`), so their
counters would be unknown until each child's next tick. `x.ai/subagent/list_running`
fills them in at once — the call the shell itself names for this case
(`:2050`), and one the terminal does not make.

### Stopping a child

Stopping is destructive and not undoable: the child's turn is cancelled where it
stands and nothing it had done is handed back to the parent. **The button asks
twice.** The first click arms it — the label becomes *confirm stop* — and the
arming expires on its own.

The terminal does not ask at all: `x` on the selected row sends the cancel
(`app/agent_view/panes.rs:502`). That is defensible there and not here, because
`x` is the second half of a gesture whose first half was moving a cursor onto
the row; a click on a button in a list is the whole gesture. The expiry is the
pager's own `PENDING_KILL_TIMEOUT_SECS` (`app/agent.rs:153`), which it uses for
the same idea — how long a stop that has not resolved keeps a row marked — and
a test reads the Rust so the two cannot drift.

Three answers come back and only one of them means a finish is coming
(`extensions/task.rs:79`). `cancelled` leaves the row marked until the real
`subagent_finished` lands. `already_finished` carries the child's true status.
`not_found` means the agent has no record of the id — **and this client says
`unknown` there, where the terminal writes `cancelled`**
(`app/dispatch/task_result.rs:780`). That substitution reports a stop that may
not have happened; not knowing is the honest answer and the one that sends a
person to look rather than to move on.

### What the terminal knows and a browser cannot

Worth stating plainly, because each of these is either a product gap or a
computation that belongs on the wire:

- **The task prompt.** `subagent_spawned` carries the model's one-line
  `description`, never the prompt the child was actually given. The terminal
  reads it from `meta.json` on disk (`app/subagent.rs:85`, filled by `enrich_from_meta` at `:246`).
  A browser has no disk.
- **The child's working directory and worktree.** Same file, same reason. So a
  browser cannot even `session/load` a child by id: it has no `cwd` to send.
  Child sessions are also excluded from the roster on purpose — their summaries
  are `hidden` (`agent/roster.rs:276`).
- **The child's transcript before this client attached.** The pane shows what a
  child says from the moment the browser is subscribed. The terminal replays the
  child's `updates.jsonl` from disk when you open it fullscreen.
- **A start time on the spawn.** `subagent_spawned` has no timestamp, so a
  client that missed it can only date the child from the first tick — which is
  why the pane calls `list_running`, whose `startedAtEpochMs` is the only place
  the wire states it.
- **Which tool call spawned which child.** `subagent_spawned` carries no
  `tool_call_id`, so the `spawn_subagent` tool call in the parent's transcript
  and the row in this pane cannot be linked. Both clients live with it; only a
  wire field would fix it.
- **Initializing versus running.** The agent distinguishes them and will report
  the difference through `x.ai/subagent/get`, but it never announces it, so a
  client watching the stream sees a spawned child as running from the first
  frame. This client matches the terminal and calls it running.
- **The waiting reasons and retry states** in the terminal's activity line. The
  three this pane draws — Thinking, Responding, Running: *tool* — are the arms
  of `format_activity_label` a client can read off the update stream
  (`app/subagent.rs:891`). The rest come from state the agent reports to the
  pager on paths a browser is not on.

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

Three runtime dependencies: `solid-js`, `@solidjs/router`, and `markdown-it`.
The last one is here because the terminal parses panel text with
`pulldown-cmark` and `PanelBlock::Markdown` promises that renderer's output —
headings, lists, tables, code — so a parser that knew six constructs was
publishing a different panel than the plugin wrote. `markdown-it` is configured
with `html: false`, so a panel's text can never become markup, and it is asked
for tokens rather than for an HTML string: `Markdown.tsx` builds DOM from those
nodes, which is why there is no `innerHTML` here and no sanitiser to remember.
Only `https?:` keeps an `href`, decided in `safeHref` and tested there.

| file | what it holds |
| --- | --- |
| `src/wire.ts` | the slice of the protocol this client speaks, and nothing else |
| `src/auth.ts` | which sign-in a client may drive, and which it must refuse |
| `src/client.ts` | JSON-RPC 2.0 over the gateway's WebSocket |
| `src/gateway.ts` | the live connection, as reactive state |
| `src/roster.ts` | the roster, grouped by `cwd` |
| `src/directory.ts` | absolute-path arithmetic, and the listing params a picker needs |
| `src/transcript.ts` | folding `session/update` into a store |
| `src/subagents.ts` | the fan-out: the fold, the labels, and what stays unknown |
| `src/commands.ts` | the slash catalog: provenance, matching, reading the composer |
| `src/markdown.ts` | the markdown parser's configuration, and which links keep an href |
| `src/panel.ts` | tone-to-role, and the block-kind exhaustiveness guard |
| `src/theme.ts` | generated palette to CSS custom properties |
| `src/App.tsx` | the screen and its routes |
| `src/components/` | AuthCard, Roster, Session, Subagents, Panel, Markdown, Settings, PermissionCard, FolderTrustCard, DirectoryPicker, CommandMenu |

`src/wire.ts` is hand-written on purpose and the reasoning is at the top of the
file: the panel types and the palette *are* generated and are imported, never
restated, but the conversational protocol has no generated form to import.

No name in this package appears on the wire. If this client ever needs something
the protocol cannot say, the answer is a change to the protocol, not a
client-prefixed method only one client understands.
