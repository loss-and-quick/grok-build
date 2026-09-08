import { describe, expect, test } from "bun:test";

import {
  EXECUTE_FIRST_LINES,
  EXECUTE_LAST_LINES,
  EXECUTE_TOOL_NAMES,
  READ_FIRST_LINES,
  READ_LAST_LINES,
  ellipsisFor,
  failureText,
  hasFailed,
  matchSummary,
  nextMode,
  relativeTo,
  toolTitle,
  truncate,
  type ToolCallFacts,
} from "../src/toolcall.ts";

const CRATES = new URL("../../../crates/codegen/", import.meta.url);
const rust = (path: string): Promise<string> => Bun.file(new URL(path, CRATES)).text();

const READ = await rust("xai-grok-pager/src/scrollback/blocks/tool/read.rs");
const SEARCH = await rust("xai-grok-pager/src/scrollback/blocks/tool/search.rs");
const EXECUTE = await rust("xai-grok-pager/src/scrollback/blocks/tool/execute.rs");
const TRACKER = await rust("xai-grok-pager/src/acp/tracker.rs");
const APPEARANCE = await rust("xai-grok-pager-render/src/appearance/config.rs");

/** A call with nothing filled in, so each test states only what it is about. */
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

const flatten = (facts: ToolCallFacts): string => {
  const title = toolTitle(facts);
  return (title.verb ?? "") + title.parts.map((part) => part.text).join("");
};

describe("the numbers are the pager's, and stay the pager's", () => {
  // Same standing as `glyphs.test.ts`: these are Rust `const`s with no
  // generated artifact, so a test reading the file is the only thing that
  // notices when somebody retunes one.
  test("a read fold keeps the head and tail `read.rs` keeps", () => {
    expect(READ).toContain(`const FIRST_LINES: usize = ${READ_FIRST_LINES};`);
    expect(READ).toContain(`const LAST_LINES: usize = ${READ_LAST_LINES};`);
  });

  test("an execute fold keeps the head and tail the appearance config keeps", () => {
    const execute = APPEARANCE.slice(APPEARANCE.indexOf("impl Default for ExecuteConfig"));
    expect(execute).toContain(`first_lines: ${EXECUTE_FIRST_LINES},`);
    expect(execute).toContain(`last_lines: ${EXECUTE_LAST_LINES},`);
  });

  test("the execute titles refused here are the ones the tracker refuses", () => {
    const guard = TRACKER.slice(
      TRACKER.indexOf("fn is_execute_tool_function_name"),
      TRACKER.indexOf("fn raw_input_command"),
    );
    for (const name of EXECUTE_TOOL_NAMES) expect(guard).toContain(`"${name}"`);
  });

  test("the ellipsis lines are the two the pager writes", () => {
    // `… +N lines` counts what it hid and `…` does not; the difference is the
    // pager's, and reproducing one of the two would be inventing a third.
    expect(EXECUTE).toContain('format!("\\u{2026} +{hidden} lines")');
    expect(READ).toContain('Span::styled("\\u{2026}", theme.muted())');
    expect(ellipsisFor("execute", 47)).toBe("… +47 lines");
    expect(ellipsisFor("read", 47)).toBe("…");
  });

  test("the match summaries are the ones `search.rs` documents", () => {
    // The doc comment above `match_summary` lists the exact strings, so the
    // comparison is against the pager's own statement of them.
    const doc = SEARCH.slice(SEARCH.indexOf("/// Build the match summary"));
    expect(doc).toContain("`(3 matches in 2 files)` / `(1 match)` / `(no matches)`");
    expect(matchSummary(3, 2, "content")).toBe("(3 matches in 2 files)");
    expect(matchSummary(1, 1, "content")).toBe("(1 match)");
    expect(matchSummary(0, 0, "content")).toBe("(no matches)");
    expect(matchSummary(3, 3, "files_with_matches")).toBe("(3 files)");
    expect(matchSummary(0, 0, "files_with_matches")).toBe("(no files)");
    expect(matchSummary(42, 5, "count")).toBe("(42 matches across 5 files)");
  });
});

