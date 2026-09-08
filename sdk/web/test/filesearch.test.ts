// `@`-completion, pinned against the pager and the wire rather than against
// itself. Every expectation here names the Rust it reproduces, because "the
// browser does something reasonable" and "the browser does what the terminal
// does" are different claims and only the second one is the job.
import { describe, expect, test } from "bun:test";

import {
  FUZZY_CHANGE,
  FUZZY_CLOSE,
  FUZZY_OPEN,
  RESULT_LIMIT,
  acceptInto,
  countHint,
  createFileSearch,
  detectAt,
  drillInto,
  isDirMode,
  isHiddenMode,
  matchRuns,
  matcherQuery,
  parseFuzzyStatus,
  pathRange,
  relativeTo,
  type FuzzyMatch,
} from "../src/filesearch.ts";

const ROOT = "/home/me/repo";

function match(path: string, indices: number[] = [], kind: "file" | "directory" = "file"): FuzzyMatch {
  return { name: path.split("/").pop() ?? path, kind, path: `${ROOT}/${path}`, score: 1, indices };
}

describe("finding the @-token the caret is in", () => {
  test("a bare @ at the end of the line is a token with an empty query", () => {
    const at = detectAt("look at @", 9);
    expect(at).toEqual({ start: 8, end: 9, query: "" });
  });

  test("the rightmost @ wins, so the token follows the caret", () => {
    // `multiple_at_picks_rightmost`.
    const at = detectAt("@first @second", 14);
    expect(at?.start).toBe(7);
    expect(at?.query).toBe("second");
  });

  test("an email address is not a file picker", () => {
    // The whole reason for the guard: `@` preceded by an alphanumeric or `_`.
    expect(detectAt("mail me@example.com", 19)).toBeNull();
    expect(detectAt("write to a_@x", 13)).toBeNull();
    // A `@` after punctuation or a space still opens one.
    expect(detectAt("(@src", 5)?.query).toBe("src");
  });

  test("whitespace, comma and semicolon end the token", () => {
    for (const terminator of [" ", ",", ";", "\n"]) {
      const text = `@src${terminator}rest`;
      expect(detectAt(text, 4)?.end).toBe(4);
      // The caret past the terminator is outside the token, so there is none.
      expect(detectAt(text, 6)).toBeNull();
    }
  });

  test("the query runs to the caret, not to the end of the token", () => {
    // `cursor_mid_token`: `@foo/bar` with the caret after `foo/` asks for
    // `foo/`, even though the token continues.
    const at = detectAt("@foo/bar", 5);
    expect(at?.query).toBe("foo/");
    expect(at?.end).toBe(8);
  });

  test("a drilled directory keeps a space inside it from ending the token", () => {
    // `detect_with_drill`: this is what makes `@my dir/` reachable at all.
    // Without the anchor the space ends the token at `@my`, and the caret is
    // past it, so there is no context at all.
    expect(detectAt("@my dir/", 8)).toBeNull();
    expect(detectAt("@my dir/", 8, "my dir")).toEqual({ start: 0, end: 8, query: "my dir/" });
    // Backspaced out of the prefix, the anchor stops applying and it is a
    // token that ended three characters in.
    expect(detectAt("@mx dir/", 8, "my dir")).toBeNull();
    expect(detectAt("@mx dir/", 3, "my dir")).toEqual({ start: 0, end: 3, query: "mx" });
  });
});

describe("the two modes a query encodes", () => {
  test("a trailing slash asks for directories only", () => {
    expect(isDirMode({ start: 0, end: 4, query: "src/" })).toBe(true);
    expect(isDirMode({ start: 0, end: 4, query: "src" })).toBe(false);
  });

  test("a leading bang asks for hidden and ignored files, and is not matched on", () => {
    const at = { start: 0, end: 5, query: "!env" };
    expect(isHiddenMode(at)).toBe(true);
    // `matcher_query` strips it: the `!` is a mode, not a character to find.
    expect(matcherQuery(at)).toBe("env");
    // And the path range steps over both markers, so drilling preserves them.
    expect(pathRange(at)).toEqual({ start: 2, end: 5 });
    expect(pathRange({ start: 0, end: 4, query: "env" })).toEqual({ start: 1, end: 4 });
  });
});

