import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";
import { createSignal, Show } from "solid-js";

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

/**
 * `role="dialog" aria-modal="true"` is a promise about focus, and this card
 * made it without keeping it: Tab walked out into the sidebar behind, which a
 * screen reader had just been told was not there. A claimed modal that is not
 * one is worse than no claim, because the claim is what a person navigates by.
 */
describe("the dialog holds the focus it claims", () => {
  function press(key: string, shift = false): void {
    document.dispatchEvent(new KeyboardEvent("keydown", { key, shiftKey: shift, bubbles: true }));
  }

  function focusables(container: Element): HTMLElement[] {
    return [
      ...container.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    ];
  }

  test("focus starts inside it, in the field that does the work", async () => {
    const { gateway } = stub();
    const { container } = mount(gateway);
    await settle();
    expect(document.activeElement).toBe(container.querySelector(".picker-path"));
  });

  test("Tab off the end comes back to the beginning instead of leaving", async () => {
    const { gateway } = stub();
    const { container } = mount(gateway);
    await settle();
    const items = focusables(container.querySelector(".picker")!);
    items[items.length - 1]!.focus();
    press("Tab");
    expect(document.activeElement).toBe(items[0]!);
  });

  test("Shift+Tab off the front wraps to the end", async () => {
    const { gateway } = stub();
    const { container } = mount(gateway);
    await settle();
    const items = focusables(container.querySelector(".picker")!);
    items[0]!.focus();
    press("Tab", true);
    expect(document.activeElement).toBe(items[items.length - 1]!);
  });

  test("focus that is already outside is brought back in", async () => {
    // A click on the page behind, or a browser that parked focus on the
    // address bar and handed it back to the body.
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    const { gateway } = stub();
    const { container } = mount(gateway);
    await settle();
    outside.focus();
    press("Tab");
    expect(container.querySelector(".picker")!.contains(document.activeElement)).toBe(true);
    outside.remove();
  });

  test("closing gives focus back to whatever opened it", async () => {
    // Not a nicety: without it a keyboard user lands at the top of the
    // document with nothing to say why, and has to walk back down to the
    // button they just pressed.
    const trigger = document.createElement("button");
    document.body.appendChild(trigger);
    trigger.focus();

    const { gateway } = stub();
    const [open, setOpen] = createSignal(true);
    const { container } = render(() => (
      <Show when={open()}>
        <DirectoryPicker gateway={gateway} onOpen={() => {}} onClose={() => setOpen(false)} />
      </Show>
    ));
    await settle();
    expect(document.activeElement).not.toBe(trigger);

    press("Escape");
    await settle();
    expect(container.querySelector(".picker")).toBeNull();
    expect(document.activeElement).toBe(trigger);
    trigger.remove();
  });
});
