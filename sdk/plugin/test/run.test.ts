// Tests for the `_sdk/run` launcher (src/run).
//
// The launcher is a POSIX sh script whose whole job is choosing a program and
// an argv, so the tests drive it against a fabricated PATH holding fake
// bun/node/deno that print the argv they were handed. Nothing here touches a
// real runtime, so the assertions hold on a machine with none installed.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const RUN = join(import.meta.dir, "..", "src", "run");

let root: string;
/** `<root>/plugin/` — stands in for a deployed plugin directory. */
let pluginRoot: string;
/** `<root>/plugin/_sdk/run` — where the launcher lives once deployed. */
let shim: string;
/** `<root>/workspace/` — the cwd the host gives a sidecar. */
let workspace: string;

beforeAll(() => {
  root = mkdtempSync(join(tmpdir(), "grok-run-test-"));
  pluginRoot = join(root, "plugin");
  workspace = join(root, "workspace");
  mkdirSync(join(pluginRoot, "_sdk"), { recursive: true });
  mkdirSync(workspace, { recursive: true });

  // Deploy the launcher the way the plugin build does: a real copy at
  // <plugin>/_sdk/run, executable, with no node_modules anywhere near it.
  shim = join(pluginRoot, "_sdk", "run");
  copyFileSync(RUN, shim);
  chmodSync(shim, 0o755);
});

afterAll(() => rmSync(root, { recursive: true, force: true }));

/**
 * Write a fake runtime binary into `<root>/bin-<label>/<name>`, returning the
 * directory to put on PATH. The fake prints its own argv (one per line,
 * `argv:` prefixed) plus its pid, so a test can assert both the constructed
 * command line and that the launcher `exec`ed rather than forked.
 */
function fakeRuntime(label: string, name: string, extra = ""): string {
  const dir = join(root, `bin-${label}`);
  mkdirSync(dir, { recursive: true });
  const path = join(dir, name);
  writeFileSync(
    path,
    [
      "#!/bin/sh",
      // Before anything else: a `--version` probe must see only the version.
      extra,
      `echo "self:${name}"`,
      'echo "pid:$$"',
      'echo "cwd:$(pwd)"',
      'for a in "$@"; do echo "argv:$a"; done',
    ].join("\n") + "\n",
  );
  chmodSync(path, 0o755);
  return dir;
}

/** A fake node that answers `--version` with `version` before echoing argv. */
function fakeNode(label: string, version: string): string {
  return fakeRuntime(
    label,
    "node",
    [
      'if [ "${1-}" = "--version" ]; then',
      `  printf '%s\\n' '${version}'`,
      "  exit 0",
      "fi",
    ].join("\n"),
  );
}

interface RunResult {
  code: number;
  stdout: string;
  stderr: string;
  pid: number;
  /** The argv the fake runtime received, in order. */
  argv: string[];
}

