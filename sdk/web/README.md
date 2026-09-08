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

**3. Connect.** The page opens on a form: paste the gateway URL
(`ws://127.0.0.1:2420/ws`, without the query string) and the secret, and press
Connect. Once it is up the form collapses to one line — a dot, the host, and
**Disconnect** — because the roster is what the column is for; pressing the host
brings the form back. The URL and the secret are remembered, so a reload
reconnects on its own and the secret is never rendered back into the page.

The sidebar fills with directories; each holds its sessions. Click one to attach
— history replays, live updates follow — or press **+ session here** to start a
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

## The transcript

An assistant reply and a reasoning block are markdown, because they are markdown
in the terminal: `AgentMessageBlock` and `ThinkingBlock` are two users of one
`MarkdownContent`. A user turn is not — the pager renders what was typed as
plain text, and marking it up would show the author something other than what
they wrote.

A tool call is titled the way the terminal titles it, from `rawInput` rather
than from the ACP `title`. That is not a preference: the pager treats `title` as
a fallback and actively refuses one that is only the tool's function name, so a
client printing it verbatim showed a bare grep pattern as a whole heading with
nothing saying a search had happened. `Search "pattern" in *.rs in src (3
matches in 2 files)`, `Run cargo test`, `Read src/main.rs (1-50 of 200)`,
`Creating src/lib.rs`, `Skill deploy` — each rule and its source is in
`src/toolcall.ts`, and `test/toolcall.test.ts` reads the pager's own files to
pin the constants.

**Output starts folded**, which is the pager's default for an agent tool call
(`default_display_mode` returns `Collapsed`). Pressing the header opens it, to
the head-and-tail the terminal keeps for a read or a shell command — with the
pager's own `… +N lines` — or the whole thing for a kind the terminal never
truncates. The one exception is an edit that succeeded: it opens showing its
diff, because `edit_default_display_mode` expands a fresh edit block whenever
`collapsed_edit_blocks` is off, and off is that flag's default. A failed call
colours its bullet and its rail `accent_error` and says what failed, down to the
exit code, which rides on the wire.

### The results the wire already carries

Three tools put their whole result on `rawOutput`, and this client draws it
rather than the prose beside it. `src/toolresult.ts` decodes each shape and
names the Rust that builds it; `test/toolresult.test.ts` reads those files so a
rename fails here rather than drifting.

| the tool sends | the page draws |
| --- | --- |
| `ReadFile::FileContent` — `raw_output`, `offset`, `total_lines` | the file's own text against its own line numbers |
| `GrepSearch` — `file_matches[].matches[].{line_number, content}` | one group per file, each hit at its line number |
| `SearchReplace::EditsApplied` — `edits.details[]` | a diff, banded and gutter-coloured per side |

**The read's numbers are the one place these two clients disagree on purpose.**
`offset` is one-based — the tool's own `resolve_read_start_line` says so in as
many words — and the pager computes `off + 1`, so it labels a read of lines
50-59 as `(51-60)` and numbers its gutter from 51. A line number is the part of
a read a person carries back out to an editor, so this client uses the tool's.

**A diff is not recomputed, because the hunk is on the wire.** Each edit detail
arrives with its line numbers on both sides and three lines of context already
cut — the same `MAX_CONTEXT` the pager's hunk builder would apply. What is not
on the wire is which lines *inside* the replacement survived it, and that is
computed: `similar::TextDiff::from_lines` in the terminal, and here a walk over
a longest common subsequence. Both are shortest edit scripts, so they agree on
how much changed; they can pick differently between two equally short scripts
when a line repeats, and that is the whole divergence. A library was weighed under
`docs/WEB-DEPS.md` and refused — `jsdiff` is a third implementation, not
`similar`, so it buys no agreement with the terminal while minimality, the
entire specification here, is what an LCS gives by construction.

**Two truncations are this client's own**, because the pager caps neither: its
search block draws every hit it was given and its blocks are bounded only by the
pane's height, which a scrolling page does not have. The transcript is not
virtualized, so a 2000-hit search would be 2000 live DOM nodes; hits and diff
rows stop at the grep tool's own `CONTENT_LINE_DEFAULT`, and whatever is dropped
is counted on the line that says so. A read keeps the pager's 5-and-3 elision
unchanged — with the numbers in the gutter, the bare `…` now states its own gap.

Syntax highlighting is the one thing here that is genuinely not a browser's:
`syntect` picks a grammar by file extension and, for a diff, re-reads the
post-edit file off disk on a worker thread so a construct opening above the hunk
still closes. Nothing on the wire carries either. Match spans inside a grep hit
are not drawn, and the terminal does not draw them either — the regex offsets
never leave the tool.

### ANSI, and the half of it that is not a browser's job

