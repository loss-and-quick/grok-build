// Markdown, parsed by a real parser and handed on as data.
//
// The terminal runs `pulldown-cmark 0.13` with `ENABLE_GFM | STRIKETHROUGH |
// MATH | TASKLISTS | TABLES` (`xai-grok-markdown-core/src/lib.rs`,
// `parser_options`), and `PanelBlock::Markdown`'s own doc comment promises
// "headings, lists, tables, code — the same one used for model output". The
// eighty-line regex parser that used to live here kept none of that promise: a
// plugin that wrote a table got a table in the terminal and a row of pipes in
// the browser. That is the divergence this client exists to argue against, so
// the parsing is now `markdown-it`, which is CommonMark-complete.
//
// **`markdown-it` and not `marked`,** which is the more obvious pick and the
// one opencode made. Three defaults decide it, and each is checkable:
//
//   - `~10%~` is literal text here and struck through by `marked`. The
//     terminal demotes a single tilde on purpose — `offset_events` in
//     `markdown-core` guards against model output like `~**10%**` — so
//     `markdown-it` agrees with the pager and `marked` does not.
//   - raw HTML is text, not markup, with `html: false`. `marked` passes it
//     through, which is why every `marked` front end needs a sanitiser after
//     it; opencode's share site is the cautionary case, rendering `marked`
//     output straight into `innerHTML` with no DOMPurify in the package at all.
//   - `javascript:` is refused by `validateLink` before it can become a link.
//     `marked` emits the href.
//
// ## The two guarantees, and where each is enforced
//
// **Panel text is plugin-authored and it never becomes markup.** Nothing here
// produces an HTML string, so there is no `innerHTML` and no sanitiser to
// forget: {@link parseMarkdown} answers with nodes and `Markdown.tsx` builds
// DOM from them, so an `<img onerror=…>` in a panel is a text node by
// construction rather than by a filter that has to be remembered.
//
// **Only `https?:` keeps an `href`,** and that decision is made twice on
// purpose. `markdown-it`'s own `validateLink` drops `javascript:`, `vbscript:`,
// `file:` and non-image `data:` at parse time; {@link safeHref} then keeps only
// what this page is willing to navigate to. The terminal is laxer — its
// `SchemeFilter::Standard` also passes `mailto:`
// (`pager-render/src/terminal/hyperlinks.rs`) — and it can afford to be: an
// unfiltered scheme in a terminal is inert text, while in a browser an href is
// something a click executes.
//
// ## Streaming, and why blocks exist here at all
//
// An assistant turn arrives as many `agent_message_chunk`s, so the same
// document is parsed once per delta over a buffer that only grows. Measured
// with `markdown-it` over a 24 KB reply arriving in 20-character chunks: 1.2 ms
// per parse by the end of the turn and 1.5 s of parsing across it — and, far
// worse than the parsing, a whole-document node tree rebuilt on every chunk,
// which tears down and recreates the message's DOM hundreds of times and takes
// the reader's text selection with it.
//
// The pager already answered this, and this module takes its answer instead of
// inventing one. `StreamingMarkdownRenderer` freezes rendered output at
// *checkpoints* and re-renders only the tail, and a checkpoint is only ever a
// **top-level block boundary**: "Blocks nested inside lists, blockquotes, or
// tables cannot be checkpoints because the outer container might continue"
// (`xai-grok-markdown/src/checkpoint.rs`). {@link createMarkdownStream} freezes
// on that same boundary, expressed the one way a token stream allows it: a
// top-level block that has a *successor* has ended, so every block but the last
// is frozen and only the last is re-parsed and re-rendered.
//
// Two consequences are the pager's as well, and are named here rather than
// hidden. An open fence never freezes, so it renders as a code block that grows
// — the pager keeps `open_code_highlighter` for exactly that tail. And a link
// reference definition arriving *after* its use cannot reach back into frozen
// output; the parser `env` is shared across pushes so a definition already seen
// still resolves, which is as far as a frozen prefix can go in either client.
import MarkdownIt from "markdown-it";
import type { Token } from "markdown-it";

/** Only these schemes keep an `href`. Everything else renders as plain text. */
export const ALLOWED_HREF = /^https?:\/\//i;

/** The href to put on an anchor, or `null` when the target was refused. */
export function safeHref(url: string | null | undefined): string | null {
  return url != null && ALLOWED_HREF.test(url) ? url : null;
}

/**
 * The parser, configured once.
 *
 * `linkify` is on because GFM autolinks bare URLs and the pager linkifies them
 * a second time in prose (`xai-grok-markdown/src/url_scan.rs`). Bare *emails*
 * are switched off with it: the terminal turns them into `mailto:`, which
 * {@link safeHref} refuses, and an anchor with no href is a worse answer than
 * the plain text the address already was.
 *
 * `typographer` stays off. It rewrites quotes and dashes, and `pulldown-cmark`
 * is configured without `ENABLE_SMART_PUNCTUATION`, so turning it on would make
 * the two clients disagree about the characters in a plugin's own sentence.
 */
