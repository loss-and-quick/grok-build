// What a tool call says, decided the way the pager decides it.
//
// The ACP `title` is a fallback in the terminal, not the headline. The pager
// re-derives a title from `rawInput` for every kind it knows
// (`tool_call_to_block`, `pager/src/acp/tracker.rs`), and it goes as far as
// *rejecting* a title that is only the tool's function name so `Run
// run_terminal_command` never flashes on screen. A client that prints `title`
// verbatim therefore does not show what the terminal shows: it showed a bare
// grep pattern, `subagent_type|slowpoke|task tool|agent\(`, as an entire
// heading, with nothing on the line saying a search had happened.
//
// So the derivations live here, from the same fields, with the same
// precedence. What cannot be reproduced is named at the bottom of this file
// rather than approximated.
import type { ToolCallStatus } from "./wire.ts";

/**
 * How a fragment of a title is painted.
 *
 * Roles, never colours — the same rule the roster follows. The pager paints the
 * verb bold in `primary`, a search term in `accent_success`, a path in `path`,
 * a command with bash syntax highlighting, and every trailing count in
 * `muted`; each name below is the role that decision maps onto.
 */
export type TitleRole = "term" | "path" | "command" | "plain" | "detail";

export interface TitlePart {
  text: string;
  role: TitleRole;
}

export interface ToolTitle {
  /** The bold leading verb, or `null` for a kind the pager gives no verb. */
  verb: string | null;
  parts: TitlePart[];
  /**
   * The command, when a description took the title line.
   *
   * The pager does exactly this: with a `rawInput.description` the header
   * becomes the description and `$ command` drops to a second line
   * (`blocks/tool/execute.rs`), so the sentence the model wrote is what you
   * read first and the command is still there under it.
   */
  secondary: string | null;
}

/**
 * Function names that must never become a title.
 *
 * `is_execute_tool_function_name` (`tracker.rs`) — the ACP title for a shell
 * call is sometimes just the tool's own name, and using it produces the "Run
 * run_terminal_command" flash the pager guards against.
 */
export const EXECUTE_TOOL_NAMES = [
  "run_terminal_command",
  "run_terminal_cmd",
  "bash",
  "shell",
  "execute",
  "run_command",
  "terminal",
] as const;

/**
 * Lines kept at the head and the tail when output is shown truncated.
 *
 * The pager's constants: `FIRST_LINES`/`LAST_LINES` in `blocks/tool/read.rs`,
 * and `ExecuteConfig::first_lines`/`last_lines` in
 * `pager-render/src/appearance/config.rs`. They are pinned by
 * `test/toolcall.test.ts` against those files, for the reason `glyphs.ts` gives:
 * a Rust `const` with no generated artifact rots the moment somebody tunes it.
 */
export const READ_FIRST_LINES = 5;
export const READ_LAST_LINES = 3;
export const EXECUTE_FIRST_LINES = 2;
export const EXECUTE_LAST_LINES = 3;

/** Where a tool call's output stands. `DisplayMode` in `scrollback/types.rs`. */
export type DisplayMode = "collapsed" | "truncated" | "expanded";

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
}

/** A `rawInput`/`rawOutput` field, but only when it is a non-empty string. */
function field(from: unknown, ...names: string[]): string | null {
  const raw = record(from);
  for (const name of names) {
    const value = raw[name];
    if (typeof value === "string" && value !== "") return value;
  }
  return null;
}

/**
 * `raw_output`, decoded far enough to know which tool produced it.
 *
 * `ToolOutput` is `#[serde(tag = "type")]`
 * (`xai-grok-tools/src/types/output.rs`), so the variant is a string on the
 * same object as its fields — the one piece of that enum a client has to know.
 */
function outputOfType(rawOutput: unknown, type: string): Record<string, unknown> | null {
  const raw = record(rawOutput);
  return raw["type"] === type ? raw : null;
}

