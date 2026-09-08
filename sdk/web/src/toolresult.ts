// The typed half of a tool call's result, decoded from `rawOutput`.
//
// `toolcall.ts` reads `rawOutput` for the two numbers a *title* needs. This
// module reads the rest of it, because the whole result is already there and
// this client was flattening it to prose: a grep arrived carrying every hit
// with its file and line number and was drawn as a paragraph, a read arrived
// carrying the file's own text plus the range it came from and was drawn
// without a gutter, and an edit arrived carrying the edited region, its line
// numbers and three lines of context on each side and was drawn as neither.
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
 * Rust's `str::split_inclusive('\n')`, which is what both the diff builder and
 * the tool's own context fields are cut with.
 *
 * It is not `String.split`: an empty string yields *no* lines rather than one
 * empty one, and the newline stays on the line it ended. Both matter — an
 * empty `old_string` is an insertion with no delete side at all, and
 * `"world"` against `"world\n"` is a changed line to
 * `TextDiff::from_lines`, which is why a write that only adds a trailing
 * newline still shows a row.
 */
export function splitInclusive(text: string): string[] {
  if (text === "") return [];
  const lines: string[] = [];
  let start = 0;
  for (let at = 0; at < text.length; at += 1) {
    if (text[at] === "\n") {
      lines.push(text.slice(start, at + 1));
      start = at + 1;
    }
  }
  if (start < text.length) lines.push(text.slice(start));
  return lines;
}

/**
 * How many rows one typed result may draw before it says it stopped.
 *
 * **The pager caps neither of the two this applies to.** Its search block
 * renders every hit it was given and its diff renderer is bounded only by the
 * height of the pane it is drawn into, which a scrolling page does not have.
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
// Edit: hunks, from the edited region the wire already carries
// ---------------------------------------------------------------------------

/**
 * `{"type":"SearchReplace","EditsApplied":{…}}`, live:
 *
 *     old_string / new_string     the whole replacement, both sides
 *     absolute_path
 *     tool_output_for_prompt      "The file edit_me.txt has been updated successfully."
 *     edits.details[]  {
 *       old_string, old_line, new_string, new_line,   // both line numbers 1-based
 *       context_before: "alpha\nbravo\n",             // up to three lines
 *       context_after:  "delta\necho\nfoxtrot\n",     // up to three lines
 *       line_prefix:    ""                            // indent the strings start after
 *     }
 *
 * **So the hunk is on the wire and is not recomputed here.** The tool has
 * already decided where the edit sits and cut the three lines of context the
 * pager wants around it, which is why `build_diff_hunks`'s `MAX_CONTEXT` trim
 * is usually a no-op. The one thing the wire does *not* carry is which lines
 * inside `old_string` survived into `new_string` — that correspondence is
 * computed, by the pager with `similar::TextDiff::from_lines` and here by
 * {@link lineDiff}. Without it a ten-line replacement that changed one line
 * draws twenty changed rows instead of two.
 *
 * A `Write` arrives on this same variant with an empty `old_string` and no
 * context, which falls out of the port as a single all-insert hunk.
 */
export type DiffTag = "equal" | "delete" | "insert";

export interface DiffLine {
  text: string;
  /** Line number on the old side; meaningless for an insert. */
  lo: number;
  /** Line number on the new side; meaningless for a delete. */
  ln: number;
  tag: DiffTag;
}

export type DiffHunk = DiffLine[];

export interface DiffResult {
  kind: "diff";
  path: string;
  hunks: DiffHunk[];
  /** Rows dropped by {@link TYPED_LINE_LIMIT}; zero when everything fits. */
  hidden: number;
  insertions: number;
  deletions: number;
}

/**
 * Lines of context the tool cuts on each side, and the trim the pager applies.
 * `MAX_CONTEXT` in `xai-grok-pager-diff/src/lib.rs`.
 */
export const MAX_CONTEXT = 3;

/** `hunk_separator` — `EditBlockConfig`'s default in `appearance/config.rs`. */
export const HUNK_SEPARATOR = "…";