describe("a title is derived from the arguments, not from `title`", () => {
  test("a search says it is a search, and quotes the pattern", () => {
    // The defect this stands against, verbatim from a screenshot of the running
    // client: the pattern alone was the entire heading.
    const pattern = String.raw`subagent_type|slowpoke|task tool|agent\(`;
    expect(flatten(call({ kind: "search", title: pattern, rawInput: { pattern } }))).toBe(
      `Search ${JSON.stringify(pattern)}`,
    );
  });

  test("a glob replaces a trivial pattern instead of being a second scope", () => {
    expect(
      flatten(call({ kind: "search", rawInput: { pattern: ".", glob: "*.rs", path: "src" } })),
    ).toBe("Search *.rs in src");
    expect(
      flatten(call({ kind: "search", rawInput: { pattern: "todo", glob: "*.rs", path: "src" } })),
    ).toBe('Search "todo" in *.rs in src');
  });

  test("a search counts its hits from the typed output the wire carries", () => {
    const facts = call({
      kind: "search",
      rawInput: { pattern: "todo" },
      rawOutput: {
        type: "GrepSearch",
        match_count: 3,
        file_matches: [{ path: "a" }, { path: "b" }],
      },
    });
    expect(flatten(facts)).toBe('Search "todo" (3 matches in 2 files)');
  });

  test("a shell call is `Run cmd` and never the tool's own function name", () => {
    expect(flatten(call({ kind: "execute", title: "run_terminal_command" }))).toBe("Run …");
    expect(
      flatten(call({ kind: "execute", title: "bash", rawInput: { command: "cargo build" } })),
    ).toBe("Run cargo build");
  });

  test("a redundant `cd` into the session's own root is peeled off", () => {
    expect(
      flatten(
        call({ kind: "execute", rawInput: { command: "cd /home/dev/project && cargo test" } }),
      ),
    ).toBe("Run cargo test");
  });

  test("a description takes the header and the command drops below it", () => {
    const title = toolTitle(
      call({
        kind: "execute",
        rawInput: { command: "cargo test --all", description: "Running the test suite" },
      }),
    );
    // `strip_leading_run_word`, so "Run Running the test suite" cannot happen.
    expect(title.verb).toBe("Run ");
    expect(title.parts[0]!.text).toBe("the test suite");
    expect(title.secondary).toBe("cargo test --all");
    expect(EXECUTE).toContain("do not read `Run Run the tests`");
    // Collapsed hides the command line, which is why the fold has to.
    expect(EXECUTE).toContain(
      "Collapsed mode passes `include_command = false` so only the description title is shown",
    );
  });

  test("a read shows the path against the session's root, and its line range", () => {
    expect(
      flatten(
        call({
          kind: "read",
          rawInput: { file_path: "/home/dev/project/src/main.rs" },
          rawOutput: {
            type: "ReadFile",
            FileContent: { offset: 0, limit: 50, total_lines: 200 },
          },
        }),
      ),
    ).toBe("Read src/main.rs (1-50 of 200)");
  });

  test("a whole-file read is not labelled with a total it already is", () => {
    expect(
      flatten(
        call({
          kind: "read",
          rawInput: { file_path: "/home/dev/project/a.txt" },
          rawOutput: { type: "ReadFile", FileContent: { offset: 0, limit: 10, total_lines: 10 } },
        }),
      ),
    ).toBe("Read a.txt (1-10)");
  });

  test("reading a SKILL.md is a skill, the way the pager titles it", () => {
    expect(
      flatten(call({ kind: "read", rawInput: { file_path: "/home/dev/.grok/skills/deploy/SKILL.md" } })),
    ).toBe("Skill deploy");
  });

  test("a write is `Creating` and an edit is `Edit`", () => {
    expect(
      flatten(call({ kind: "edit", rawInput: { file_path: "src/a.rs", variant: "Write" } })),
    ).toBe("Creating src/a.rs");
    expect(flatten(call({ kind: "edit", rawInput: { file_path: "src/a.rs" } }))).toBe("Edit src/a.rs");
  });

  test("a fetch names the url, from the input or from the prefixed title", () => {
    expect(flatten(call({ kind: "fetch", title: "Fetch: https://example.com" }))).toBe(
      "Fetch https://example.com",
    );
    expect(flatten(call({ kind: "fetch", rawInput: { url: "https://x.ai" } }))).toBe(
      "Fetch https://x.ai",
    );
  });

  test("a directory listing is reached by its input field, not by its kind", () => {
    // The pager's own route: `_ if extract_raw_field(tc, "target_directory")`
    // comes before any remaining kind arm.
    expect(TRACKER).toContain('_ if extract_raw_field(tc, "target_directory").is_some()');
    expect(
      flatten(call({ kind: "other", rawInput: { target_directory: "/home/dev/project/src" } })),
    ).toBe("List src");
  });

  test("a think has no arm of its own and is titled like any other tool", () => {
    // Asserting the absence, because that absence is the rule: adding a
    // `ToolKind::Think` arm to the pager would make this client wrong.
    expect(TRACKER).not.toContain("acp::ToolKind::Think =>");
    expect(flatten(call({ kind: "think", title: "Considering the options" }))).toBe(
      "Considering the options",
    );
  });

  test("a name with a colon splits into a label and its content", () => {
    const title = toolTitle(call({ kind: "other", title: "Ask: What shall I do?" }));
    expect(title.verb).toBe("Ask: ");
    expect(title.parts[0]!.text).toBe("What shall I do?");
  });
});

