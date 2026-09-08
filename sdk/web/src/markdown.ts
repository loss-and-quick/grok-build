// Panel markdown, parsed by a real parser and handed on as data.
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

/** Parse panel markdown into renderable nodes. */
export function parseMarkdown(text: string): MarkdownNode[] {
  return build(PARSER.parse(text, {}));
}