Tool output is a program's own bytes. A shell tool's `content` carries the raw
PTY stream, so escape sequences used to land on the page verbatim. The fix is
mostly not a conversion: `ToolOutput::Bash` also carries `output_for_prompt`,
which the shell already built by stripping ANSI for the model, and `raw_output`
is serialized to ACP untouched — **the clean text was on the wire the whole
time**, and this client now reads it.

Every other tool still arrives raw, so `src/ansi.ts` takes the escapes off.
It applies `\r` and `CSI K` as well, because without them a `cargo build` turns
one progress bar into dozens of rows; `test/fixtures/cargo-build.pty` is a real
capture of exactly that, and the test asserts on it. What it does *not* do is
emulate a terminal — no cursor addressing, no scroll regions, no colour. The
pager runs a real `vte`-driven emulator (`render/terminal_output.rs`), and a
second one written in JavaScript would be a second opinion about the same bytes.
Colour in tool output belongs on the wire, next to `output_for_prompt`, not in
each client.

## Switching the model

The model this session runs on sits in the header; pressing it opens the same
two-phase list the terminal opens on Ctrl+M. Type to filter, arrows to move,
Enter to take a row, Escape to step back out of the effort phase and then to
close.

**Nothing was added to the wire for it, and nothing changed in Rust.** The
catalog is in the reply to `session/load` — field `models`, an ACP
`SessionModelState` — and this client used to discard that reply whole. The
switch is standard ACP `session/set_model`. `session/new` answers with the same
field, and `initialize` carries a pre-session copy under `_meta.modelState`,
which is what the terminal's dashboard reads when there is no session yet.

Both phases are the pager's, row for row: the `(current)` suffix on `display`
only, so it can never affect the filter; the `(active)` suffix on an effort row
only when the chosen model is also the session's; and the filter itself, which
for *this* screen is a case-insensitive substring over the name, the label and
the description (`app/modals.rs`) rather than the `nucleo` ranking the inline
slash dropdown uses. That is why it is reproduced exactly instead of
approximated — `docs/WEB-DEPS.md` rejects JS fuzzy libraries because a different
algorithm is a different match set, and here there is no algorithm to port.

### The two calls, and why sending one is wrong

`/model <name>` in the terminal emits **two** effects — a `default_model` write
*and* a session switch (`app/dispatch/settings/setters.rs`) — while the
shell-side setter behind that key only writes the file
(`util/config/settings_apply.rs`). A client that sent the setting alone would
save a preference and leave the live session on the old model, contradicting the
catalog row's own description: *"Changing this also switches the active
session."* So a model with no reasoning effort switches **and** is remembered,
switch first: a default remembered for a model the agent refused would start the
*next* session on a model this one could not use.

`/model <name> <effort>` is the opposite and must **not** persist. The effort is
session-scoped and rides in `_meta.reasoningEffort` on the same `set_model`, so
the footer of the dialog says which of the two is about to happen.

A refused write is not a failure. `x.ai/settings/set` answers `applied: false`
with the shell's own sentence naming the file, which is `update_config`'s `stat`
protecting a declaratively configured machine from a browser exactly as it does
from a terminal.

### What is carried, and what is not

- **A row is keyed by id.** The terminal's rows are text to be typed into a
  composer, so its `insert_text` holds the model's *name* and it resolves that
  again on dispatch. An effort row is the sharper case: its menu id and its
  canonical value are different fields, and only the value goes on the wire
  (`sampling-types`, `ReasoningEffortOption`). Sending the id would name a level
  the agent does not have.
- **"More input expected" is a boolean, not a trailing space.** The terminal
  marks a reasoning model by ending `insert_text` with a space and detecting it
  with `ends_with(char::is_whitespace)`. That is a composer convention; a list
  can show a chevron.
- **`supportsReasoningEffort` defaults to false**, where its neighbours
  `firstParty` and `acceptsImages` default to true — so an absent flag means no
  effort menu, not an empty one. An absent, empty, junk or non-array
  `reasoningEfforts` all collapse to the built-in `xhigh/high/medium/low` menu,
  which the wire states as a contract.
- **Matching on text the row never draws is not.** The pager's effort rows
  carried an invisible `"a "` / `"b "` in `match_text`, there to steer a
  `nucleo` tiebreak — in a modal that does not rank with `nucleo` but filters
  with `contains` over that same field, so typing `a ` selected `xhigh` for no
  visible reason. This client never carried it, the terminal stopped carrying it
  in `3e446fad`, and a test on each side pins that neither does.
- **A switch made elsewhere moves this picker.** The shell broadcasts
  `model_changed` to every subscriber of the session, so a terminal on the same
  leader, or a second tab, is reflected here without asking for anything.