const PARSER = new MarkdownIt({ html: false, linkify: true, typographer: false });
PARSER.linkify.set({ fuzzyEmail: false });

/**
 * Tags this renderer will build an element for.
 *
 * A token whose tag is not here contributes its children and no wrapper, so a
 * parser rule this client has not accounted for degrades to its text instead of
 * reaching the DOM as an unreviewed element name.
 */
export const MARKDOWN_TAGS = [
  "p",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "ul",
  "ol",
  "li",
  "blockquote",
  "table",
  "thead",
  "tbody",
  "tr",
  "th",
  "td",
  "hr",
  "strong",
  "em",
  "s",
  "a",
] as const;

export type MarkdownTag = (typeof MARKDOWN_TAGS)[number];

const TAGS: ReadonlySet<string> = new Set(MARKDOWN_TAGS);

export type MarkdownNode =
  | { kind: "text"; text: string }
  /** A hard break; a soft break is folded into a space, as the pager folds it. */
  | { kind: "break" }
  | { kind: "inline_code"; text: string }
  /** A fenced or indented block. `language` is the fence's first info word. */
  | { kind: "code"; text: string; language: string | null }
  | {
      kind: "element";
      tag: MarkdownTag;
      /** Anchors only, and `null` when {@link safeHref} refused the target. */
      href: string | null;
      /** `<ol start>`, when the list did not begin at one. */
      start: number | null;
      children: MarkdownNode[];
    };

function attr(token: Token, name: string): string | null {
  const found = token.attrs?.find(([key]) => key === name);
  return found ? String(found[1]) : null;
}

function elementOf(token: Token, children: MarkdownNode[]): MarkdownNode {
  const start = attr(token, "start");
  return {
    kind: "element",
    tag: token.tag as MarkdownTag,
    href: token.tag === "a" ? safeHref(attr(token, "href")) : null,
    start: start === null ? null : Number(start),
    children,
  };
}

/**
 * Fold `markdown-it`'s flat token stream into a tree.
 *
 * The stream is flat and self-describing: `nesting` is `1` to open, `-1` to
 * close and `0` for a leaf, and an `inline` token carries its own such stream
 * in `children`. So this walk is generic — it knows about nesting, not about
 * markdown — and everything markdown-shaped stays in the parser.
 */
function build(tokens: readonly Token[]): MarkdownNode[] {
  const nodes: MarkdownNode[] = [];
  let at = 0;

  while (at < tokens.length) {
    const token = tokens[at];
    at += 1;
    if (!token) continue;

    if (token.nesting === 1) {
      // Find this token's own closing partner, counting nesting so a `li`
      // inside a `li` closes the inner one first.
      let depth = 1;
      const from = at;
      while (at < tokens.length && depth > 0) {
        const nesting = tokens[at]?.nesting ?? 0;
        depth += nesting;
        at += 1;
      }
      const inner = build(tokens.slice(from, Math.max(from, at - 1)));
      // A tight list hides the paragraph inside each item; keeping the wrapper
      // would put a blank line between bullets the terminal draws adjacent.
      if (token.hidden || !TAGS.has(token.tag)) nodes.push(...inner);
      else nodes.push(elementOf(token, inner));
      continue;
    }

    switch (token.type) {
      case "inline":
        nodes.push(...build(token.children ?? []));
        break;
      case "text":
        if (token.content) nodes.push({ kind: "text", text: token.content });
        break;
      case "softbreak":
        // `pulldown-cmark` collapses a soft break to a space unless a block
        // continues on the next line, and the pager renders that collapse.
        nodes.push({ kind: "text", text: " " });
        break;
      case "hardbreak":
        nodes.push({ kind: "break" });
        break;
      case "code_inline":
        nodes.push({ kind: "inline_code", text: token.content });
        break;
      case "fence":
      case "code_block":
        nodes.push({
          kind: "code",
          text: token.content,
          language: token.info.trim().split(/\s+/)[0] || null,
        });
        break;
      case "image":
        // The pager renders an image as `alt (src)` — its pretty mode rewrites
        // `](` to ` (` and drops the brackets — and no client here fetches a
        // URL a plugin chose, so the browser renders the same text rather than
        // an `<img>` that would reach out to the plugin's host on load.
        nodes.push({ kind: "text", text: `${token.content} (${attr(token, "src") ?? ""})` });
        break;
      case "hr":
        nodes.push(elementOf(token, []));
        break;
      default:
        // An unrecognized leaf keeps its text. Dropping it would lose a
        // plugin's words to a token type nobody here had heard of.
        if (token.content) nodes.push({ kind: "text", text: token.content });
        break;
    }
  }

  return nodes;
}

/**
 * One top-level block, and the source it was parsed from.
 *
 * `source` is the block's own byte range, **line terminators included**. That
 * is not fussiness: a fence whose last line has not yet ended renders its body
 * without the trailing newline and gains it the moment the newline arrives, so
 * a `source` trimmed to whole lines would call those two states equal and the
 * stream would keep the earlier render.
 */