/** Everything a title is derived from. */
export interface ToolCallFacts {
  title: string;
  kind: string;
  rawInput: unknown;
  rawOutput: unknown;
  status: ToolCallStatus;
  /** The session's own root, the only thing a path can honestly be relative to. */
  cwd: string;
}

/**
 * A path shown against the session's root.
 *
 * The pager relativises against `std::env::current_dir()` and its session cwd
 * (`make_relative_path`, `render/tool_paths.rs`). A browser has no process
 * directory, but it does have the session's — roster entries carry `cwd` — and
 * that is the one the tool actually ran in.
 */
export function relativeTo(cwd: string, path: string): string {
  const root = cwd.replace(/\/+$/, "");
  if (root && path.startsWith(`${root}/`)) return path.slice(root.length + 1);
  return path;
}

/** `…/skills/deploy/SKILL.md` is a skill, and the pager titles it as one. */
function skillName(path: string): string | null {
  const match = /(?:^|\/)([^/]+)\/SKILL\.md$/.exec(path);
  return match ? match[1]! : null;
}

/**
 * The match summary the pager puts after a search.
 *
 * `SearchToolCallBlock::match_summary` (`blocks/tool/search.rs`), reading the
 * two numbers off `ToolOutput::GrepSearch`. The wire carries them, so this is
 * the pager's own count rather than one recomputed from prose.
 */
export function matchSummary(
  matches: number,
  files: number,
  mode: "content" | "files_with_matches" | "count",
): string {
  if (matches === 0) return mode === "files_with_matches" ? "(no files)" : "(no matches)";
  if (mode === "files_with_matches") return matches === 1 ? "(1 file)" : `(${matches} files)`;
  if (mode === "content") {
    if (files > 1) return `(${matches} matches in ${files} files)`;
    return matches === 1 ? "(1 match)" : `(${matches} matches)`;
  }
  if (files > 1) return `(${matches} matches across ${files} files)`;
  return matches === 1 ? "(1 match)" : `(${matches} matches)`;
}

function searchMode(rawInput: unknown): "content" | "files_with_matches" | "count" {
  const mode = field(rawInput, "output_mode");
  return mode === "files_with_matches" || mode === "count" ? mode : "content";
}

/** `Search "pattern" in glob in path (summary)`, in the pager's three cases. */
function searchTitle(call: ToolCallFacts): ToolTitle {
  const pattern = field(call.rawInput, "pattern", "glob_pattern") ?? call.title;
  const glob = field(call.rawInput, "glob");
  const scope = field(call.rawInput, "path", "target_directory");
  const parts: TitlePart[] = [];

  // A `.` or empty pattern means the glob *is* the search term, so it goes
  // unquoted where the pattern would have been.
  if ((pattern === "" || pattern === ".") && glob) {
    parts.push({ text: glob, role: "term" });
  } else {
    parts.push({ text: JSON.stringify(pattern), role: "term" });
    if (glob) {
      parts.push({ text: " in ", role: "plain" });
      parts.push({ text: glob, role: "term" });
    }
  }
  if (scope && scope !== ".") {
    parts.push({ text: " in ", role: "plain" });
    parts.push({ text: relativeTo(call.cwd, scope), role: "path" });
  }

  const grep = outputOfType(call.rawOutput, "GrepSearch");
  if (grep) {
    const matches = typeof grep["match_count"] === "number" ? grep["match_count"] : 0;
    const files = Array.isArray(grep["file_matches"]) ? grep["file_matches"].length : 0;
    parts.push({ text: ` ${matchSummary(matches, files, searchMode(call.rawInput))}`, role: "detail" });
  }
  return { verb: "Search ", parts, secondary: null };
}