**One thing the terminal can do that this cannot:** make a *reasoning* model the
remembered default. Chosen from the terminal's own dialog it cannot either — the
effort phase is session-scoped — and the terminal's other route is typing
`/model <name>`, which is a pager command the shell never advertises, so a
browser has nothing to dispatch. The wire's home for it is the `default_model`
row of the settings catalog, already `surface: any` with `kind: dynamicEnum {
source: activeModelCatalog }` and already drawn here; this client still renders
settings read-only, and that is where the gap closes.

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

## Finding a file with `@`

Type `@` in the composer and the file list opens. Arrows move (Ctrl-N/P and
Ctrl-J/K too), PageUp and PageDown move half a screen, Tab or Enter takes the
row as `@path ` and closes it, the right arrow steps *into* a directory without
committing it, and Escape closes the list and leaves what was typed. A `@`
straight after a letter or `_` opens nothing, which is what keeps
`me@example.com` from becoming a file picker. A query ending in `/` asks for
directories only; one starting with `!` asks for hidden and ignored files.

**Nothing here matches, ranks or scores.** `x.ai/search/fuzzy/open`, `/change`
and `/close` run the same `nucleo` matcher over the same `ignore` walk the
terminal uses, and every batch carries `indices` — the character positions that
matched — because the agent had them. The rows are drawn from those positions.
The slash menu one section up computes its own, because the command catalog on
the wire carries none; that is the whole difference between the two lists, and
`src/highlight.ts` is the part they share.

Two things about the wire are worth stating, because neither is guessable from
the method names. `open` returns no results at all — it builds the matcher and
starts the walk, and only `change` spawns the status stream — so an empty query
is still sent as a `change`. And `path` arrives absolute while `indices` are
numbered against the path *relative to the search root*: the matcher strips the
root before scoring and the poll puts it back before sending. Highlighting the
delivered string with the delivered offsets lights up characters inside the
user's home directory instead of inside the file name.

### Who owns the search

The session, not the list. `open` builds a matcher and walks the whole tree, so
opening one per `@` would re-index the repository on every at-sign; the pager
does not do that either — its daemon lives as long as the process and the
dropdown is a view over it. So the search is opened on the first `@`, kept for
as long as this client stays attached, and closed when it attaches elsewhere or
disconnects. Re-opening the list is a `change` with an empty query, which is
what re-walks.

That matters because the agent has no hook for a client going away. A search is
freed by `close`, or by sitting idle for 300s **and** somebody else calling
`open` — the only caller of `cleanup_stale`. So if the socket drops with a
search open, its id is worthless (the stream is addressed to a leader client
that no longer exists, and a `close` has nowhere to go): this client forgets it
rather than pretending, and the orphan is collected by the next `open`, which is
this client's own, the next time anyone types `@`.

Hidden mode swaps the search rather than adding a parameter, because
`FuzzySearchContext.hidden` is fixed when the search opens. Typing `!` after the
`@` therefore costs one re-walk, and there is no other way to ask.

The count in the corner is the pager's: shown over indexed, with a `+` when the
cap bit. The cap is 100 — the agent's own default for `limit` — where the
terminal's is a thousand, because there a row costs a pointer and here it costs
bytes in a frame on every keystroke. There is no debounce, which is also the
pager's behaviour; a query typed while one is in flight replaces it rather than
queueing behind it.

Verified against a live gateway on this repository — 4483 files indexed, 87721
with `!` — in a real browser: browsing, a query with sixty hits, one with none,
one capped at a hundred, directory mode, drilling in, and the email guard.

## Trying a plugin panel

`test/fixtures/panel-probe` is a plugin that publishes one panel using all five
`PanelBlock` kinds. It knows nothing about browsers — it is the same publish the
pager renders.