describe("failure, folding and paths", () => {
  test("a failed call says what failed when the tool said nothing", () => {
    expect(failureText(call({ kind: "read", status: "failed" }), "")).toBe("Read failed");
    expect(failureText(call({ kind: "search", status: "failed" }), "")).toBe("Search failed");
    expect(failureText(call({ kind: "fetch", status: "failed" }), "")).toBe("Fetch failed");
    expect(failureText(call({ kind: "other", status: "failed" }), "")).toBe("Failed");
  });

  test("a non-zero exit is a failure however the status field was filled in", () => {
    // Watched live: `exit 3` arrives as `status: "completed"` with
    // `exit_code: 3`. Reading the status alone paints a failed command green.
    expect(TRACKER).toContain("if !success || bash.exit_code != 0 {");
    const facts = call({
      kind: "execute",
      status: "completed",
      rawOutput: { type: "Bash", exit_code: 3, signal: null },
    });
    expect(hasFailed(facts)).toBe(true);
    expect(failureText(facts, "")).toBe("exit code 3");
    expect(hasFailed({ ...facts, rawOutput: { type: "Bash", exit_code: 0, signal: null } })).toBe(
      false,
    );
    expect(hasFailed(call({ kind: "read", status: "failed" }))).toBe(true);
    expect(hasFailed(call({ kind: "read", status: "completed" }))).toBe(false);
  });

  test("a shell failure carries the exit code the typed output already knows", () => {
    const facts = call({
      kind: "execute",
      status: "failed",
      rawOutput: { type: "Bash", exit_code: 101, signal: null },
    });
    expect(failureText(facts, "")).toBe("exit code 101");
    expect(failureText({ ...facts, rawOutput: { type: "Bash", exit_code: 0, signal: "SIGKILL" } }, "")).toBe(
      "SIGKILL",
    );
  });

  test("what the tool actually said always wins over the fallback phrase", () => {
    expect(failureText(call({ kind: "read", status: "failed" }), "no such file")).toBe(
      "no such file",
    );
  });

  test("output opens closed, and reopens closed", () => {
    // Collapsed is the pager's default for an agent tool call, which is the
    // whole reason a raw JSON result never filled its transcript.
    expect(READ).toContain("fn default_display_mode(&self) -> DisplayMode {");
    expect(nextMode("read", "collapsed")).toBe("truncated");
    expect(nextMode("read", "truncated")).toBe("collapsed");
    expect(nextMode("search", "collapsed")).toBe("expanded");
    expect(nextMode("search", "expanded")).toBe("collapsed");
  });

  test("only the kinds with head-and-tail constants truncate", () => {
    const lines = Array.from({ length: 20 }, (_, at) => `line ${at}`);
    const read = truncate(lines, "read");
    expect(read.head).toHaveLength(READ_FIRST_LINES);
    expect(read.tail).toHaveLength(READ_LAST_LINES);
    expect(read.hidden).toBe(20 - READ_FIRST_LINES - READ_LAST_LINES);
    expect(truncate(lines, "search").hidden).toBe(0);
    expect(truncate(lines.slice(0, 4), "read").hidden).toBe(0);
  });

  test("a path is shown against the session's root and never rewritten otherwise", () => {
    expect(relativeTo("/home/dev/project", "/home/dev/project/src/a.rs")).toBe("src/a.rs");
    expect(relativeTo("/home/dev/project/", "/home/dev/project/a.rs")).toBe("a.rs");
    // A path outside the session's root keeps every character: a browser has no
    // process directory and no home to expand, so guessing would be worse than
    // the absolute truth.
    expect(relativeTo("/home/dev/project", "/etc/hosts")).toBe("/etc/hosts");
    expect(relativeTo("/home/dev/project", "relative/a.rs")).toBe("relative/a.rs");
  });
});