/**
 * The lines a read actually covers.
 *
 * **`offset` is one-based**, and the tool says so: `resolve_read_start_line`
 * is documented "Harness-compatible negative offset resolution (1-indexed
 * start line)" and returns a positive `offset` unchanged as the start line
 * (`xai-grok-tools/src/implementations/grok_build/read_file/mod.rs`), with
 * `stored_read_offset` putting that same raw value on `FileContent.offset`.
 * Read live, `offset: 50, limit: 10` returns the ten lines whose own numbers
 * are 50 to 59, and the agent's numbered copy of them opens with `50→`.
 *
 * The pager computes `start = off + 1` and `end = off + lim`
 * (`tool_call_to_block`, `acp/tracker.rs`), a line too far at each end: it
 * labels that read `(51-60)` and numbers its gutter from 51. **This client
 * does not reproduce it**, which is the one place these two disagree on
 * purpose. A line number is a claim about where in the file you are looking,
 * and it is the part of a read a person carries back out to an editor; being
 * consistently wrong about it is worse than not showing it. Pinned against the
 * tool's own rule by `test/toolresult.test.ts`.
 *
 * Zero is the exception, and the reason the two agreed until now: the tool
 * folds `offset: 0` to line 1 as well (`if offset_raw == 0 { return 1 }`) while
 * `stored_read_offset` still puts the literal `0` on the wire, so `off + 1`
 * happens to be right for exactly that one value.
 */
export function readRange(
  offset: number | null,
  limit: number | null,
  total: number,
): { start: number; end: number } | null {
  if (offset === null && limit === null) return null;
  const start = offset === null || offset === 0 ? 1 : offset;
  const end = limit === null ? total : Math.min(start + limit - 1, total);
  return { start, end };
}

/** `Read path (1-50 of 200)`, or `Skill deploy`. */
function readTitle(call: ToolCallFacts): ToolTitle {
  const path = field(call.rawInput, "file_path", "target_file", "path") ?? call.title;
  const skill = skillName(path);
  if (skill) return { verb: "Skill ", parts: [{ text: skill, role: "path" }], secondary: null };

  const parts: TitlePart[] = [{ text: relativeTo(call.cwd, path), role: "path" }];
  const read = outputOfType(call.rawOutput, "ReadFile");
  const content = record(read?.["FileContent"]);
  const offset = typeof content["offset"] === "number" ? content["offset"] : null;
  const limit = typeof content["limit"] === "number" ? content["limit"] : null;
  const total = typeof content["total_lines"] === "number" ? content["total_lines"] : null;
  const range = total === null ? null : readRange(offset, limit, total);
  if (range && total !== null) {
    // The `of total` half appears only when the range is not the whole file —
    // the pager's own condition, so a short file is not labelled twice.
    const { start, end } = range;
    parts.push({
      text: total > end - start + 1 ? ` (${start}-${end} of ${total})` : ` (${start}-${end})`,
      role: "detail",
    });
  }
  return { verb: "Read ", parts, secondary: null };
}

/** `Edit path`, or `Creating path` when the call is a write rather than an edit. */
function editTitle(call: ToolCallFacts): ToolTitle {
  const path = field(call.rawInput, "file_path", "filePath", "target_file", "path") ?? call.title;
  const write = field(call.rawInput, "variant") === "Write";
  return {
    verb: write ? "Creating " : "Edit ",
    parts: [{ text: relativeTo(call.cwd, path), role: "path" }],
    secondary: null,
  };
}

/** `Run cmd`, with the redundant `cd <session cwd> &&` peeled off the front. */
function executeTitle(call: ToolCallFacts): ToolTitle {
  const named = call.title !== "" && !EXECUTE_TOOL_NAMES.includes(call.title.toLowerCase() as never);
  const raw = field(call.rawInput, "command") ?? (named ? call.title : "");
  const root = call.cwd.replace(/\/+$/, "");
  const prefix = `cd ${root} && `;
  const command = root && raw.startsWith(prefix) ? raw.slice(prefix.length) : raw;

  const description = field(call.rawInput, "description");
  if (description) {
    // `Run <description>`, with the command on a second line — the pager's
    // Label header, which is its default (`ExecuteHeaderStyle::Label`). The
    // verb stays, and `strip_leading_run_word` takes a leading Run/Running off
    // the description so it cannot read "Run Run the tests".
    const said = description.replace(/^(?:Running|Run)(?:\s+|$)/i, "");
    return { verb: "Run ", parts: [{ text: said, role: "plain" }], secondary: command || null };
  }
  // `Run …` and never the tool id, for an execute call with no command at all.
  return { verb: "Run ", parts: [{ text: command || "…", role: "command" }], secondary: null };
}

