// The typed half of a tool call's result, decoded from `rawOutput`.
//
// `toolcall.ts` reads `rawOutput` for the two numbers a *title* needs. This
// module reads the rest of it, because the whole result is already there and
// this client was flattening it to prose: a grep arrived carrying every hit
// with its file and line number and was drawn as a paragraph, and a read
// arrived carrying the file's own text plus the range it came from and was
// drawn without a gutter.
//
// Nothing here asks the protocol for anything. Every field below was read off
// a live socket against a running gateway; the shapes are named at each
// decoder, with the Rust that builds them.
//
// What the terminal derives and this module cannot is at the bottom of the
// file, next to the same list in `toolcall.ts`.

import { readRange, relativeTo, type ToolCallFacts } from "./toolcall.ts";

// ---------------------------------------------------------------------------
// Shared decoding helpers
// ---------------------------------------------------------------------------

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
}

/**
 * One arm of `ToolOutput`, which is `#[serde(tag = "type")]`
 * (`xai-grok-tools/src/types/output.rs`), so the variant name is a `type`
 * string on the same object as its fields.
 *
 * The inner enums carry no serde attribute at all, so they stay *externally*
 * tagged and land one level down: a read is
 * `{"type":"ReadFile","FileContent":{…}}` and a failed one is
 * `{"type":"ReadFile","FileNotFound":"…"}`. Both shapes are live captures.
 */
function outputOfType(rawOutput: unknown, type: string): Record<string, unknown> | null {
  const raw = record(rawOutput);
  return raw["type"] === type ? raw : null;
}

function str(from: Record<string, unknown>, name: string): string | null {
  const value = from[name];
  return typeof value === "string" ? value : null;
}

function num(from: Record<string, unknown>, name: string): number | null {
  const value = from[name];
  return typeof value === "number" ? value : null;
}

/**
 * How many rows one typed result may draw before it says it stopped.
 *
 * **The pager does not cap what this applies to.** Its search block renders
 * every hit it was given, and its blocks are bounded only by the height of the
 * pane they are drawn into, which a scrolling page does not have.
 * So the cap is this client's, and rather than inventing a number it takes the
 * one line budget this product already states for a tool result:
 * `CONTENT_LINE_DEFAULT` in
 * `xai-grok-tools/src/implementations/grok_build/grep/mod.rs`, the number of
 * lines a grep returns when the model does not ask for more. A result longer
 * than a default grep's worth is longer than anything the terminal routinely
 * shows in one entry.
 *
 * The transcript is not virtualized (a deliberate deferral, `docs/WEB-DEPS.md`
 * §4.4), so every row is a live DOM node; a 2000-hit search would put 2000 of
 * them in a page that already holds the whole session. Whatever is dropped is
 * counted and said out loud — `hidden` on every result below.
 */
export const TYPED_LINE_LIMIT = 200;

// ---------------------------------------------------------------------------
// Read: a line gutter over the file's own text
// ---------------------------------------------------------------------------

/**
 * `{"type":"ReadFile","FileContent":{…}}`, live:
 *
 *     content        "1→def fn_1(…)\n    return …\n…\n40→    return …"
 *     content_concise  same, or a shorter retelling
 *     absolute_path  "/…/long.py"
 *     offset         50            (absent for a whole-file read)
 *     limit          10            (absent for a whole-file read)
 *     raw_output     "    return needle_4 * 25\ndef fn_26(…)\n…"
 *     total_lines    801
 *
 * Two channels again, and again the typed one wins. `content` is the model's
 * copy with a `N→` marker interleaved every tenth line; `raw_output` is the
 * file's own text, and it is the one the pager reads too
 * (`block.with_content(fc.raw_output, …)`, `tracker.rs`). Feeding the marked
 * copy into a gutter would print two line numbers on every tenth row.
 */
export interface ReadResult {
  kind: "read";
  /** Absolute path as the tool reported it; relativised at the call site. */
  path: string;
  /** Line number of the first row. */
  base: number;
  lines: string[];
  total: number;
}

export function readResult(rawOutput: unknown): ReadResult | null {
  const read = outputOfType(rawOutput, "ReadFile");
  if (!read) return null;
  const content = record(read["FileContent"]);
  const text = str(content, "raw_output");
  if (text === null) return null;
  const total = num(content, "total_lines") ?? 0;
  const range = readRange(num(content, "offset"), num(content, "limit"), total);
  return {
    kind: "read",
    path: str(content, "absolute_path") ?? "",
    base: range?.start ?? 1,
    // `content.lines()` in the pager, which is a plain split that drops the
    // final empty piece a trailing newline leaves behind. A file's last line
    // is a line, not an empty row after it.
    lines: text === "" ? [] : text.replace(/\n$/, "").split("\n"),
    total,
  };
}

// ---------------------------------------------------------------------------
// Search: the hits, by file and line
// ---------------------------------------------------------------------------

/**
 * `{"type":"GrepSearch",…}`, live:
 *
 *     stdout       [byte…]   the `<workspace_result>` block ripgrep was wrapped in
 *     stderr       [byte…]
 *     exit_code    0, or 1 for no matches
 *     match_count  120
 *     file_matches [{path, matches:[{line_number, content}]}]
 *
 * `file_matches` is empty in `files_with_matches` and `count` mode, where the
 * paths exist only inside `stdout`; the pager parses them back out of that
 * envelope (`parse_file_paths_from_stdout`, `tracker.rs`) and so does
 * {@link filePathsFromStdout}.
 *
 * Note `exit_code` here is ripgrep's and is *not* a failure: a search that
 * found nothing exits 1 and is `status: "completed"`. Only `hasFailed` reads
 * an exit code, and only from `ToolOutput::Bash`.
 */
