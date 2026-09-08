import { describe, expect, test } from "bun:test";

import { readRange } from "../src/toolcall.ts";
import {
  TYPED_LINE_LIMIT,
  filePathsFromStdout,
  readResult,
  resultPath,
  searchResult,
  typedResult,
} from "../src/toolresult.ts";
import type { ToolCallFacts } from "../src/toolcall.ts";

const CRATES = new URL("../../../crates/codegen/", import.meta.url);
const rust = (path: string): Promise<string> => Bun.file(new URL(path, CRATES)).text();

const TRACKER = await rust("xai-grok-pager/src/acp/tracker.rs");
const SEARCH = await rust("xai-grok-pager/src/scrollback/blocks/tool/search.rs");
const READ_TOOL = await rust(
  "../../crates/codegen/xai-grok-tools/src/implementations/grok_build/read_file/mod.rs",
);
const GREP_TOOL = await rust(
  "../../crates/codegen/xai-grok-tools/src/implementations/grok_build/grep/mod.rs",
);

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

describe("the constants this module borrows", () => {
  test("the row budget is the grep tool's own default, not a number invented here", () => {
    // The cap is this client's — the pager caps neither hits nor diff rows —
    // but the *value* is the one line budget the product already states for a
    // tool result. If that default moves, this fails rather than drifting.
    expect(GREP_TOOL).toContain(`const CONTENT_LINE_DEFAULT: usize = ${TYPED_LINE_LIMIT};`);
  });
});

describe("a read's line numbers", () => {
  test("`offset` is one-based, which is why this client does not add one to it", () => {
    // The tool's own resolution: zero folds to line 1, anything positive is
    // already the start line. The pager's `off + 1` is a line too far, and
    // this is the assertion that says the divergence is deliberate.
    expect(READ_TOOL).toContain(
      "Harness-compatible negative offset resolution (1-indexed start line)",
    );
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

describe("choosing a body", () => {
  test("a tool with no typed result falls back to its text, as it always did", () => {
    expect(
      typedResult(call({ kind: "execute", rawOutput: { type: "Bash", exit_code: 0 } })),
    ).toBeNull();
    expect(typedResult(call())).toBeNull();
    expect(
      typedResult(call({ rawOutput: { type: "ListDir", Content: { content: "a\nb" } } })),
    ).toBeNull();
  });
});