/**
 * Largest LCS table this client will build, in cells.
 *
 * A number this client owns, because only a browser pays for it: the table is
 * `old × new` 32-bit cells, so this is a 16 MB ceiling on one edit. Above it
 * the replacement is shown whole — every old line deleted, every new line
 * inserted — which is what an un-diffed replacement is, rather than blocking
 * the tab. Common prefixes and suffixes are trimmed before the table is sized,
 * so a real edit reaches this only if both of its sides are thousands of lines
 * that share nothing; the terminal, running Myers, still diffs that case.
 */
const LCS_CELL_LIMIT = 4_000_000;

/**
 * A line-level diff of two strings, the way the hunk builder needs it.
 *
 * The pager calls `similar::TextDiff::from_lines`, which is Myers. This is a
 * longest-common-subsequence walk. **Both produce a shortest edit script**, so
 * they agree on how many lines changed; where they can disagree is which of two
 * equally short scripts they pick when a line repeats — with `["A","B"]`
 * against `["B","A"]` either `A` may be the one that moves. That is the whole
 * divergence class, and it is named here rather than papered over.
 *
 * A library was considered and refused under `docs/WEB-DEPS.md`'s rule. `diff`
 * (jsdiff) is a *third* implementation, not `similar`, so it buys no agreement
 * with the terminal — the same reason `fuzzysort` was refused for the command
 * menu — while minimality, which is the entire specification here, is what an
 * LCS gives by construction.
 *
 * Deletes are emitted before inserts within a replacement, which is the order
 * `iter_all_changes` yields a `Replace` op in.
 */
export function lineDiff(oldText: string, newText: string): { tag: DiffTag; text: string }[] {
  const before = splitInclusive(oldText);
  const after = splitInclusive(newText);

  const out: { tag: DiffTag; text: string }[] = [];
  // Common prefix and suffix are Equal in any minimal script, and trimming
  // them is what keeps the table small enough to build at all.
  let head = 0;
  while (head < before.length && head < after.length && before[head] === after[head]) {
    out.push({ tag: "equal", text: before[head]! });
    head += 1;
  }
  let tail = 0;
  while (
    tail < before.length - head &&
    tail < after.length - head &&
    before[before.length - 1 - tail] === after[after.length - 1 - tail]
  ) {
    tail += 1;
  }
  const a = before.slice(head, before.length - tail);
  const b = after.slice(head, after.length - tail);

  const middle: { tag: DiffTag; text: string }[] = [];
  if ((a.length + 1) * (b.length + 1) > LCS_CELL_LIMIT) {
    for (const text of a) middle.push({ tag: "delete", text });
    for (const text of b) middle.push({ tag: "insert", text });
  } else {
    const width = b.length + 1;
    const lcs = new Int32Array((a.length + 1) * width);
    for (let i = a.length - 1; i >= 0; i -= 1) {
      for (let j = b.length - 1; j >= 0; j -= 1) {
        lcs[i * width + j] =
          a[i] === b[j]
            ? lcs[(i + 1) * width + j + 1]! + 1
            : Math.max(lcs[(i + 1) * width + j]!, lcs[i * width + j + 1]!);
      }
    }
    let i = 0;
    let j = 0;
    while (i < a.length && j < b.length) {
      if (a[i] === b[j]) {
        middle.push({ tag: "equal", text: a[i]! });
        i += 1;
        j += 1;
      } else if (lcs[(i + 1) * width + j]! >= lcs[i * width + j + 1]!) {
        middle.push({ tag: "delete", text: a[i]! });
        i += 1;
      } else {
        middle.push({ tag: "insert", text: b[j]! });
        j += 1;
      }
    }
    while (i < a.length) middle.push({ tag: "delete", text: a[i++]! });
    while (j < b.length) middle.push({ tag: "insert", text: b[j++]! });
  }
  out.push(...middle);
  for (let at = after.length - tail; at < after.length; at += 1) {
    out.push({ tag: "equal", text: after[at]! });
  }
  return out;
}

interface EditDetail {
  oldString: string;
  oldLine: number;
  newString: string;
  newLine: number;
  contextBefore: string;
  contextAfter: string;
  linePrefix: string;
}

