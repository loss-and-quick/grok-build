import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";

import { DirectoryPicker } from "../src/components/DirectoryPicker.tsx";
import type { Gateway } from "../src/gateway.ts";
import type { FsListResponse, FsNode } from "../src/wire.ts";

const TREE: Record<string, FsNode[]> = {
  "/home/me/repo": [
    { name: "crates", path: "/home/me/repo/crates", type: "directory" },
    { name: "target", path: "/home/me/repo/target", type: "directory" },
    { name: "result", path: "/home/me/repo/result", type: "directory", isSymlink: true },
    { name: "Cargo.toml", path: "/home/me/repo/Cargo.toml", type: "file" },
  ],
  "/home/me": [{ name: "repo", path: "/home/me/repo", type: "directory" }],
  "/home/me/repo/crates": [],
};

/** Only what the picker touches; the rest of the gateway is not its business. */
function stub(over: Partial<Gateway> = {}): { gateway: Gateway; listed: string[] } {
  const listed: string[] = [];
  const gateway = {
    agentCwd: () => "/home/me/repo",
    listDirectory: async (path: string): Promise<FsListResponse> => {
      listed.push(path);
      return { nodes: TREE[path] ?? [], truncated: false };
    },
    pathExists: async (path: string) => path in TREE,
    ...over,
  } as unknown as Gateway;
  return { gateway, listed };
}

function settle(ms = 20): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function mount(gateway: Gateway, onOpen: (cwd: string) => void = () => {}) {
  return render(() => DirectoryPicker({ gateway, onOpen, onClose: () => {} }));
}

describe("choosing a working directory", () => {
  test("starts where `initialize` said the agent is", async () => {
    // The agent names its own launch directory in `initialize`'s `_meta`, which
    // this client used to throw away. It is a starting point, not a limit.
    const { gateway, listed } = stub();
    const { container } = mount(gateway);
    await settle();
    expect(listed).toEqual(["/home/me/repo"]);
    expect(container.querySelector(".picker-chosen")?.textContent).toBe("/home/me/repo");
  });

  test("directories are enterable, files are shown and are not", async () => {
    const { gateway } = stub();
    const { container } = mount(gateway);
    await settle();
    const names = [...container.querySelectorAll(".picker-dir .picker-name")].map(
      (n) => n.textContent,
    );
    expect(names).toEqual(["crates/", "target/", "result/"]);
    expect(container.querySelectorAll(".picker-file .picker-name")[0]?.textContent).toBe(
      "Cargo.toml",
    );
    // A file row is not a button, so it cannot be walked into.
    expect(container.querySelector("button.picker-file")).toBeNull();
    // A symlinked directory is still a directory, and says so.
    expect(container.querySelectorAll(".picker-link")).toHaveLength(1);
  });

  test("descending and the breadcrumbs walk in both directions", async () => {
    const { gateway, listed } = stub();
    const { container } = mount(gateway);
    await settle();

    const crates = [...container.querySelectorAll<HTMLButtonElement>(".picker-dir")].find((b) =>
      b.textContent?.includes("crates/"),
    );
    crates!.click();
    await settle();
    expect(container.querySelector(".picker-chosen")?.textContent).toBe("/home/me/repo/crates");

    // The crumb for `me`, two levels up in one click.
    const crumbs = [...container.querySelectorAll<HTMLButtonElement>(".picker-crumb")];
    crumbs.find((c) => c.textContent === "me")!.click();
    await settle();
    expect(container.querySelector(".picker-chosen")?.textContent).toBe("/home/me");
    expect(listed).toEqual(["/home/me/repo", "/home/me/repo/crates", "/home/me"]);
  });

  test("an empty listing says what it cannot know", async () => {
    // `fs/list` answers the same empty page for an empty directory, a file and
    // an unreadable directory; only `fs/exists` separates the missing case.
    const { gateway } = stub();
    const { container } = mount(gateway);
    await settle();
    [...container.querySelectorAll<HTMLButtonElement>(".picker-dir")]
      .find((b) => b.textContent?.includes("crates/"))!
      .click();
    await settle();
    expect(container.querySelector(".picker-note")?.textContent).toContain("not readable");
  });

  test("a slow answer for a directory already left behind does not repaint", async () => {
    // Directories can be clicked faster than the leader answers. Without a
    // ticket the stale reply would draw over the newer one, and the path in the
    // footer would then disagree with the list above it — which is how a person
    // opens a session in the wrong root.
    const delays: Record<string, number> = { "/home/me/repo/crates": 80, "/home/me": 0 };
    const { gateway } = stub({
      listDirectory: async (path: string): Promise<FsListResponse> => {
        await settle(delays[path] ?? 0);
        return { nodes: TREE[path] ?? [], truncated: false };
      },
    });
    const { container } = mount(gateway);
    await settle();

    [...container.querySelectorAll<HTMLButtonElement>(".picker-dir")]
      .find((b) => b.textContent?.includes("crates/"))!
      .click();
    [...container.querySelectorAll<HTMLButtonElement>(".picker-crumb")]
      .find((c) => c.textContent === "me")!
      .click();
    await settle(150);

    expect(container.querySelector(".picker-chosen")?.textContent).toBe("/home/me");
    const names = [...container.querySelectorAll(".picker-dir .picker-name")].map(
      (n) => n.textContent,
    );
    expect(names).toEqual(["repo/"]);
  });

  test("the chosen root is handed back exactly as shown", async () => {
    const chosen: string[] = [];
    const { gateway } = stub();
    const { container } = mount(gateway, (cwd) => chosen.push(cwd));
    await settle();
    container.querySelector<HTMLButtonElement>(".picker-open")!.click();
    expect(chosen).toEqual(["/home/me/repo"]);
  });

  test("a relative path cannot be submitted, because the agent would refuse it", async () => {
    const { gateway } = stub();
    const { container } = mount(gateway);
    await settle();
    const input = container.querySelector<HTMLInputElement>(".picker-path")!;
    input.value = "repo/crates";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await settle();
    expect(container.querySelector<HTMLButtonElement>(".picker-go")!.disabled).toBe(true);
  });
});
