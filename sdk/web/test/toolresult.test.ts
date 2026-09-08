import { describe, expect, test } from "bun:test";

import { defaultMode, readRange } from "../src/toolcall.ts";
import {
  HUNK_SEPARATOR,
  MAX_CONTEXT,
  TYPED_LINE_LIMIT,
  buildDiffHunks,
  diffResult,
  filePathsFromStdout,
  gutterWidth,
  hunkGapLines,
  hunkSeparator,
  lineDiff,
  readResult,
  resultPath,
  searchResult,
  splitInclusive,
  typedResult,
} from "../src/toolresult.ts";
import type { ToolCallFacts } from "../src/toolcall.ts";

const CRATES = new URL("../../../crates/codegen/", import.meta.url);
const rust = (path: string): Promise<string> => Bun.file(new URL(path, CRATES)).text();

const DIFF = await rust("xai-grok-pager-diff/src/lib.rs");
const EDIT = await rust("xai-grok-pager/src/scrollback/blocks/tool/edit.rs");
const SEARCH = await rust("xai-grok-pager/src/scrollback/blocks/tool/search.rs");
const TRACKER = await rust("xai-grok-pager/src/acp/tracker.rs");
const APPEARANCE = await rust("xai-grok-pager-render/src/appearance/config.rs");
const CACHE = await rust("xai-grok-pager-render/src/appearance/cache.rs");
const READ_TOOL = await rust("../../crates/codegen/xai-grok-tools/src/implementations/grok_build/read_file/mod.rs");
const GREP_TOOL = await rust("../../crates/codegen/xai-grok-tools/src/implementations/grok_build/grep/mod.rs");

function call(over: Partial<ToolCallFacts> = {}): ToolCallFacts {
  return {
    title: "",
    kind: "other",
    rawInput: undefined,
    rawOutput: undefined,
    status: "completed",
    cwd: "/home/dev/project",
    ...over,
  };
}

// ---------------------------------------------------------------------------
// Constants that are Rust somewhere else
// ---------------------------------------------------------------------------

describe("the constants this module borrows", () => {
  test("three lines of context, from the hunk builder that cuts them", () => {
    expect(DIFF).toContain(`const MAX_CONTEXT: usize = ${MAX_CONTEXT};`);
  });

  test("the hunk separator is the appearance default, not a glyph chosen here", () => {
    expect(APPEARANCE).toContain(`hunk_separator: "${HUNK_SEPARATOR}"`);
    expect(EDIT).toContain(`hunk_separator: "${HUNK_SEPARATOR}"`);
  });

  test("the row budget is the grep tool's own default, not a number invented here", () => {
    // The cap is this client's — the pager caps neither hits nor diff rows —
    // but the *value* is the one line budget the product already states for a
    // tool result. If that default moves, this fails rather than drifting.
    expect(GREP_TOOL).toContain(`const CONTENT_LINE_DEFAULT: usize = ${TYPED_LINE_LIMIT};`);
  });

  test("the hunk separator counts what it skipped, in the pager's own words", () => {
    expect(EDIT).toContain('format!("{} 1 unchanged line", config.hunk_separator)');
    expect(EDIT).toContain('format!("{} {n} unchanged lines", config.hunk_separator)');
  });
});

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