describe("the path a row shows, and what the highlight is numbered against", () => {
  test("the row is the path relative to the search root", () => {
    expect(relativeTo(ROOT, `${ROOT}/src/main.rs`)).toBe("src/main.rs");
    expect(relativeTo(`${ROOT}/`, `${ROOT}/src/main.rs`)).toBe("src/main.rs");
    expect(relativeTo("/", "/etc/hosts")).toBe("etc/hosts");
    expect(relativeTo(ROOT, ROOT)).toBe("");
    // `normalize_display_path`.
    expect(relativeTo(ROOT, `${ROOT}/./src`)).toBe("src");
    // Not under the root: shown whole rather than silently shortened.
    expect(relativeTo(ROOT, "/other/file")).toBe("/other/file");
  });

  test("indices are offsets into the relative path, not the absolute one", () => {
    // The load-bearing asymmetry on the wire: the matcher scores paths with the
    // root stripped (`check_entry`), but `fuzzy_poll` rewrites `path` to
    // `root.join(path)` before sending. Applying the offsets to what arrives
    // would light up characters inside `/home/me/repo`.
    const runs = matchRuns(ROOT, match("src/main.rs", [0, 1, 2]));
    expect(runs).toEqual([
      { text: "src", match: true },
      { text: "/main.rs", match: false },
    ]);
  });

  test("indices count characters, so a non-ASCII path stays aligned", () => {
    // `FuzzyMatchResult.indices` are "matched indices of characters" into a
    // `Utf32String`; a UTF-16 walk would slide by one per astral character.
    const runs = matchRuns(ROOT, match("\u{1F600}/ab", [2]));
    expect(runs.map((run) => run.text).join("")).toBe("\u{1F600}/ab");
    expect(runs.filter((run) => run.match).map((run) => run.text)).toEqual(["a"]);
  });

  test("an empty query carries no indices, so nothing is highlighted", () => {
    // Browsing is a depth-1 walk with `score: 0, indices: []` — the pager shows
    // those rows unmarked, because there was no query to mark.
    expect(matchRuns(ROOT, match("src"))).toEqual([{ text: "src", match: false }]);
  });
});

describe("the count in the corner", () => {
  test("shown over indexed, and a cap that says it capped", () => {
    // The pager writes `k/n`, and `1k+/n` once its own top-k bites.
    expect(countHint(7, 5312)).toBe("7/5312");
    expect(countHint(RESULT_LIMIT, 5312)).toBe(`${RESULT_LIMIT}+/5312`);
  });
});

describe("taking a row into the composer", () => {
  test("a file replaces the whole token with the path and a space", () => {
    // `accept_file_search_result_inner`: the element text is `@{path}` and a
    // space follows it.
    const result = acceptInto("see @mai", { start: 4, end: 8, query: "mai" }, match("src/main.rs"), ROOT);
    expect(result.text).toBe("see @src/main.rs ");
    expect(result.caret).toBe(result.text.length);
    expect(result.keepOpen).toBe(false);
  });

  test("a file accepted in hidden mode loses the bang, because the pager's element text has none", () => {
    const result = acceptInto("@!env", { start: 0, end: 5, query: "!env" }, match(".env"), ROOT);
    expect(result.text).toBe("@.env ");
  });

  test("a directory in dir mode drills in and keeps the list up", () => {
    const at = { start: 0, end: 1, query: "" };
    const dir = match("src", [], "directory");
    // Not dir mode: it is taken as a plain path.
    expect(acceptInto("@", at, dir, ROOT).text).toBe("@src ");
    // Dir mode: `try_replace` appends the slash and stays open.
    const drilling = acceptInto("@s/", { start: 0, end: 3, query: "s/" }, dir, ROOT);
    expect(drilling.text).toBe("@src/");
    expect(drilling.keepOpen).toBe(true);
    expect(drilling.drill).toBe("src");
  });

  test("re-confirming a directory already typed commits it", () => {
    // `no_op` in `try_replace`: the `/`-append matches what is there, so the
    // choice commits, takes a trailing space at the end of the line, and closes.
    const dir = match("src", [], "directory");
    const settled = acceptInto("@src/", { start: 0, end: 5, query: "src/" }, dir, ROOT);
    expect(settled.text).toBe("@src/ ");
    expect(settled.keepOpen).toBe(false);
    // Not at the end of the line, the terminator already there is stepped over
    // rather than doubled.
    const inline = acceptInto("@src/ tail", { start: 0, end: 5, query: "src/" }, dir, ROOT);
    expect(inline.text).toBe("@src/ tail");
    expect(inline.caret).toBe(6);
  });

  test("the hidden-mode bang survives a directory drill", () => {
    // `path_range` exists for exactly this: the replacement starts after `!`.
    const dir = match("target", [], "directory");
    const result = acceptInto("@!t/", { start: 0, end: 4, query: "!t/" }, dir, ROOT);
    expect(result.text).toBe("@!target/");
  });

  test("the right arrow steps into a directory without the slash, and drops dir mode", () => {
    // `accept_file_search_result_no_space`: the missing `/` is the point — the
    // next list shows files as well as directories.
    const dir = match("src", [], "directory");
    const stepped = drillInto("@s", { start: 0, end: 2, query: "s" }, dir, ROOT);
    expect(stepped.text).toBe("@src");
    expect(stepped.keepOpen).toBe(true);
    expect(stepped.drill).toBe("src");
    // On a file it is Tab: there is nothing under a file to step into.
    expect(drillInto("@m", { start: 0, end: 2, query: "m" }, match("main.rs"), ROOT).text).toBe(
      "@main.rs ",
    );
  });
});