export interface MarkdownBlock {
  readonly source: string;
  readonly nodes: MarkdownNode[];
}

/** A block plus where it ends, which only the stream needs. */
interface SplitBlock {
  readonly block: MarkdownBlock;
  /**
   * Byte offset just past the block's last line, or `null` when the parser gave
   * no line map — in which case nothing from here on may be frozen, because
   * there is no boundary to freeze at.
   */
  readonly endsAt: number | null;
}

/** Byte offset of the start of each line, plus one entry for the end of the text. */
function lineOffsets(text: string): number[] {
  const offsets = [0];
  for (let at = 0; at < text.length; at += 1) {
    if (text.charCodeAt(at) === 10) offsets.push(at + 1);
  }
  // A block's line map ends *exclusive*, and the last block's end is the end of
  // the text, which is one past the last line start.
  offsets.push(text.length);
  return offsets;
}

/**
 * Cut the token stream at depth zero.
 *
 * The walk is the same generic nesting walk {@link build} does; all it adds is
 * that a group which opens and closes at depth zero is one top-level block, and
 * every top-level token carries the `map` that names its lines.
 */
function splitBlocks(text: string, env: Record<string, unknown>): SplitBlock[] {
  const tokens = PARSER.parse(text, env);
  const offsets = lineOffsets(text);
  const blocks: SplitBlock[] = [];
  let depth = 0;
  let from = 0;
  for (let at = 0; at < tokens.length; at += 1) {
    const token = tokens[at];
    if (!token) continue;
    if (depth === 0) from = at;
    depth += token.nesting;
    if (depth !== 0) continue;
    const group = tokens.slice(from, at + 1);
    const map = group[0]?.map ?? null;
    const from_ = map ? (offsets[map[0]] ?? null) : null;
    const to = map ? (offsets[map[1]] ?? null) : null;
    blocks.push({
      block: {
        source: from_ !== null && to !== null ? text.slice(from_, to) : "",
        nodes: build(group),
      },
      endsAt: to,
    });
  }
  return blocks;
}

/** Parse a whole markdown document into its top-level blocks. */
export function parseMarkdownBlocks(text: string): MarkdownBlock[] {
  return splitBlocks(text, {}).map((split) => split.block);
}

/** Parse markdown into renderable nodes. */
export function parseMarkdown(text: string): MarkdownNode[] {
  return parseMarkdownBlocks(text).flatMap((block) => block.nodes);
}

/**
 * A document that is still arriving.
 *
 * {@link MarkdownStream.push} takes the whole buffer so far and answers with
 * every block in it, **keeping the object identity of blocks that have not
 * changed**. That identity is the point: `<For>` reuses the DOM of an item it
 * has seen before, so a delta rebuilds the one block still being written and
 * leaves every earlier paragraph, list and fence — and any selection inside
 * them — untouched.
 */
export interface MarkdownStream {
  push(text: string): MarkdownBlock[];
}

export function createMarkdownStream(): MarkdownStream {
  /** Blocks that can no longer change, in order, with the identity handed out. */
  let frozen: MarkdownBlock[] = [];
  /** The exact prefix `frozen` was parsed from; the tail is everything after it. */
  let frozenText = "";
  /** The previous answer, so a block that just froze keeps the DOM it already had. */
  let previous: MarkdownBlock[] = [];
  /**
   * Shared across pushes so a link reference definition seen in a frozen block
   * still resolves in the tail. `markdown-it` accumulates `references` here.
   */
  let env: Record<string, unknown> = {};

  return {
    push(text) {
      // Not an extension of what was frozen — a panel republished, or a
      // different document entirely — so nothing frozen applies.
      if (!text.startsWith(frozenText)) {
        frozen = [];
        frozenText = "";
        previous = [];
        env = {};
      }

      const split = splitBlocks(text.slice(frozenText.length), env);
      // Every block but the last has a successor, so it has ended: nothing
      // appended after it can reopen a block at depth zero. The last one is
      // still open by definition and is re-parsed on the next delta.
      let boundary: number | null = null;
      const freezing: MarkdownBlock[] = [];
      for (const candidate of split.slice(0, -1)) {
        if (candidate.endsAt === null) break;
        freezing.push(candidate.block);
        boundary = candidate.endsAt;
      }
      if (boundary !== null) {
        frozenText = text.slice(0, frozenText.length + boundary);
        frozen.push(...freezing);
      }

      const all = frozen.concat(split.slice(freezing.length).map((rest) => rest.block));
      // A block frozen on this push was the open tail on the last one, with the
      // same source; handing back the object already rendered keeps its DOM.
      const answer = all.map((block, at) =>
        previous[at]?.source === block.source ? previous[at]! : block,
      );
      for (let at = 0; at < frozen.length; at += 1) frozen[at] = answer[at]!;
      previous = answer;
      return answer;
    },
  };
}