export interface SearchHit {
  line: number;
  text: string;
}

export interface SearchFile {
  path: string;
  matches: SearchHit[];
}

export interface SearchResult {
  kind: "search";
  matchCount: number;
  files: SearchFile[];
  /** `files_with_matches` and `count` mode, where a hit has no line of its own. */
  paths: string[];
  /** Hit lines dropped by {@link TYPED_LINE_LIMIT}; zero when everything fits. */
  hidden: number;
  meta: SearchMeta;
}

/** The one metadata line an expanded search draws. `metadata_line`, `search.rs`. */
export interface SearchMeta {
  mode: "pattern" | "files" | "count";
  fileType: string | null;
  caseInsensitive: boolean;
  multiline: boolean;
}

export function searchMeta(rawInput: unknown): SearchMeta {
  const raw = record(rawInput);
  const declared = str(raw, "output_mode");
  return {
    mode: declared === "files_with_matches" ? "files" : declared === "count" ? "count" : "pattern",
    fileType: str(raw, "type") || null,
    caseInsensitive: raw["-i"] === true,
    multiline: raw["multiline"] === true,
  };
}

/**
 * The path lines out of a grep's stdout envelope.
 *
 * The shell wraps ripgrep in `<workspace_result workspace_path="…">` with a
 * `Found N files` line under it; everything else on its own line is a path.
 * The pager's filter, character for character (`tracker.rs`).
 */
export function filePathsFromStdout(stdout: string): string[] {
  return stdout
    .split("\n")
    .filter((line) => line !== "" && !line.startsWith("<") && !line.startsWith("Found "));
}

function bytesToText(value: unknown): string {
  if (!Array.isArray(value)) return "";
  return new TextDecoder().decode(Uint8Array.from(value as number[]));
}

export function searchResult(rawOutput: unknown, rawInput: unknown): SearchResult | null {
  const grep = outputOfType(rawOutput, "GrepSearch");
  if (!grep) return null;
  const matchCount = num(grep, "match_count") ?? 0;
  const raw = Array.isArray(grep["file_matches"]) ? (grep["file_matches"] as unknown[]) : [];

  const files: SearchFile[] = [];
  let shown = 0;
  let hidden = 0;
  for (const entry of raw) {
    const file = record(entry);
    const hits = Array.isArray(file["matches"]) ? (file["matches"] as unknown[]) : [];
    const kept: SearchHit[] = [];
    for (const hit of hits) {
      const row = record(hit);
      const line = num(row, "line_number");
      if (line === null) continue;
      if (shown >= TYPED_LINE_LIMIT) {
        hidden += 1;
        continue;
      }
      shown += 1;
      // `m.content.trim_end()` — the pager's own trim, so a file with CRLF
      // endings does not draw a stray column of carriage returns.
      kept.push({ line, text: (str(row, "content") ?? "").replace(/\s+$/, "") });
    }
    // A file whose every hit fell past the limit is dropped whole rather than
    // drawn as a heading with nothing under it.
    if (kept.length > 0) files.push({ path: str(file, "path") ?? "", matches: kept });
  }

  const paths =
    files.length === 0 && matchCount > 0 ? filePathsFromStdout(bytesToText(grep["stdout"])) : [];
  return {
    kind: "search",
    matchCount,
    files,
    paths: paths.slice(0, TYPED_LINE_LIMIT),
    hidden: hidden + Math.max(0, paths.length - TYPED_LINE_LIMIT),
    meta: searchMeta(rawInput),
  };
}


// ---------------------------------------------------------------------------
// The one entry point
// ---------------------------------------------------------------------------

export type TypedResult = ReadResult | SearchResult;

/**
 * The typed result of a call, or `null` when the wire carried none.
 *
 * `null` is the ordinary case for most tools and for every call that failed:
 * `ToolOutput::ReadFile` carries `FileNotFound` rather than `FileContent`, and
 * a rejected call carries no `rawOutput` at all. Both fall back to the text
 * body, which is what the terminal shows for them too.
 */
export function typedResult(call: ToolCallFacts): TypedResult | null {
  return readResult(call.rawOutput) ?? searchResult(call.rawOutput, call.rawInput);
}

/** A typed result's paths, shown against the session's root like every other. */
export function resultPath(cwd: string, path: string): string {
  // Grep reports its hits under the scope it was given, so a search rooted at
  // `.` yields `/…/work/./hits_0.txt`. The `/./` is the tool's join, not part
  // of any name.
  return relativeTo(cwd, path.replaceAll("/./", "/"));
}

// ---------------------------------------------------------------------------
// What the terminal draws here and a browser cannot
//
//   - **Syntax highlighting.** `syntect` picks a grammar from the file
//     extension alone (`find_syntax_by_file_path`,
//     `xai-grok-markdown/src/syntax.rs`) and paints it in the terminal's own
//     `.tmTheme`. Nothing on the wire carries it, and a second highlighter
//     would be a second opinion about the same code — the same call
//     `markdown.ts` already makes about fenced code.
//   - **Match spans inside a grep hit.** The terminal does not draw them
//     either, and could not: `SearchLineMatch` is `{line_number, content}` and
//     the regex offsets are never on the wire.
//   - **Soft wrap accounting.** The pager wraps to its column budget and only
//     then counts rows, so its head-and-tail elision is measured in *visual*
//     rows. A page wraps in CSS and cannot count that before layout, so the
//     elision here is measured in file lines.
// ---------------------------------------------------------------------------