describe("reading a status notification", () => {
  test("the five fields the Serialize impl writes", () => {
    const status = parseFuzzyStatus({
      sessionId: "s1",
      searchId: "abc",
      matches: [{ name: "main.rs", type: "file", path: "/r/src/main.rs", score: 9, indices: [0, 1] }],
      total: 12,
      done: true,
      generation: 3,
    });
    expect(status).toEqual({
      searchId: "abc",
      matches: [
        { name: "main.rs", kind: "file", path: "/r/src/main.rs", score: 9, indices: [0, 1] },
      ],
      total: 12,
      done: true,
      generation: 3,
    });
  });

  test("`type` is the wire's word for what kind of node it is", () => {
    const status = parseFuzzyStatus({
      searchId: "a",
      matches: [
        { name: "src", type: "directory", path: "/r/src", score: 0, indices: [] },
        { name: "x", type: "file", path: "/r/x", score: 0, indices: [] },
      ],
      total: 2,
      done: true,
      generation: 1,
    });
    expect(status?.matches.map((m) => m.kind)).toEqual(["directory", "file"]);
  });

  test("anything that is not a status is refused rather than half-read", () => {
    expect(parseFuzzyStatus(null)).toBeNull();
    expect(parseFuzzyStatus("nope")).toBeNull();
    expect(parseFuzzyStatus({ matches: [] })).toBeNull();
    // A row without a path is not a row; the rest of the batch still lands.
    const status = parseFuzzyStatus({ searchId: "a", matches: [{}, { path: "/r/x" }] });
    expect(status?.matches.map((m) => m.path)).toEqual(["/r/x"]);
  });
});

interface Call {
  method: string;
  params: Record<string, unknown>;
}

function harness(session: { sessionId: string; cwd: string } | null = { sessionId: "s1", cwd: ROOT }) {
  const calls: Call[] = [];
  const said: string[] = [];
  let current = session;
  let ids = 0;
  const search = createFileSearch({
    ext: async (method, params) => {
      calls.push({ method, params: params as Record<string, unknown> });
      if (method === FUZZY_OPEN) return { searchId: `search-${(ids += 1)}` };
      return {};
    },
    session: () => current,
    say: (text) => said.push(text),
  });
  return {
    search,
    calls,
    said,
    setSession: (next: typeof current) => {
      current = next;
    },
  };
}

/** Let the search's own promise chain run to a stop. */
async function settle(): Promise<void> {
  for (let turn = 0; turn < 8; turn += 1) await Promise.resolve();
}