/** `List path`, reached by `rawInput.target_directory` rather than by kind. */
function listTitle(call: ToolCallFacts, target: string): ToolTitle {
  return {
    verb: "List ",
    parts: [{ text: relativeTo(call.cwd, target), role: "path" }],
    secondary: null,
  };
}

/** `Fetch url`. */
function fetchTitle(call: ToolCallFacts): ToolTitle {
  const url = field(call.rawInput, "url") ?? call.title.replace(/^Fetch: /, "");
  return { verb: "Fetch ", parts: [{ text: url, role: "command" }], secondary: null };
}

/**
 * Everything the pager sends to `OtherToolCallBlock`, `think` included.
 *
 * `acp::ToolKind::Think` has no arm of its own in `tool_call_to_block` and
 * falls through the catch-all, so it is titled by the same rule as the rest: a
 * name containing `": "` splits into a bold label and its content.
 */
function otherTitle(call: ToolCallFacts): ToolTitle {
  const at = call.title.indexOf(": ");
  if (at > 0) {
    return {
      verb: call.title.slice(0, at + 2),
      parts: [{ text: call.title.slice(at + 2), role: "plain" }],
      secondary: null,
    };
  }
  return { verb: null, parts: [{ text: call.title, role: "plain" }], secondary: null };
}

/** The title the terminal would have drawn for this call. */
export function toolTitle(call: ToolCallFacts): ToolTitle {
  const target = field(call.rawInput, "target_directory");
  switch (call.kind) {
    case "execute":
      return executeTitle(call);
    case "read":
      return readTitle(call);
    case "edit":
      return editTitle(call);
    case "search": {
      // A web search wears the `search` kind too, and the pager splits them on
      // `rawInput.variant` before it looks at anything else.
      const variant = field(call.rawInput, "variant");
      if (variant === "WebSearch" || variant === "XSearch" || /^(?:Web|X) search:/.test(call.title)) {
        const query = field(call.rawInput, "query") ?? call.title.replace(/^\w+ search:\s*/, "");
        return { verb: "Web search: ", parts: [{ text: query, role: "term" }], secondary: null };
      }
      return searchTitle(call);
    }
    case "fetch":
      return fetchTitle(call);
    default:
      // The pager reaches its list-directory block by the input field rather
      // than by kind, because the kind an agent sends for it is not fixed.
      return target ? listTitle(call, target) : otherTitle(call);
  }
}

/**
 * Whether this call failed.
 *
 * **Not the ACP status alone.** Watched live, `exit 3` arrives as
 * `status: "completed"` with `exit_code: 3` — the status says the tool ran, not
 * that the command succeeded. The pager reads both, `if !success ||
 * bash.exit_code != 0` (`tracker.rs`), and a client reading only the status
 * paints a failed build in the success colour.
 */
export function hasFailed(call: ToolCallFacts): boolean {
  if (call.status === "failed") return true;
  const bash = outputOfType(call.rawOutput, "Bash");
  if (!bash) return false;
  const code = bash["exit_code"];
  const signal = bash["signal"];
  return (typeof code === "number" && code !== 0) || (typeof signal === "string" && signal !== "");
}

/**
 * What a failed call says when it has nothing else to say.
 *
 * The pager replaces the model's error prose with a fixed phrase per kind
 * (`tracker.rs`), so the line reads the same however the tool worded it. The
 * shell command's `exit code N` comes off `ToolOutput::Bash`, which is on the
 * wire.
 */
