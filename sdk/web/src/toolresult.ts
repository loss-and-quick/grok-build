// The typed half of a tool call's result, decoded from `rawOutput`.
//
// `toolcall.ts` reads `rawOutput` for the two numbers a *title* needs. This
// module reads the rest of it, because the whole result is already there and
// this client was flattening it to prose: a read arrived carrying the file's
// own text plus the range it came from, and was drawn without a gutter.
//
// Nothing here asks the protocol for anything. Every field below was read off
// a live socket against a running gateway; the shapes are named at each
// decoder, with the Rust that builds them.
//
// What the terminal derives and this module cannot is at the bottom of the
// file, next to the same list in `toolcall.ts`.

import { readRange, type ToolCallFacts } from "./toolcall.ts";

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
// The one entry point
// ---------------------------------------------------------------------------

export type TypedResult = ReadResult;

/**
 * The typed result of a call, or `null` when the wire carried none.
 *
 * `null` is the ordinary case for most tools and for every call that failed:
 * `ToolOutput::ReadFile` carries `FileNotFound` rather than `FileContent`, and
 * a rejected call carries no `rawOutput` at all. Both fall back to the text
 * body, which is what the terminal shows for them too.
 */
export function typedResult(call: ToolCallFacts): TypedResult | null {
  return readResult(call.rawOutput);
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
//   - **Soft wrap accounting.** The pager wraps to its column budget and only
//     then counts rows, so its head-and-tail elision is measured in *visual*
//     rows. A page wraps in CSS and cannot count that before layout, so the
//     elision here is measured in file lines.
// ---------------------------------------------------------------------------