describe("who owns the search", () => {
  test("the first query opens, and every one after it only changes", async () => {
    // `open` builds a nucleo matcher and starts an `ignore` walk of the tree, so
    // opening one per keystroke — or per opening of the menu — would re-index
    // the repository each time. The pager holds one daemon for the process.
    const { search, calls } = harness();
    search.ask({ query: "", dirsOnly: false, hidden: false });
    await settle();
    search.ask({ query: "mai", dirsOnly: false, hidden: false });
    await settle();
    expect(calls.map((call) => call.method)).toEqual([FUZZY_OPEN, FUZZY_CHANGE, FUZZY_CHANGE]);
    expect(calls[0]!.params).toEqual({ sessionId: "s1", cwd: ROOT, hidden: false });
    expect(calls[2]!.params).toEqual({
      searchId: "search-1",
      query: "mai",
      dirsOnly: false,
      limit: RESULT_LIMIT,
    });
  });

  test("an empty query is still sent, because `open` returns no results", async () => {
    // `FuzzyOpenReq::execute` never spawns the status driver; only
    // `FuzzyChangeReq::execute` does. A client that opened and waited would wait
    // for ever.
    const { search, calls } = harness();
    search.ask({ query: "", dirsOnly: false, hidden: false });
    await settle();
    expect(calls.at(-1)).toEqual({
      method: FUZZY_CHANGE,
      params: { searchId: "search-1", query: "", dirsOnly: false, limit: RESULT_LIMIT },
    });
  });

  test("a query typed while one is in flight replaces it instead of queueing", async () => {
    const { search, calls } = harness();
    search.ask({ query: "a", dirsOnly: false, hidden: false });
    search.ask({ query: "ab", dirsOnly: false, hidden: false });
    search.ask({ query: "abc", dirsOnly: false, hidden: false });
    await settle();
    const queries = calls
      .filter((call) => call.method === FUZZY_CHANGE)
      .map((call) => call.params["query"]);
    expect(queries).toEqual(["abc"]);
  });

  test("hidden mode is a property of the search, so the bang swaps it", async () => {
    // `FuzzySearchContext.hidden` is fixed at open; there is no per-query flag
    // for it on the wire.
    const { search, calls } = harness();
    search.ask({ query: "e", dirsOnly: false, hidden: false });
    await settle();
    search.ask({ query: "e", dirsOnly: false, hidden: true });
    await settle();
    expect(calls.map((call) => call.method)).toEqual([
      FUZZY_OPEN,
      FUZZY_CHANGE,
      FUZZY_CLOSE,
      FUZZY_OPEN,
      FUZZY_CHANGE,
    ]);
    expect(calls[3]!.params["hidden"]).toBe(true);
  });

  test("releasing hands the matcher back; forgetting does not pretend to", async () => {
    // The agent has no hook on a client going away — `cleanup_stale` runs only
    // inside the next `open` — so `close` is the one chance to free it, and a
    // dead socket has no chance at all.
    const { search, calls } = harness();
    search.ask({ query: "", dirsOnly: false, hidden: false });
    await settle();
    search.release();
    await settle();
    expect(calls.at(-1)).toEqual({ method: FUZZY_CLOSE, params: { searchId: "search-1" } });

    search.ask({ query: "", dirsOnly: false, hidden: false });
    await settle();
    search.forget();
    await settle();
    expect(calls.at(-1)!.method).toBe(FUZZY_CHANGE);
    // And the next query opens afresh rather than addressing the dead id.
    search.ask({ query: "", dirsOnly: false, hidden: false });
    await settle();
    expect(calls.at(-2)!.method).toBe(FUZZY_OPEN);
  });

  test("batches under another search id are somebody else's", async () => {
    // The leader routes this notification by the session id in its own params,
    // so every client subscribed to the session — a second tab, a terminal —
    // receives it.
    const { search } = harness();
    search.ask({ query: "", dirsOnly: false, hidden: false });
    await settle();
    search.apply({ searchId: "someone-else", matches: [{ path: "/r/x" }], total: 1, generation: 1 });
    expect(search.matches()).toEqual([]);
    search.apply({ searchId: "search-1", matches: [{ path: "/r/x" }], total: 1, generation: 1 });
    expect(search.matches().map((m) => m.path)).toEqual(["/r/x"]);
  });

  test("a late batch from a superseded query does not overwrite a newer one", async () => {
    // `generation` is monotonic across queries, and a driver that has been
    // superseded keeps polling until its next tick notices.
    const { search } = harness();
    search.ask({ query: "", dirsOnly: false, hidden: false });
    await settle();
    search.apply({ searchId: "search-1", matches: [{ path: "/r/new" }], total: 1, generation: 9 });
    search.apply({ searchId: "search-1", matches: [{ path: "/r/old" }], total: 1, generation: 4 });
    expect(search.matches().map((m) => m.path)).toEqual(["/r/new"]);
  });

  test("with nothing attached there is nothing to search", async () => {
    const { search, calls } = harness(null);
    search.ask({ query: "x", dirsOnly: false, hidden: false });
    await settle();
    expect(calls).toEqual([]);
    expect(search.root()).toBe("");
  });

  test("a refused open is reported once and does not wedge the queue", async () => {
    const said: string[] = [];
    const search = createFileSearch({
      ext: async () => {
        throw new Error("no");
      },
      session: () => ({ sessionId: "s1", cwd: ROOT }),
      say: (text) => said.push(text),
    });
    search.ask({ query: "x", dirsOnly: false, hidden: false });
    await settle();
    expect(said).toEqual(["file search failed: Error: no"]);
    search.ask({ query: "y", dirsOnly: false, hidden: false });
    await settle();
    expect(said.length).toBe(2);
  });
});
