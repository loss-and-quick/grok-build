import { describe, expect, test } from "bun:test";

import {
  LIST_PARAMS,
  ROOT,
  breadcrumbs,
  childOf,
  emptyReason,
  isAbsolute,
  normalizePath,
  parentOf,
  partition,
} from "../src/directory.ts";
import type { FsNode } from "../src/wire.ts";

function node(over: Partial<FsNode> & { name: string }): FsNode {
  return { path: `/x/${over.name}`, type: "file", ...over };
}

describe("path arithmetic, because the agent does none of it", () => {
  test("`..` is resolved here, since the agent hands it straight back", () => {
    // `fs/list` on `/a/b/..` answers with entries whose `path` is literally
    // `/a/b/../name`. Walking up from one of those would grow a path that never
    // shortens, so every path this client holds is normalized before it is sent.
    expect(normalizePath("/a/b/..")).toBe("/a");
    expect(normalizePath("/a/b/../../c")).toBe("/c");
    expect(normalizePath("/a/./b")).toBe("/a/b");
    expect(normalizePath("/a//b///c")).toBe("/a/b/c");
    expect(normalizePath("/a/b/")).toBe("/a/b");
    expect(normalizePath("  /a/b  ")).toBe("/a/b");
  });

  test("climbing past the root stays at the root", () => {
    expect(normalizePath("/../../..")).toBe(ROOT);
    expect(normalizePath("/")).toBe(ROOT);
  });

  test("a relative path is left alone, so the agent's own refusal is what shows", () => {
    // The agent answers `sessionId is required for relative paths`. Inventing a
    // base here would turn that honest error into a silent wrong directory.
    expect(normalizePath("crates/x")).toBe("crates/x");
    expect(isAbsolute("crates/x")).toBe(false);
    expect(isAbsolute("/crates")).toBe(true);
  });

  test("the root is the one path with no parent", () => {
    expect(parentOf("/a/b/c")).toBe("/a/b");
    expect(parentOf("/a")).toBe(ROOT);
    expect(parentOf(ROOT)).toBeNull();
    expect(parentOf("relative")).toBeNull();
  });

  test("a child of the root does not double the separator", () => {
    expect(childOf(ROOT, "home")).toBe("/home");
    expect(childOf("/home", "me")).toBe("/home/me");
    expect(childOf("/home/", "me")).toBe("/home/me");
  });

  test("breadcrumbs are every ancestor, root first, each a place to jump to", () => {
    expect(breadcrumbs("/home/me/grok-build")).toEqual([
      { label: ROOT, path: ROOT },
      { label: "home", path: "/home" },
      { label: "me", path: "/home/me" },
      { label: "grok-build", path: "/home/me/grok-build" },
    ]);
    expect(breadcrumbs(ROOT)).toEqual([{ label: ROOT, path: ROOT }]);
    expect(breadcrumbs("relative")).toEqual([]);
  });
});

describe("a listing page", () => {
  test("the wire's order is preserved, not re-derived", () => {
    // The agent sorts directories first, then case-insensitively by name
    // (`xai-grok-workspace/src/file_system/walk.rs`). Re-sorting here would be a
    // second opinion about the same list, which is exactly how two clients
    // start to disagree.
    const { directories, files } = partition([
      node({ name: "zed", type: "directory" }),
      node({ name: "apps", type: "directory" }),
      node({ name: "README.md" }),
      node({ name: "Cargo.toml" }),
    ]);
    expect(directories.map((d) => d.name)).toEqual(["zed", "apps"]);
    expect(files.map((f) => f.name)).toEqual(["README.md", "Cargo.toml"]);
  });

  test("a symlinked directory is still a directory", () => {
    // The walk follows links, so `type` is already `"directory"` and
    // `isSymlink` rides alongside as a note. Treating the flag as a kind would
    // hide a perfectly ordinary place to put a session.
    const { directories } = partition([node({ name: "result", type: "directory", isSymlink: true })]);
    expect(directories).toHaveLength(1);
  });
});

describe("what the wire cannot say", () => {
  test("an empty page is three different things, and the message says so", () => {
    // `fs/list` answers `{nodes: [], truncated: false}` for an empty directory,
    // for a path that is a file, and for a directory the leader may not read.
    // `fs/exists` separates only the missing case. Naming the ambiguity is
    // honest; picking one of the two and printing it would be wrong half the
    // time.
    expect(emptyReason(false)).toBe("No such directory.");
    expect(emptyReason(true)).toContain("not readable");
  });
});

describe("the listing params a picker must send", () => {
  test("git-ignored directories are asked for, because the default hides them", () => {
    // This is the one option that is *not* the agent's default, and it is the
    // one that matters: with `respectGitIgnore` left at `true`, listing this
    // repository omits `target/`, `docs/` and `Cargo.toml`, because a parent
    // `.gitignore` names them. `session/new` would accept any of those roots
    // happily — so the default would hide places the product supports.
    expect(LIST_PARAMS.respectGitIgnore).toBe(false);
  });

  test("one directory at a time, hidden entries included", () => {
    // Depth 2 flattens grandchildren into the same list with nothing to say
    // which parent they came from, so two `src` entries become
    // indistinguishable. Hidden entries stay because `.config` and a dotted
    // worktree root are places sessions really live.
    expect(LIST_PARAMS.depth).toBe(1);
    expect(LIST_PARAMS.includeHidden).toBe(true);
  });
});
