import { describe, expect, test } from "bun:test";

import { readRange } from "../src/toolcall.ts";
import { readResult, typedResult } from "../src/toolresult.ts";
import type { ToolCallFacts } from "../src/toolcall.ts";

const CRATES = new URL("../../../crates/codegen/", import.meta.url);
const rust = (path: string): Promise<string> => Bun.file(new URL(path, CRATES)).text();

const TRACKER = await rust("xai-grok-pager/src/acp/tracker.rs");
const READ_TOOL = await rust(
  "../../crates/codegen/xai-grok-tools/src/implementations/grok_build/read_file/mod.rs",
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
