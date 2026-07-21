<div align="center">

<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://media.x.ai/v1/website/spacexai-symbol-white-transparent-0c31957f.png">
    <source media="(prefers-color-scheme: light)" srcset="https://media.x.ai/v1/website/spacexai-symbol-black-transparent-6435cf42.png">
    <img alt="SpaceXAI logo" src="https://media.x.ai/v1/website/spacexai-symbol-black-transparent-6435cf42.png" width="96">
  </picture>
  <br>
  Grok Build (<code>grok</code>)
</h1>

**Grok Build** is SpaceXAI's terminal-based AI coding agent. It runs as a
full-screen TUI that understands your codebase, edits files, executes shell
commands, searches the web, and manages long-running tasks — interactively,
headlessly for scripting/CI, or embedded in editors via the Agent Client
Protocol (ACP).

[Installing the released binary](#installing-the-released-binary) ·
[Building from source](#building-from-source) ·
[Documentation](#documentation) ·
[Repository layout](#repository-layout) ·
[Development](#development) ·
[Contributing](#contributing) ·
[License](#license)

![Grok Build TUI](https://media.x.ai/v1/website/universe-tui-screenshot-6f7a0837.png)

**Learn more about Grok Build at [x.ai/cli](https://x.ai/cli)**

This repository contains the Rust source for the `grok` CLI/TUI and its agent
runtime. It is synced periodically from the SpaceXAI monorepo.

A small `SOURCE_REV` file at the root records the full monorepo commit SHA
for the version of the code present in this tree.

</div>

## About this fork

A fork of [xai-org/grok-build](https://github.com/xai-org/grok-build) that adds
a TypeScript plugin system.

### Why this fork exists

Upstream grok-build has a strong Rust core — a hooks dispatcher, a subagent
system, an OS-level sandbox (Landlock on Linux, Seatbelt on macOS), and a
mature ratatui TUI — but no extensibility short of recompiling the whole
workspace. This fork adds a TypeScript plugin system on top of that core
without changing it: plugins run as sidecar processes and talk to the core
over a versioned JSON-RPC wire contract. A plugin never sees core internals,
and a plugin crash doesn't take the core down with it.

### What's here now

- `plugin.json` gains a `"plugin": "./index.ts"` sidecar entry. The runtime is
  auto-discovered in preference order **bun → node (>=22) → deno**, or pinned
  explicitly via a `"runtime"` field. No vendoring, no embedded JS engine.
- All 15 hook events bridged from the core's hook dispatcher are available to
  TS plugins, each with its typed gate semantics: **Observe** (acknowledged,
  no control), **Tool** (allow/deny a tool call, with a reason), or **Stop**
  (block the stop and/or inject `additionalContext` into the next turn).
- Per-plugin KV storage (`ctx.storage`), structured logging (`ctx.log`), and
  manifest-config access (`ctx.config()`), all over the same RPC channel — no
  self-managed files or locks in plugin code.
- Fail-open supervision: a plugin that never starts, times out, or crashes
  doesn't block the hook it's gating — the host falls back to "no gate
  applied" and restarts the sidecar with exponential backoff, disabling it
  after too many consecutive crashes.
- Network is denied by default (`"network": false` in `plugin.json`). On
  Linux this is enforced with a per-child seccomp filter installed on the
  sidecar unless the plugin opts in; this enforcement is not yet wired up on
  non-Linux hosts.
- `@grok-build/plugin` ([`sdk/plugin/`](sdk/plugin/)) — the TypeScript SDK:
  `definePlugin()`, wire types generated from the Rust side, and a typed
  `ctx` (log/storage/config). No build step: Bun and Deno run the source
  directly, Node runs it via `--experimental-strip-types`.
- A reference plugin at
  [`examples/plugins/demo-hooks/`](examples/plugins/demo-hooks/), with e2e
  tests asserting its `pre_tool_use`/`stop` behavior matches an equivalent
  command-hook.
- A Nix devshell (`nix develop`), a `grok`/`xai-grok-pager` package, and a
  Home Manager module (`programs.grok-build`, in
  [`nix/hm-module.nix`](nix/hm-module.nix)) exposing `enable`, `settings`
  (written to `~/.grok/config.toml`), and `plugins` (deployed to
  `~/.grok/plugins/<name>/`).

### Planned

- Provider request interception and model fallback.
- Programmatic subagent orchestration for plugins.
- OAuth flows via plugins.
- Plugin TUI panels.
- WebSocket access to the headless server.

### Quickstart

```sh
nix develop                                  # toolchain + bun/node/deno, see nix/shells.nix
cargo build -p xai-grok-pager-bin --release  # build the grok binary
```

A plugin is a directory with a `plugin.json` and an entry file:

```json
{
  "name": "my-plugin",
  "plugin": "./index.ts"
}
```

```ts
import { definePlugin, allow, deny } from "@grok-build/plugin";

definePlugin({
  name: "my-plugin",
  hooks: {
    pre_tool_use: (payload, ctx) => {
      // inspect payload; use ctx.log / ctx.storage / ctx.config() as needed
      return allow();
    },
  },
});
```

See [`examples/plugins/demo-hooks/`](examples/plugins/demo-hooks/) for a
fuller reference plugin and [`sdk/plugin/README.md`](sdk/plugin/README.md)
for the full SDK surface.

Plugins are discovered from, in priority order: `--plugin-dir`,
`.grok/plugins/*/` (project scope, walked up to the worktree root),
`~/.grok/plugins/*/` (user scope, always trusted), and `[plugins].paths` in
config. `.claude/plugins/` paths are also read for compatibility.

### Relationship to upstream

Upstream syncs continue as before. Core crate names (`xai-grok-*`) are
unchanged. The plugin system lives in its own crates
(`xai-grok-plugin-host`, `xai-grok-plugin-protocol`) plus a thin
`HandlerType::Plugin` seam added to the existing hooks dispatcher — the
plugin surface is additive and nothing upstream depends on it.

---

## Installing the released binary

Prebuilt binaries are published for macOS, Linux, and Windows:

```sh
curl -fsSL https://x.ai/cli/install.sh | bash   # macOS / Linux / Git Bash
irm https://x.ai/cli/install.ps1 | iex          # Windows PowerShell
grok --version
```

See the [changelog](https://x.ai/build/changelog) for the latest fixes,
features, and improvements in each release.

## Building from source

Requirements:

- **Rust** — the toolchain is pinned by [`rust-toolchain.toml`](rust-toolchain.toml);
  `rustup` installs it automatically on first build.
- **[DotSlash](https://dotslash-cli.com)** — required so hermetic tools under
  [`bin/`](bin/) (notably [`bin/protoc`](bin/protoc)) can download and run.
  Install it and ensure `dotslash` is on your `PATH` **before** building:

  ```sh
  cargo install dotslash
  # or: prebuilt packages — https://dotslash-cli.com/docs/installation/
  /usr/bin/env dotslash --help   # sanity check
  ```

- **protoc** — proto codegen resolves [`bin/protoc`](bin/protoc) via DotSlash,
  or falls back to a `protoc` on `PATH` / `$PROTOC`.
- macOS and Linux are supported build hosts; Windows builds are best-effort
  and not currently tested from this tree.

```sh
cargo run -p xai-grok-pager-bin              # build + launch the TUI
cargo build -p xai-grok-pager-bin --release  # release binary: target/release/xai-grok-pager
cargo check -p xai-grok-pager-bin            # fast validation
```

The binary artifact is named `xai-grok-pager`; official installs ship it as
`grok`. On first launch it opens your browser to authenticate — see the
[authentication guide](crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md).

## Documentation

Full online documentation is available at
[docs.x.ai/build/overview](https://docs.x.ai/build/overview).

The user guide ships with the pager crate:
[`crates/codegen/xai-grok-pager/docs/user-guide/`](crates/codegen/xai-grok-pager/docs/user-guide/)
— getting started, keyboard shortcuts, slash commands, configuration, theming,
MCP servers, skills, plugins, hooks, headless mode, sandboxing, and more.

## Repository layout

| Path | Contents |
|------|----------|
| `crates/codegen/xai-grok-pager-bin` | Composition-root package; builds the `xai-grok-pager` binary |
| `crates/codegen/xai-grok-pager` | The TUI: scrollback, prompt, modals, rendering |
| `crates/codegen/xai-grok-shell` | Agent runtime + leader/stdio/headless entry points |
| `crates/codegen/xai-grok-tools` | Tool implementations (terminal, file edit, search, ...) |
| `crates/codegen/xai-grok-workspace` | Host filesystem, VCS, execution, checkpoints |
| `crates/codegen/...` | The rest of the CLI crate closure (config, MCP, markdown, sandbox, ...) |
| `crates/common/`, `crates/build/`, `prod/mc/` | Small shared leaf crates pulled in by the closure |
| `third_party/` | Vendored upstream source (Mermaid diagram stack) — see below |

> [!IMPORTANT]
> The root `Cargo.toml` (workspace members, dependency versions, lints,
> profiles) is **generated** — treat it as read-only. Prefer editing per-crate
> `Cargo.toml` files.

## Development

```sh
cargo check -p <crate>        # always target specific crates; full-workspace builds are slow
cargo test -p xai-grok-config # per-crate tests
cargo clippy -p <crate>       # lint config: clippy.toml at the repo root
cargo fmt --all               # rustfmt.toml at the repo root
```

## Contributing

> [!NOTE]
> External contributions are not accepted. See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

First-party code in this repository is licensed under the **Apache License,
Version 2.0** — see [`LICENSE`](LICENSE).

Third-party and vendored code remains under its original licenses. See:

- [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES) — crates.io / git dependencies,
  bundled UI themes, and **in-tree source ports** (including openai/codex and
  sst/opencode tool implementations)
- [`crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md`](crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md)
  — crate-local notice for the codex and opencode ports (license texts +
  Apache §4(b) change notice)
- [`third_party/NOTICE`](third_party/NOTICE) — vendored Mermaid-stack index