function editDetails(from: unknown): EditDetail[] {
  const raw = Array.isArray(from) ? (from as unknown[]) : [];
  return raw.map((entry) => {
    const detail = record(entry);
    return {
      oldString: str(detail, "old_string") ?? "",
      oldLine: num(detail, "old_line") ?? 1,
      newString: str(detail, "new_string") ?? "",
      newLine: num(detail, "new_line") ?? 1,
      contextBefore: str(detail, "context_before") ?? "",
      contextAfter: str(detail, "context_after") ?? "",
      linePrefix: str(detail, "line_prefix") ?? "",
    };
  });
}

/**
 * `build_diff_hunks` (`xai-grok-pager-diff/src/lib.rs`), ported.
 *
 * Line for line, including the two rules that are not obvious from the shape:
 * an empty-to-empty edit *with* context is a blank line being inserted and is
 * diffed as `"\n"`, and `line_prefix` is prepended to the first changed line on
 * each side because `old_string` may start after the indentation the context
 * lines carry.
 */
export function buildDiffHunks(from: unknown): DiffHunk[] {
  const hunks: DiffHunk[] = [];
  for (const edit of editDetails(from)) {
    const lines: DiffHunk = [];

    const beforeLines = splitInclusive(edit.contextBefore);
    for (const [i, text] of beforeLines.entries()) {
      // The last `context_before` line sits just above old_line/new_line.
      const fromEnd = Math.max(0, beforeLines.length - (i + 1));
      lines.push({
        text,
        lo: Math.max(0, edit.oldLine - (fromEnd + 1)),
        ln: Math.max(0, edit.newLine - (fromEnd + 1)),
        tag: "equal",
      });
    }

    let lo = edit.oldLine;
    let ln = edit.newLine;
    const emptyToEmpty = edit.oldString === "" && edit.newString === "";
    const midFile = edit.contextBefore !== "" || edit.contextAfter !== "";
    const newText = emptyToEmpty && midFile ? "\n" : edit.newString;

    const prefix = edit.linePrefix;
    let prefixedDelete = false;
    let prefixedInsert = false;
    for (const change of lineDiff(edit.oldString, newText)) {
      let text = change.text;
      if (prefix !== "") {
        const needs =
          change.tag === "delete"
            ? !prefixedDelete
            : change.tag === "insert"
              ? !prefixedInsert
              : !prefixedDelete && !prefixedInsert;
        if (needs) text = prefix + text;
        if (change.tag === "insert") prefixedInsert = true;
        else prefixedDelete = true;
      }
      lines.push({ text, lo, ln, tag: change.tag });
      if (change.tag === "equal") {
        lo += 1;
        ln += 1;
      } else if (change.tag === "delete") {
        lo += 1;
      } else {
        ln += 1;
      }
    }

    for (const text of splitInclusive(edit.contextAfter)) {
      lines.push({ text, lo, ln, tag: "equal" });
      lo += 1;
      ln += 1;
    }

    let start: number;
    let end = lines.length;
    if (lines.every((line) => line.tag === "equal")) {
      start = end;
    } else {
      let equalBefore = 0;
      while (equalBefore < lines.length && lines[equalBefore]!.tag === "equal") equalBefore += 1;
      let equalAfter = 0;
      while (equalAfter < lines.length && lines[lines.length - 1 - equalAfter]!.tag === "equal") {
        equalAfter += 1;
      }
      start = Math.max(0, equalBefore - MAX_CONTEXT);
      end = lines.length - Math.max(0, equalAfter - MAX_CONTEXT);
    }
    // Blank context lines at either edge are trimmed, so a hunk never opens or
    // closes on an empty row.
    while (start < end && lines[start]!.tag === "equal" && lines[start]!.text.trim() === "") {
      start += 1;
    }
    while (start < end && lines[end - 1]!.tag === "equal" && lines[end - 1]!.text.trim() === "") {
      end -= 1;
    }
    if (start < end) hunks.push(lines.slice(start, end));
  }
  return hunks;
}

/**
 * Unchanged lines between two hunks, for the separator's own count.
 *
 * `hunk_gap_lines` (`edit.rs`): measured on the new side, and `null` — a bare
 * separator with no count — when the hunks are adjacent or out of order, which
 * happens when one call's later edit landed above an earlier one.
 */
