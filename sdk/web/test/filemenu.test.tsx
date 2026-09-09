// The `@`-menu as it is actually driven: a real `Session` over a stub gateway
// whose file search is the real one with a fake socket under it, so the keys
// land on the textarea the way they do in a browser and the rows arrive the way
// the agent sends them.
import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";

import { Session } from "../src/components/Session.tsx";
import { FUZZY_OPEN, createFileSearch } from "../src/filesearch.ts";
import type { Gateway } from "../src/gateway.ts";
import { createQueue } from "../src/queue.ts";
import { createSubagents } from "../src/subagents.ts";
import { createTranscript } from "../src/transcript.ts";
import type { RosterEntry } from "../src/wire.ts";

const CWD = "/home/me/repo";

const ENTRY: RosterEntry = {
  sessionId: "s1",
  cwd: CWD,
  isWorktree: false,
  yolo: false,
  activity: "idle",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

const SEARCH_ID = "search-1";

interface Row {
  path: string;
  indices?: number[];
  directory?: boolean;
}

/** A batch as the agent serializes one: absolute paths, `type`, `indices`. */
function batch(rows: Row[], total = rows.length, generation = 1) {
  return {
    sessionId: ENTRY.sessionId,
    searchId: SEARCH_ID,
    matches: rows.map((row) => ({
      name: row.path.split("/").pop(),
      type: row.directory ? "directory" : "file",
      path: `${CWD}/${row.path}`,
      score: 100,
      indices: row.indices ?? [],
    })),
    total,
    done: true,
    generation,
  };
}

function mount() {
  const asked: { query: string; dirsOnly: boolean; hidden: boolean }[] = [];
  const fileSearch = createFileSearch({
    ext: async (method, params) => {
      if (method === FUZZY_OPEN) return { searchId: SEARCH_ID };
      const body = params as { query: string; dirsOnly: boolean };
      asked.push({ query: body.query, dirsOnly: body.dirsOnly, hidden: false });
      return {};
    },
    session: () => ({ sessionId: ENTRY.sessionId, cwd: CWD }),
    say: () => {},
  });

  const gateway = {
    attached: () => ({ entry: ENTRY, transcript: createTranscript(), subagents: createSubagents(ENTRY.sessionId), queue: createQueue() }),
    permissions: [],
    folderTrusts: [],
    sessionMode: () => null,
    setSessionMode: async () => {},
    setPermissionMode: () => {},
    status: () => "connected",
    commands: () => [],
    models: () => null,
    fileSearch,
    prompt: async () => {},
    panelAction: async () => {},
  } as unknown as Gateway;

  const { container } = render(() => Session({ gateway, rail: false }));
  const input = container.querySelector<HTMLTextAreaElement>(".prompt-input")!;

  // Async because a keystroke reaches the agent: `sync` asks the search, which
  // opens on the first `@` and only then has an id to match a batch against.
  const type = async (text: string, caret = text.length): Promise<void> => {
    input.value = text;
    input.setSelectionRange(caret, caret);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await settle();
  };
  const key = (name: string, init: KeyboardEventInit = {}): void => {
    input.dispatchEvent(
      new KeyboardEvent("keydown", { key: name, bubbles: true, cancelable: true, ...init }),
    );
  };
  const rows = (): string[] =>
    [...container.querySelectorAll(".file-row .file-path")].map((n) => n.textContent ?? "");
  const marked = (at: number): string[] => {
    const row = [...container.querySelectorAll(".file-row")].at(at)!;
    return [...row.querySelectorAll(".file-match")].map((n) => n.textContent ?? "");
  };
  const selected = (): number =>
    [...container.querySelectorAll(".file-row")].findIndex((n) => n.classList.contains("selected"));

  return { container, input, type, key, rows, marked, selected, asked, fileSearch };
}

/** Let the search's promise chain reach its request. */
async function settle(): Promise<void> {
  for (let turn = 0; turn < 8; turn += 1) await Promise.resolve();
}

describe("finding a file from the browser", () => {
  test("an at-sign asks for the directory listing, and the rows are relative paths", async () => {
    const menu = mount();
    await menu.type("look at @");
    await settle();
    // An empty query browses: the agent answers with a depth-1 listing, scored
    // zero and unmarked, because there was no query to mark.
    expect(menu.asked).toEqual([{ query: "", dirsOnly: false, hidden: false }]);
    menu.fileSearch.apply(batch([{ path: "src", directory: true }, { path: "README.md" }]));
    expect(menu.rows()).toEqual(["src", "README.md"]);
  });

  test("the highlight is the agent's, on the path the agent scored", async () => {
    // The wire carries `indices` because the agent ran the matcher. This client
    // computes none — and the offsets are into the *relative* path, which is
    // what the row shows.
    const menu = mount();
    await menu.type("@mrs");
    menu.fileSearch.apply(batch([{ path: "src/main.rs", indices: [0, 4, 9, 10] }]));
    expect(menu.rows()).toEqual(["src/main.rs"]);
    expect(menu.marked(0)).toEqual(["s", "m", "rs"]);
  });

  test("no matches draws nothing at all, and leaves the typed text alone", async () => {
    // `is_visible()` is `context.is_some() && !topk.is_empty()`: the pager has
    // no "no results" row, so neither does this.
    const menu = mount();
    await menu.type("@zzzz");
    menu.fileSearch.apply(batch([]));
    expect(menu.rows()).toEqual([]);
    expect(menu.container.querySelector(".file-menu")).toBeNull();
    expect(menu.input.value).toBe("@zzzz");
  });

  test("an email address does not open a file picker", async () => {
    const menu = mount();
    await menu.type("@");
    menu.fileSearch.apply(batch([{ path: "README.md" }]));
    expect(menu.rows()).toEqual(["README.md"]);
    // The rows are still loaded; what changed is that the caret is no longer in
    // an `@`-token, because this `@` follows a letter.
    await menu.type("mail me@example.com");
    expect(menu.rows()).toEqual([]);
  });

  test("a trailing slash asks for directories only, and the rows show it", async () => {
    const menu = mount();
    await menu.type("@src/");
    expect(menu.asked.at(-1)).toEqual({ query: "src/", dirsOnly: true, hidden: false });
    menu.fileSearch.apply(batch([{ path: "src/views", directory: true }]));
    // The slash follows the query's mode, not the row's kind — the pager's own
    // condition.
    expect(menu.rows()).toEqual(["src/views/"]);
  });

  test("the arrows walk and wrap, and the page keys move half a screen", async () => {
    const menu = mount();
    await menu.type("@a");
    menu.fileSearch.apply(
      batch(Array.from({ length: 12 }, (_, at) => ({ path: `f${at}.rs` }))),
    );
    expect(menu.selected()).toBe(0);
    menu.key("ArrowDown");
    expect(menu.selected()).toBe(1);
    menu.key("ArrowUp");
    menu.key("ArrowUp");
    expect(menu.selected()).toBe(11);
    menu.key("ArrowDown");
    expect(menu.selected()).toBe(0);
    // Half of `MAX_DROPDOWN_ROWS`, the pager's `page_move(±1, 8)`.
    menu.key("PageDown");
    expect(menu.selected()).toBe(4);
    menu.key("PageUp");
    expect(menu.selected()).toBe(0);
    // The terminal's aliases work too.
    menu.key("n", { ctrlKey: true });
    expect(menu.selected()).toBe(1);
  });

  test("Enter takes the row as a path and a space, and closes the list", async () => {
    const menu = mount();
    await menu.type("see @mai");
    menu.fileSearch.apply(batch([{ path: "src/main.rs" }]));
    menu.key("Enter");
    expect(menu.input.value).toBe("see @src/main.rs ");
    expect(menu.rows()).toEqual([]);
  });

  test("the right arrow steps into a directory and asks again under it", async () => {
    const menu = mount();
    await menu.type("@sr");
    menu.fileSearch.apply(batch([{ path: "src", directory: true }]));
    menu.key("ArrowRight");
    await settle();
    expect(menu.input.value).toBe("@src");
    // Still open, and asking for what is inside — files as well as directories,
    // because stepping in deliberately drops the trailing slash.
    expect(menu.asked.at(-1)).toEqual({ query: "src", dirsOnly: false, hidden: false });
  });

  test("Escape closes the list and keeps what was typed", async () => {
    const menu = mount();
    await menu.type("@mai");
    menu.fileSearch.apply(batch([{ path: "src/main.rs" }]));
    expect(menu.rows()).toEqual(["src/main.rs"]);
    menu.key("Escape");
    expect(menu.rows()).toEqual([]);
    expect(menu.input.value).toBe("@mai");
    // And it comes back the moment the query moves on, rather than making the
    // rest of the line unsearchable.
    await menu.type("@main");
    expect(menu.rows()).toEqual(["src/main.rs"]);
  });

  test("the count says how many are shown out of how many are indexed", async () => {
    const menu = mount();
    await menu.type("@a");
    menu.fileSearch.apply(batch([{ path: "a.rs" }, { path: "b.rs" }], 5312));
    expect(menu.container.querySelector(".file-menu-hint")?.textContent).toBe("2/5312");
  });

  test("a thousand rows arrive as a thousand rows, capped for the eye, not for the list", async () => {
    // The scroll cap is CSS — eight rows of a fixed height — so the list is
    // whole and the box is eight tall, which is how the pager scrolls its own.
    const menu = mount();
    await menu.type("@r");
    menu.fileSearch.apply(
      batch(Array.from({ length: 1000 }, (_, at) => ({ path: `deep/nest/f${at}.rs` }))),
    );
    expect(menu.rows().length).toBe(1000);
    const list = menu.container.querySelector<HTMLElement>(".file-menu")!;
    expect(list.style.getPropertyValue("--file-rows")).toBe("8");
  });

  test("a path is text, not markup", async () => {
    // Paths are arbitrary bytes off the file system and reach the page as
    // content. Nothing here builds HTML from one.
    const menu = mount();
    await menu.type("@x");
    menu.fileSearch.apply(batch([{ path: "<img src=x onerror=boom>.rs", indices: [0] }]));
    const row = menu.container.querySelector(".file-row .file-path")!;
    expect(row.querySelector("img")).toBeNull();
    expect(row.textContent).toBe("<img src=x onerror=boom>.rs");
  });
});