async function runShim(
  args: string[],
  opts: { path?: string[]; cwd?: string; env?: Record<string, string> } = {},
): Promise<RunResult> {
  const proc = Bun.spawn([shim, ...args], {
    cwd: opts.cwd ?? workspace,
    // A deliberately minimal PATH: only the fakes the test asked for, so a
    // real bun/node/deno on the developer's machine cannot influence a result.
    env: { PATH: (opts.path ?? []).join(":"), ...(opts.env ?? {}) },
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  return {
    code,
    stdout,
    stderr,
    pid: proc.pid,
    argv: stdout
      .split("\n")
      .filter((l) => l.startsWith("argv:"))
      .map((l) => l.slice("argv:".length)),
  };
}

/** Create `<pluginRoot>/<name>` so the launcher's existence check passes. */
function entryFile(name: string): string {
  const path = join(pluginRoot, name);
  writeFileSync(path, "// entry\n");
  return path;
}

describe("runtime discovery", () => {
  test("prefers bun over node and deno, and passes only the entry", async () => {
    const entry = entryFile("index.ts");
    const bun = fakeRuntime("bun", "bun");
    const node = fakeNode("node24", "v24.0.0");
    const deno = fakeRuntime("deno", "deno");

    const r = await runShim(["index.ts"], { path: [bun, node, deno] });
    expect(r.code).toBe(0);
    expect(r.stdout).toContain("self:bun");
    expect(r.argv).toEqual([entry]);
  });

  test("falls back to node when bun is absent", async () => {
    entryFile("index.ts");
    const node = fakeNode("node24", "v24.0.0");
    const deno = fakeRuntime("deno", "deno");

    const r = await runShim(["index.ts"], { path: [node, deno] });
    expect(r.stdout).toContain("self:node");
  });

  test("falls back to deno when neither bun nor node is present", async () => {
    entryFile("index.ts");
    const deno = fakeRuntime("deno", "deno");

    const r = await runShim(["index.ts"], { path: [deno] });
    expect(r.stdout).toContain("self:deno");
  });

  test("skips an unsupported node in the auto chain rather than failing", async () => {
    // The host's auto chain treats a too-old node as "not a candidate" and
    // gives deno its turn; only an explicit `runtime: node` is fatal.
    entryFile("index.ts");
    const node = fakeNode("node20", "v20.11.0");
    const deno = fakeRuntime("deno", "deno");

    const r = await runShim(["index.ts"], { path: [node, deno] });
    expect(r.code).toBe(0);
    expect(r.stdout).toContain("self:deno");
    expect(r.stderr).toContain("older than v22");
  });

  test("honours --runtime= over the discovery order", async () => {
    entryFile("index.ts");
    const bun = fakeRuntime("bun", "bun");
    const deno = fakeRuntime("deno", "deno");

    const r = await runShim(["--runtime=deno", "index.ts"], { path: [bun, deno] });
    expect(r.stdout).toContain("self:deno");
  });

  test("honours GROK_PLUGIN_RUNTIME, and the flag wins over it", async () => {
    entryFile("index.ts");
    const bun = fakeRuntime("bun", "bun");
    const deno = fakeRuntime("deno", "deno");

    const viaEnv = await runShim(["index.ts"], {
      path: [bun, deno],
      env: { GROK_PLUGIN_RUNTIME: "deno" },
    });
    expect(viaEnv.stdout).toContain("self:deno");

    const viaFlag = await runShim(["--runtime=bun", "index.ts"], {
      path: [bun, deno],
      env: { GROK_PLUGIN_RUNTIME: "deno" },
    });
    expect(viaFlag.stdout).toContain("self:bun");
  });

  test("execs the runtime rather than forking it", async () => {
    // The host supervises the spawned pid and signals it at shutdown, so a
    // shell must not remain between the two.
    entryFile("index.ts");
    const bun = fakeRuntime("bun", "bun");

    const r = await runShim(["index.ts"], { path: [bun] });
    expect(r.stdout).toContain(`pid:${r.pid}`);
  });
});

describe("node version probe", () => {
  test.each([
    ["v22.0.0", true],
    ["v22.11.0", true],
    ["v23.0.0", true],
    ["v23.5.9", true],
    ["v23.6.0", false],
    ["v23.7.1", false],
    ["v24.0.0", false],
  ])("%s -> strip flag: %s", async (version, wantsFlag) => {
    const entry = entryFile("index.ts");
    const node = fakeNode(`node-${version}`, version);

    const r = await runShim(["--runtime=node", "index.ts"], { path: [node] });
    expect(r.code).toBe(0);
    expect(r.argv).toEqual(
      wantsFlag ? ["--experimental-strip-types", entry] : [entry],
    );
  });

  test("tolerates a version with no minor component", async () => {
    const entry = entryFile("index.ts");
    const node = fakeNode("node-bare", "v23");

    const r = await runShim(["--runtime=node", "index.ts"], { path: [node] });
    expect(r.argv).toEqual(["--experimental-strip-types", entry]);
  });

  test("assumes the strip flag when --version is unparseable", async () => {
    const entry = entryFile("index.ts");
    const node = fakeNode("node-garbage", "garbage");

    const r = await runShim(["--runtime=node", "index.ts"], { path: [node] });
    expect(r.code).toBe(0);
    expect(r.stderr).toContain("unparseable");
    expect(r.argv).toEqual(["--experimental-strip-types", entry]);
  });

  test("assumes the strip flag when the probe itself fails", async () => {
    const entry = entryFile("index.ts");
    const dir = join(root, "bin-node-broken");
    mkdirSync(dir, { recursive: true });
    const path = join(dir, "node");
    writeFileSync(
      path,
      [
        "#!/bin/sh",
        'if [ "${1-}" = "--version" ]; then exit 3; fi',
        'echo "self:node"',
        'for a in "$@"; do echo "argv:$a"; done',
      ].join("\n") + "\n",
    );
    chmodSync(path, 0o755);

    const r = await runShim(["--runtime=node", "index.ts"], { path: [dir] });
    expect(r.code).toBe(0);
    expect(r.stderr).toContain("node --version failed");
    expect(r.argv).toEqual(["--experimental-strip-types", entry]);
  });
});

describe("deno permissions", () => {
  test("scopes read and write to the workspace and withholds net by default", async () => {
    const entry = entryFile("index.ts");
    const deno = fakeRuntime("deno", "deno");

    const r = await runShim(["index.ts"], { path: [deno] });
    expect(r.argv).toEqual([
      "run",
      "--no-prompt",
      `--allow-read=${workspace}`,
      `--allow-write=${workspace}`,
      entry,
    ]);
  });

  test("adds --allow-net only when the plugin declares it", async () => {
    const entry = entryFile("index.ts");
    const deno = fakeRuntime("deno", "deno");

    const r = await runShim(["--net", "index.ts"], { path: [deno] });
    expect(r.argv).toEqual([
      "run",
      "--no-prompt",
      `--allow-read=${workspace}`,
      `--allow-write=${workspace}`,
      "--allow-net",
      entry,
    ]);
  });

  test("scopes to the real cwd even when PWD is inherited stale", async () => {
    // The host sets the child's cwd but not its PWD, so a stale value can be
    // inherited from the host's environment. It must never reach the
    // allow-list.
    entryFile("index.ts");
    const deno = fakeRuntime("deno", "deno");

    const r = await runShim(["index.ts"], {
      path: [deno],
      env: { PWD: "/definitely/not/the/workspace" },
    });
    expect(r.argv).toContain(`--allow-read=${workspace}`);
    expect(r.stdout).not.toContain("/definitely/not/the/workspace");
  });
});

describe("entry resolution", () => {
  test("resolves a relative entry against the plugin root, not the cwd", async () => {
    // The trap this exists to avoid: the sidecar's cwd is the *workspace*, so
    // a bare `index.ts` would otherwise be looked up in the user's project.
    const entry = entryFile("index.ts");
    writeFileSync(join(workspace, "index.ts"), "// decoy\n");
    const bun = fakeRuntime("bun", "bun");

    const r = await runShim(["index.ts"], { path: [bun] });
    expect(r.argv).toEqual([entry]);
    expect(r.stdout).toContain(`cwd:${workspace}`);
  });

  test("passes an absolute entry through untouched", async () => {
    const entry = entryFile("other.ts");
    const bun = fakeRuntime("bun", "bun");

    const r = await runShim([entry], { path: [bun] });
    expect(r.argv).toEqual([entry]);
  });

  test("forwards trailing arguments to the entry", async () => {
    const entry = entryFile("index.ts");
    const bun = fakeRuntime("bun", "bun");

    const r = await runShim(["index.ts", "--flag", "a b"], { path: [bun] });
    expect(r.argv).toEqual([entry, "--flag", "a b"]);
  });

  test("stops option parsing at --", async () => {
    const entry = entryFile("index.ts");
    const bun = fakeRuntime("bun", "bun");

    const r = await runShim(["--", "index.ts", "--net"], { path: [bun] });
    expect(r.argv).toEqual([entry, "--net"]);
  });
});

describe("failure is loud", () => {
  test("exits 127 with a stderr diagnostic when no runtime is found", async () => {
    entryFile("index.ts");
    const empty = join(root, "bin-empty");
    mkdirSync(empty, { recursive: true });

    const r = await runShim(["index.ts"], { path: [empty] });
    expect(r.code).toBe(127);
    expect(r.stdout).toBe("");
    expect(r.stderr).toContain("no JavaScript runtime");
    expect(r.stderr).toContain("bun");
    expect(r.stderr).toContain("node (>=22)");
    expect(r.stderr).toContain("deno");
  });

  test("exits 127 when an explicitly pinned runtime is missing", async () => {
    entryFile("index.ts");
    const bun = fakeRuntime("bun", "bun");

    const r = await runShim(["--runtime=deno", "index.ts"], { path: [bun] });
    expect(r.code).toBe(127);
    expect(r.stderr).toContain("deno not on PATH");
  });

  test("exits 127 when an explicitly pinned node is too old", async () => {
    entryFile("index.ts");
    const node = fakeNode("node18", "v18.19.0");
    const deno = fakeRuntime("deno", "deno");

    const r = await runShim(["--runtime=node", "index.ts"], { path: [node, deno] });
    expect(r.code).toBe(127);
    expect(r.stderr).toContain("older than v22");
    expect(r.stderr).toContain("cannot run TS plugins");
  });

  test("exits 127 when the entry file does not exist", async () => {
    const bun = fakeRuntime("bun", "bun");

    const r = await runShim(["nope.ts"], { path: [bun] });
    expect(r.code).toBe(127);
    expect(r.stderr).toContain("plugin entry not found");
    expect(r.stderr).toContain(join(pluginRoot, "nope.ts"));
  });

  test("rejects an unknown runtime name and an absent entry with a usage error", async () => {
    const bun = fakeRuntime("bun", "bun");

    const badRuntime = await runShim(["--runtime=python", "index.ts"], { path: [bun] });
    expect(badRuntime.code).toBe(2);
    expect(badRuntime.stderr).toContain("unknown runtime 'python'");

    const noEntry = await runShim([], { path: [bun] });
    expect(noEntry.code).toBe(2);
    expect(noEntry.stderr).toContain("missing plugin entry file");

    const badFlag = await runShim(["--nope", "index.ts"], { path: [bun] });
    expect(badFlag.code).toBe(2);
    expect(badFlag.stderr).toContain("unknown option: --nope");
  });

  test("keeps every diagnostic off stdout, which is the JSON-RPC channel", async () => {
    entryFile("index.ts");
    const node = fakeNode("node20", "v20.11.0");

    // Warns about the skipped node, then fails to find anything else.
    const r = await runShim(["index.ts"], { path: [node] });
    expect(r.stdout).toBe("");
    expect(r.stderr).not.toBe("");
  });
});