export function failureText(call: ToolCallFacts, output: string): string {
  if (output.trim() !== "") return output;
  const bash = outputOfType(call.rawOutput, "Bash");
  if (bash) {
    const signal = bash["signal"];
    if (typeof signal === "string" && signal !== "") return signal;
    const code = bash["exit_code"];
    if (typeof code === "number" && code !== 0) return `exit code ${code}`;
    return "Command failed";
  }
  switch (call.kind) {
    case "execute":
      return "Command failed";
    case "read":
      return "Read failed";
    case "search":
      return "Search failed";
    case "fetch":
      return "Fetch failed";
    default:
      return field(call.rawInput, "target_directory") ? "List directory failed" : "Failed";
  }
}

/** How many head and tail lines this kind keeps when truncated. */
export function truncationFor(kind: string): { first: number; last: number } | null {
  if (kind === "read") return { first: READ_FIRST_LINES, last: READ_LAST_LINES };
  if (kind === "execute") return { first: EXECUTE_FIRST_LINES, last: EXECUTE_LAST_LINES };
  return null;
}

/**
 * The mode a click moves to.
 *
 * The pager's own two-state cycles, not a general three-state one: Read cycles
 * collapsed ↔ truncated and Search cycles collapsed ↔ expanded
 * (`read.rs`, `search.rs`). A kind with no truncation constants has no middle
 * state to stop at, so it opens the whole way.
 */
export function nextMode(kind: string, mode: DisplayMode): DisplayMode {
  if (mode !== "collapsed") return "collapsed";
  return truncationFor(kind) ? "truncated" : "expanded";
}

export interface TruncatedOutput {
  head: string[];
  /** Lines the fold is hiding; zero when everything fits. */
  hidden: number;
  tail: string[];
}

/** Split output into the head and tail a truncated fold shows. */
export function truncate(lines: readonly string[], kind: string): TruncatedOutput {
  const bounds = truncationFor(kind);
  if (!bounds || lines.length <= bounds.first + bounds.last) {
    return { head: [...lines], hidden: 0, tail: [] };
  }
  return {
    head: lines.slice(0, bounds.first),
    hidden: lines.length - bounds.first - bounds.last,
    tail: lines.slice(lines.length - bounds.last),
  };
}

/**
 * The line that stands in for what a fold is hiding.
 *
 * Two texts because the pager has two: an execute fold counts what it hid
 * (`… +47 lines`, `execute.rs`) and a read fold does not (`…`, `read.rs`). The
 * third — `... (9 more lines, press Enter to view)` — is deliberately not
 * reproduced: it names a key, and the affordance here is a click.
 */
export function ellipsisFor(kind: string, hidden: number): string {
  return kind === "execute" ? `… +${hidden} lines` : "…";
}

// ---------------------------------------------------------------------------
// What the terminal derives and a browser cannot
//
// Named rather than approximated, because a near-miss reads as agreement:
//
//   - **Syntax highlighting.** Read content and Edit diffs are painted by
//     `syntect` against the terminal's theme, selected from the file extension
//     and, for a scoped highlight, by reading the file off disk
//     (`app/edit_highlight_worker.rs`). Nothing on the wire carries it. A
//     read's line numbers and its text *are* on the wire, and
//     `toolresult.ts` draws them.
//   - **Typed output re-laid-out.** `rawOutput` carries a grep's hits and an
//     edit's diff as well, and neither is drawn from them yet. That is a gap
//     in this client, not a gap in the protocol.
//   - **Path surfaces by terminal width.** The pager shows a basename when
//     collapsed, a cwd-relative path when expanded and fish-shortens either to
//     fit the columns it has. A page has no column budget, so it shows the
//     cwd-relative form and lets the layout wrap.
//   - **Timing.** `started_at` is a local `Instant` the pager sets itself; the
//     wire carries no tool-call duration at all.
//   - **`~` expansion and process cwd.** `make_relative_path` uses the pager's
//     own working directory and `xai_dirs::home_dir()`. The session's `cwd` is
//     the only root a browser can honestly measure a path against.
// ---------------------------------------------------------------------------