export function hunkGapLines(prev: DiffHunk, next: DiffHunk): number | null {
  const last = [...prev].reverse().find((line) => line.tag !== "delete");
  const first = next.find((line) => line.tag !== "delete");
  if (!last || !first) return null;
  const gap = first.ln - last.ln - 1;
  return gap > 0 ? gap : null;
}

/** The separator between two hunks, counted when the count is knowable. */
export function hunkSeparator(prev: DiffHunk, next: DiffHunk): string {
  const gap = hunkGapLines(prev, next);
  if (gap === null) return HUNK_SEPARATOR;
  return `${HUNK_SEPARATOR} ${gap} unchanged line${gap === 1 ? "" : "s"}`;
}

/**
 * Width of the single line-number column of a hunk.
 *
 * `gutter_layout` with `dual_line_numbers: false`, its default: one column,
 * right-aligned, as wide as the larger of the two sides' largest number.
 */
export function gutterWidth(hunk: DiffHunk): number {
  let widest = 1;
  for (const line of hunk) widest = Math.max(widest, line.lo, line.ln);
  return String(widest).length;
}

export function diffResult(rawOutput: unknown): DiffResult | null {
  const edit = outputOfType(rawOutput, "SearchReplace");
  if (!edit) return null;
  const applied = record(edit["EditsApplied"]);
  const hunks = buildDiffHunks(record(applied["edits"])["details"]);
  if (hunks.length === 0) return null;

  let insertions = 0;
  let deletions = 0;
  for (const hunk of hunks) {
    for (const line of hunk) {
      if (line.tag === "insert") insertions += 1;
      else if (line.tag === "delete") deletions += 1;
    }
  }

  const kept: DiffHunk[] = [];
  let shown = 0;
  let hidden = 0;
  for (const hunk of hunks) {
    if (shown >= TYPED_LINE_LIMIT) {
      hidden += hunk.length;
      continue;
    }
    const room = TYPED_LINE_LIMIT - shown;
    kept.push(hunk.slice(0, room));
    hidden += Math.max(0, hunk.length - room);
    shown += Math.min(hunk.length, room);
  }

  return {
    kind: "diff",
    path: str(applied, "absolute_path") ?? "",
    hunks: kept,
    hidden,
    insertions,
    deletions,
  };
}

// ---------------------------------------------------------------------------
// The one entry point
// ---------------------------------------------------------------------------

export type TypedResult = ReadResult | SearchResult | DiffResult;

/**
 * The typed result of a call, or `null` when the wire carried none.
 *
 * `null` is the ordinary case for most tools and for every call that failed:
 * `ToolOutput::ReadFile` carries `FileNotFound` rather than `FileContent`, and
 * a rejected call carries no `rawOutput` at all. Both fall back to the text
 * body, which is what the terminal shows for them too.
 */
export function typedResult(call: ToolCallFacts): TypedResult | null {
  return (
    readResult(call.rawOutput) ??
    searchResult(call.rawOutput, call.rawInput) ??
    diffResult(call.rawOutput)
  );
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
//   - **Syntax highlighting**, on both the read body and the diff. `syntect`
//     picks a grammar from the file extension alone
//     (`find_syntax_by_file_path`, `xai-grok-markdown/src/syntax.rs`) and paints
//     it in the terminal's own `.tmTheme`. The diff goes further and *re-reads
//     the post-edit file off disk* on a background thread, so a construct that
//     opens above the hunk still closes correctly
//     (`app/edit_highlight_worker.rs`); the disk text supplies styles only, and
//     is discarded line by line if it no longer matches the hunk. Nothing on
//     the wire carries either, a browser has no disk, and a second highlighter
//     would be a second opinion about the same code — the same call
//     `markdown.ts` already makes about fenced code.
//   - **Which of two equally short diffs**, when a line repeats. Named at
//     {@link lineDiff}.
//   - **Match spans inside a grep hit.** The terminal does not draw them
//     either, and could not: `SearchLineMatch` is `{line_number, content}` and
//     the regex offsets are never on the wire.
//   - **Soft wrap accounting.** The pager wraps to its column budget and only
//     then counts rows, so its head-and-tail elision is measured in *visual*
//     rows. A page wraps in CSS and cannot count that before layout, so the
//     elision here is measured in file lines.
// ---------------------------------------------------------------------------