describe("a read's line numbers", () => {
  test("`offset` is one-based, which is why this client does not add one to it", () => {
    // The tool's own resolution: zero folds to line 1, anything positive is
    // already the start line. The pager's `off + 1` is a line too far, and
    // this is the assertion that says the divergence is deliberate.
    expect(READ_TOOL).toContain("Harness-compatible negative offset resolution (1-indexed start line)");
    expect(READ_TOOL).toContain("if offset_raw > 0 {\n        return offset_raw as usize;");
    expect(TRACKER).toContain("let start = off + 1;");

    expect(readRange(50, 10, 801)).toEqual({ start: 50, end: 59 });
    expect(readRange(0, 50, 200)).toEqual({ start: 1, end: 50 });
    expect(readRange(null, null, 200)).toBeNull();
    // A limit past the end of the file stops at the end of the file.
    expect(readRange(795, 40, 801)).toEqual({ start: 795, end: 801 });
  });

  test("the gutter is built from the file's text, never from the model's copy", () => {
    // `content` carries a `N→` marker every tenth line; feeding it to a gutter
    // would print two numbers on those rows. The pager reads `raw_output` for
    // the same reason (`block.with_content(fc.raw_output, …)`).
    expect(TRACKER).toContain("block = block.with_content(fc.raw_output, fc.total_lines);");
    const result = readResult({
      type: "ReadFile",
      FileContent: {
        content: "50→    return needle_4 * 25\ndef fn_26(needle_5):  # line 26",
        raw_output: "    return needle_4 * 25\ndef fn_26(needle_5):  # line 26\n",
        absolute_path: "/home/dev/project/long.py",
        offset: 50,
        limit: 10,
        total_lines: 801,
      },
    });
    expect(result).not.toBeNull();
    expect(result!.base).toBe(50);
    expect(result!.lines).toEqual(["    return needle_4 * 25", "def fn_26(needle_5):  # line 26"]);
    expect(result!.total).toBe(801);
  });

  test("a trailing newline is not an extra empty row", () => {
    const result = readResult({
      type: "ReadFile",
      FileContent: { raw_output: "a\nb\n", absolute_path: "/x", total_lines: 2 },
    });
    expect(result!.lines).toEqual(["a", "b"]);
  });

  test("a failed read carries no content, so there is nothing typed to draw", () => {
    // Live shapes, both of them: a missing file and a binary one. There is no
    // binary variant — an unreadable encoding arrives as `FileReadError`.
    expect(readResult({ type: "ReadFile", FileNotFound: "Error: /x does not exist." })).toBeNull();
    expect(
      readResult({ type: "ReadFile", FileReadError: "Cannot read binary file: /x/blob.bin" }),
    ).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

describe("a search's hits", () => {
  const grep = (over: Record<string, unknown> = {}): unknown => ({
    type: "GrepSearch",
    stdout: [],
    stderr: [],
    exit_code: 0,
    match_count: 3,
    file_matches: [
      {
        path: "/home/dev/project/./src/a.rs",
        matches: [
          { line_number: 1, content: "one NEEDLE  " },
          { line_number: 42, content: "two NEEDLE" },
        ],
      },
      { path: "/home/dev/project/src/b.rs", matches: [{ line_number: 7, content: "three" }] },
    ],
    ...over,
  });

  test("hits arrive typed: a path, a line number and the line", () => {
    const result = searchResult(grep(), { pattern: "NEEDLE" });
    expect(result!.matchCount).toBe(3);
    expect(result!.files).toHaveLength(2);
    expect(result!.files[0]!.matches[0]).toEqual({ line: 1, text: "one NEEDLE" });
    expect(result!.hidden).toBe(0);
  });

  test("a path is shown against the session root, with the tool's own `/./` folded", () => {
    // A scope of `.` leaves `/./` in every path the tool reports. The pager
    // prints them absolute; this client relativises, which is what its titles
    // already do.
    expect(TRACKER).toContain("path: make_relative_path(&fm.path),");
    expect(resultPath("/home/dev/project", "/home/dev/project/./src/a.rs")).toBe("src/a.rs");
    expect(resultPath("/home/dev/project", "/elsewhere/b.rs")).toBe("/elsewhere/b.rs");
  });

  test("no hits is not a failure: ripgrep exits 1 and the call still completes", () => {
    const result = searchResult(
      { type: "GrepSearch", stdout: [], stderr: [], exit_code: 1, match_count: 0, file_matches: [] },
      { pattern: "ZZZ" },
    );
    expect(result!.matchCount).toBe(0);
    expect(result!.files).toEqual([]);
    expect(result!.paths).toEqual([]);
    // The body the pager draws for it.
    expect(SEARCH).toContain('"  (no results)"');
  });

  test("files-with-matches mode has no line numbers, so the paths come out of stdout", () => {
    const stdout =
      '<workspace_result workspace_path="/home/dev/project">\n' +
      "Found 2 files\n" +
      "/home/dev/project/src/a.rs\n" +
      "/home/dev/project/src/b.rs\n" +
      "</workspace_result>";
    expect(filePathsFromStdout(stdout)).toEqual([
      "/home/dev/project/src/a.rs",
      "/home/dev/project/src/b.rs",
    ]);
    const result = searchResult(
      {
        type: "GrepSearch",
        stdout: [...new TextEncoder().encode(stdout)],
        stderr: [],
        exit_code: 0,
        match_count: 2,
        file_matches: [],
      },
      { pattern: "NEEDLE", output_mode: "files_with_matches" },
    );
    expect(result!.paths).toHaveLength(2);
    expect(result!.meta.mode).toBe("files");
  });

  test("the metadata line reports only what was not the default", () => {
    expect(searchResult(grep(), { pattern: "x" })!.meta).toEqual({
      mode: "pattern",
      fileType: null,
      caseInsensitive: false,
      multiline: false,
    });
    expect(
      searchResult(grep(), { pattern: "x", "-i": true, type: "rust", multiline: true })!.meta,
    ).toEqual({ mode: "pattern", fileType: "rust", caseInsensitive: true, multiline: true });
  });

  test("a search past the row budget says how many hits it is not drawing", () => {
    const many = Array.from({ length: TYPED_LINE_LIMIT + 25 }, (_, at) => ({
      line_number: at + 1,
      content: `hit ${at}`,
    }));
    const result = searchResult(
      grep({ match_count: many.length, file_matches: [{ path: "/x", matches: many }] }),
      { pattern: "x" },
    );
    expect(result!.files[0]!.matches).toHaveLength(TYPED_LINE_LIMIT);
    expect(result!.hidden).toBe(25);
  });
});

// ---------------------------------------------------------------------------
// Edit
// ---------------------------------------------------------------------------

describe("splitting the way the hunk builder splits", () => {
  test("`split_inclusive` keeps the newline and yields nothing for an empty string", () => {
    expect(splitInclusive("")).toEqual([]);
    expect(splitInclusive("a")).toEqual(["a"]);
    expect(splitInclusive("a\n")).toEqual(["a\n"]);
    expect(splitInclusive("a\nb")).toEqual(["a\n", "b"]);
    expect(splitInclusive("a\n\n")).toEqual(["a\n", "\n"]);
  });
});

describe("the line correspondence the wire does not carry", () => {
  test("unchanged lines inside a replacement stay unchanged", () => {
    // The whole reason this is computed at all: without it a ten-line
    // replacement that touched one line draws twenty changed rows.
    expect(lineDiff("a\nb\nc\n", "a\nB\nc\n")).toEqual([
      { tag: "equal", text: "a\n" },
      { tag: "delete", text: "b\n" },
      { tag: "insert", text: "B\n" },
      { tag: "equal", text: "c\n" },
    ]);
  });

  test("deletes come before inserts, the order a replacement is emitted in", () => {
    expect(lineDiff("x\ny\n", "p\nq\n").map((c) => c.tag)).toEqual([
      "delete",
      "delete",
      "insert",
      "insert",
    ]);
  });

  test("an added trailing newline is a changed line, because the line carries it", () => {
    // Live: a write of "hello\nworld" over "hello\nworld\n" is one equal row and
    // one replaced row, not two equal rows.
    expect(lineDiff("hello\nworld", "hello\nworld\n")).toEqual([
      { tag: "equal", text: "hello\n" },
      { tag: "delete", text: "world" },
      { tag: "insert", text: "world\n" },
    ]);
  });

  test("an empty side is all insert or all delete", () => {
    expect(lineDiff("", "a\nb\n").map((c) => c.tag)).toEqual(["insert", "insert"]);
    expect(lineDiff("a\nb\n", "").map((c) => c.tag)).toEqual(["delete", "delete"]);
  });

  test("the script is minimal, which is what it shares with the terminal's Myers", () => {
    // Not "the same script" — two shortest scripts can differ on a repeated
    // line, and that divergence is named in `lineDiff`. What is asserted is the
    // property both algorithms guarantee: nothing that survived is marked
    // changed.
    const before = Array.from({ length: 40 }, (_, at) => `line ${at}\n`).join("");
    const after = before.replace("line 17\n", "LINE 17\n");
    const changes = lineDiff(before, after);
    expect(changes.filter((c) => c.tag !== "equal")).toEqual([
      { tag: "delete", text: "line 17\n" },
      { tag: "insert", text: "LINE 17\n" },
    ]);
  });
});

describe("hunks, from the edit details the wire already carries", () => {
  // The live shape, captured against a running gateway.
  const detail = {
    old_string: "charlie",
    old_line: 3,
    new_string: "CHARLIE-X",
    new_line: 3,
    context_before: "alpha\nbravo\n",
    context_after: "delta\necho\nfoxtrot\n",
    line_prefix: "",
  };

  test("context, the change, context — with both sides' line numbers", () => {
    const [hunk] = buildDiffHunks([detail]);
    expect(hunk!.map((line) => [line.tag, line.lo, line.ln, line.text])).toEqual([
      ["equal", 1, 1, "alpha\n"],
      ["equal", 2, 2, "bravo\n"],
      ["delete", 3, 3, "charlie"],
      ["insert", 4, 3, "CHARLIE-X"],
      ["equal", 4, 4, "delta\n"],
      ["equal", 5, 5, "echo\n"],
      ["equal", 6, 6, "foxtrot\n"],
    ]);
  });

  test("a write is one all-insert hunk, because its old side is empty", () => {
    const [hunk] = buildDiffHunks([
      {
        old_string: "",
        old_line: 1,
        new_string: "hello\nworld\n",
        new_line: 1,
        context_before: "",
        context_after: "",
        line_prefix: "",
      },
    ]);
    expect(hunk!.map((line) => [line.tag, line.ln])).toEqual([
      ["insert", 1],
      ["insert", 2],
    ]);
  });

  test("an edit that changed nothing produces no hunk at all", () => {
    expect(
      buildDiffHunks([
        {
          old_string: "same\n",
          old_line: 2,
          new_string: "same\n",
          new_line: 2,
          context_before: "a\n",
          context_after: "b\n",
          line_prefix: "",
        },
      ]),
    ).toEqual([]);
  });

  test("`line_prefix` goes on the first changed line of each side", () => {
    // `old_string` may start after the indentation the context lines carry, so
    // without this the changed rows sit a column left of everything around them.
    const [hunk] = buildDiffHunks([
      {
        old_string: "old\n",
        old_line: 2,
        new_string: "new\n",
        new_line: 2,
        context_before: "    head\n",
        context_after: "",
        line_prefix: "    ",
      },
    ]);
    expect(hunk!.map((line) => line.text)).toEqual(["    head\n", "    old\n", "    new\n"]);
  });

  test("the gutter is one column, as wide as the largest number in the hunk", () => {
    // `gutter_layout` with `dual_line_numbers: false`, its default.
    expect(APPEARANCE).toContain("dual_line_numbers: false");
    const [hunk] = buildDiffHunks([{ ...detail, old_line: 998, new_line: 998 }]);
    expect(gutterWidth(hunk!)).toBe(4);
    expect(gutterWidth(buildDiffHunks([detail])[0]!)).toBe(1);
  });

  test("the gap between two hunks is counted on the new side", () => {
    const hunks = buildDiffHunks([
      detail,
      { ...detail, old_line: 20, new_line: 20, old_string: "x", new_string: "X" },
    ]);
    expect(hunks).toHaveLength(2);
    expect(hunkGapLines(hunks[0]!, hunks[1]!)).toBe(11);
    expect(hunkSeparator(hunks[0]!, hunks[1]!)).toBe(`${HUNK_SEPARATOR} 11 unchanged lines`);
  });

  test("adjacent or out-of-order hunks keep a bare separator", () => {
    // A later edit that landed *above* an earlier one is numbered against its
    // own snapshot, so a count there would be a lie.
    const hunks = buildDiffHunks([
      { ...detail, old_line: 30, new_line: 30 },
      { ...detail, old_line: 3, new_line: 3 },
    ]);
    expect(hunkGapLines(hunks[0]!, hunks[1]!)).toBeNull();
    expect(hunkSeparator(hunks[0]!, hunks[1]!)).toBe(HUNK_SEPARATOR);
  });

  test("the diff is read off `rawOutput`, counts included", () => {
    const result = diffResult({
      type: "SearchReplace",
      EditsApplied: {
        old_string: "charlie",
        new_string: "CHARLIE-X",
        absolute_path: "/home/dev/project/edit_me.txt",
        edits: { details: [detail] },
      },
    });
    expect(result!.path).toBe("/home/dev/project/edit_me.txt");
    expect(result!.insertions).toBe(1);
    expect(result!.deletions).toBe(1);
    expect(result!.hidden).toBe(0);
  });

  test("a failed edit carries a message, not applied edits", () => {
    expect(diffResult({ type: "SearchReplace", NoMatchesFound: { message: "x" } })).toBeNull();
    expect(diffResult({ type: "SearchReplace", FileNotFound: "nope" })).toBeNull();
  });

  test("a diff past the row budget says how many rows it is not drawing", () => {
    const body = Array.from({ length: TYPED_LINE_LIMIT + 30 }, (_, at) => `line ${at}\n`).join("");
    const result = diffResult({
      type: "SearchReplace",
      EditsApplied: {
        absolute_path: "/x",
        edits: {
          details: [
            {
              old_string: "",
              old_line: 1,
              new_string: body,
              new_line: 1,
              context_before: "",
              context_after: "",
              line_prefix: "",
            },
          ],
        },
      },
    });
    expect(result!.hunks[0]).toHaveLength(TYPED_LINE_LIMIT);
    expect(result!.hidden).toBe(30);
    // The counts are of the whole edit, not of what fitted.
    expect(result!.insertions).toBe(TYPED_LINE_LIMIT + 30);
  });
});

// ---------------------------------------------------------------------------
// Which body a call gets, and how it opens
// ---------------------------------------------------------------------------

describe("choosing a body", () => {
  test("a tool with no typed result falls back to its text, as it always did", () => {
    expect(typedResult(call({ kind: "execute", rawOutput: { type: "Bash", exit_code: 0 } }))).toBeNull();
    expect(typedResult(call())).toBeNull();
    expect(typedResult(call({ rawOutput: { type: "ListDir", Content: { content: "a\nb" } } }))).toBeNull();
  });

  test("a successful edit opens showing its diff, which is the terminal's default", () => {
    // `effective_expanded` is `!collapsed_edit_blocks`, and that flag is
    // asserted OFF by default in the pager's own test.
    expect(APPEARANCE).toContain("self.expanded_by_default.unwrap_or(!collapsed_edit_blocks)");
    expect(CACHE).toContain('"collapsed_edit_blocks must default OFF (rollout flag)"');
    expect(defaultMode("edit", false)).toBe("expanded");
    expect(defaultMode("edit", true)).toBe("collapsed");
    expect(defaultMode("read", false)).toBe("collapsed");
    expect(defaultMode("search", false)).toBe("collapsed");
    expect(defaultMode("execute", false)).toBe("collapsed");
  });

  test("the collapsed diffstat is not drawn, because the terminal does not draw it either", () => {
    // `line_summary` unset follows the same flag, and the flag is off: the
    // `+N/-M` suffix exists for the one-liner shape, which is not the default.
    expect(APPEARANCE).toContain("self.line_summary.unwrap_or(collapsed_edit_blocks)");
  });
});