```sh
GROK_HOME=~/.cache/grok-panel-probe                   # or your real ~/.grok
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

Typing survives that re-publish. A plugin repaints its whole panel on every
status tick, and the terminal treats keeping the half-typed line through it as
this layer's headline property (`PanelState::merge`, and a test named
`merge_reuses_editor_and_discards_new_value`). The browser now matches it: the
field is the same element after a repaint, and the value the re-publish carried
for a field that already exists is discarded.

## The widget rail

Panels can live in a column beside the session instead of in a stack above the
transcript. The shape is not ours: the pager designed it as `views/dock.rs`,
the Figma "Exploration" layout — named sections, a header carrying a count and a
rule, a section that folds, and a dock with nothing in it drawing nothing at all
— and the plugin protocol has promised a panel a "compact sidebar widget" since
it was written without any client building one.

The gate is the agent's. `dock_enabled` rides `x.ai/settings/update`, which the
shell forwards to **every** attached client rather than to the terminal that
caused the refresh, and this client used to drop that notification, so an
account the dock was on for saw no sign of it here. The checkbox in the sidebar
is the browser's own layer above it, and it is not an invention either: the
pager resolves the same feature through pin, environment, config file, then the
cohort flag, and a `[features] dock` in a file on that machine outranks the
rollout. A browser has no such file, so storage takes that place. The label says
which way the agent has voted, because a switch whose default comes from
elsewhere is unreadable without it.

Each section folds from its header; `↑` and `↓` walk headers and rows
together, as the dock's cursor does, and clamp at the ends rather than wrapping. **Open**
raises the panel in a dialog — the F6 overlay as a button, since F6 itself moves
focus between browser regions and is an accessibility control. Both surfaces
share one set of editors, so a code typed into the widget is still there in the
dialog. A plugin that throws while being drawn takes down its own widget and
nothing else.

### Which widths hold three columns

Measured in Chromium against a live gateway and a plugin publishing a
four-column table, not assumed:

| window | what it does |
| --- | --- |
| ≥ 1200px | three columns; the rail is 360–384px, which is what that table needs |
| < 1200px | the rail becomes a strip directly above the prompt, full width — the pager's own geometry, where the same table has 400px or more |
| < 760px | the navigator becomes a drawer: a button opens it, Escape and the scrim close it, and the page behind is `inert` while it is open |

At 1100px a third column is 300px wide and cuts that table's last column off
with nothing on screen to say so, which is why the breakpoint is 1200 and not
the round number.

**700px is the narrowest window this client is meant for.** It is not a guess:
at 700px the session column is 676px, and 80 monospace columns at this font are
672px — below that the transcript is narrower than the terminal it mirrors.
Nothing breaks between 480px and 700px (checked: no horizontal page scroll,
every control reachable), and below 480px it is untested. A phone layout is
deliberately not attempted: this is a view of a local machine over loopback.

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
The last one is here because the terminal parses both panel text and every
assistant reply with `pulldown-cmark`, so a parser that knew six constructs was
publishing a different panel than the plugin wrote and a different reply than
the model wrote. `markdown-it` is configured with `html: false`, so that text
can never become markup, and it is asked for tokens rather than for an HTML
string: `Markdown.tsx` builds DOM from those nodes, which is why there is no
`innerHTML` here and no sanitiser to remember. Only `https?:` keeps an `href`,
decided in `safeHref` and tested there.

A reply streams, so `markdown.ts` freezes what cannot change and re-parses only
the tail — the pager's own rule, and its own boundary: a checkpoint is a
top-level block, never one inside a list, a quote or a table
(`xai-grok-markdown/src/checkpoint.rs`). Blocks that have not changed are handed
back as the same objects, so `<For>` leaves their DOM — and any selection in it
— alone.

| file | what it holds |
| --- | --- |
| `src/wire.ts` | the slice of the protocol this client speaks, and nothing else |
| `src/auth.ts` | which sign-in a client may drive, and which it must refuse |
| `src/client.ts` | JSON-RPC 2.0 over the gateway's WebSocket |
| `src/gateway.ts` | the live connection, as reactive state |
| `src/roster.ts` | the roster, grouped by `cwd` |
| `src/directory.ts` | absolute-path arithmetic, and the listing params a picker needs |
| `src/transcript.ts` | folding `session/update` into a store |
| `src/models.ts` | the model catalog, the two phases, and the two calls a switch is |
| `src/toolcall.ts` | the title, the fold and the truncation the terminal gives a tool call |
| `src/toolresult.ts` | the typed result — a read's gutter, a search's hits, an edit's hunks |
| `src/ansi.ts` | escape sequences off arbitrary program output, and where that stops |
| `src/subagents.ts` | the fan-out: the fold, the labels, and what stays unknown |
| `src/commands.ts` | the slash catalog: provenance, matching, reading the composer |
| `src/markdown.ts` | the markdown parser's configuration, and which links keep an href |
| `src/panel.ts` | tone-to-role, and the block-kind exhaustiveness guard |
| `src/rail.ts` | the rail's line walk, ported from `views/dock.rs`, and who decides it is drawn |
| `src/focus.ts` | the focus trap every surface claiming `aria-modal` calls |
| `src/theme.ts` | generated palette to CSS custom properties |
| `src/App.tsx` | the screen and its routes |
| `src/components/` | AuthCard, Roster, Session, ToolResult, Subagents, Panel, Rail, Widget, Overlay, Markdown, ModelPicker, Settings, PermissionCard, FolderTrustCard, DirectoryPicker, CommandMenu |

`src/wire.ts` is hand-written on purpose and the reasoning is at the top of the
file: the panel types and the palette *are* generated and are imported, never
restated, but the conversational protocol has no generated form to import.

No name in this package appears on the wire. If this client ever needs something
the protocol cannot say, the answer is a change to the protocol, not a
client-prefixed method only one client understands.
