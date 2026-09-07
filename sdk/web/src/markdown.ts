// The panel markdown parser, as data.
//
// Parsing and rendering are split so the rule that matters — which links are
// allowed to keep an `href` — is decided here, in a pure function a test can
// interrogate, rather than inside a component. Panel text is plugin-authored
// and reaches the page as markup input; `javascript:` in an href is script
// execution in this page's origin.
//
// Deliberately small. The pager runs a real markdown parser; matching it is a
// later job and a shared one, since duplicating a renderer per client is
// exactly the drift this client exists to argue against.

export type MarkdownSpan =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; text: string }
  /** `href` is `null` when the target was not an allowed scheme; render as plain text. */
  | { kind: "link"; text: string; href: string | null };

export type MarkdownBlock =
  | { kind: "code"; text: string }
  | { kind: "heading"; level: number; spans: MarkdownSpan[] }
  | { kind: "paragraph"; spans: MarkdownSpan[] };

/** Only these schemes keep an `href`. Everything else renders as text. */
const ALLOWED_HREF = /^https?:\/\//i;

const INLINE = /`([^`]+)`|\*\*([^*]+)\*\*|\[([^\]]+)\]\(([^)\s]+)\)/g;

export function parseInline(text: string): MarkdownSpan[] {
  const spans: MarkdownSpan[] = [];
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    const at = m.index;
    if (at > last) spans.push({ kind: "text", text: text.slice(last, at) });
    if (m[1] !== undefined) {
      spans.push({ kind: "code", text: m[1] });
    } else if (m[2] !== undefined) {
      spans.push({ kind: "strong", text: m[2] });
    } else if (m[3] !== undefined && m[4] !== undefined) {
      spans.push({ kind: "link", text: m[3], href: ALLOWED_HREF.test(m[4]) ? m[4] : null });
    }
    last = at + m[0].length;
  }
  if (last < text.length) spans.push({ kind: "text", text: text.slice(last) });
  return spans;
}

export function parseMarkdown(text: string): MarkdownBlock[] {
  const blocks: MarkdownBlock[] = [];
  const lines = text.split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? "";
    if (line.startsWith("```")) {
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !(lines[i] ?? "").startsWith("```")) {
        body.push(lines[i] ?? "");
        i += 1;
      }
      i += 1;
      blocks.push({ kind: "code", text: body.join("\n") });
      continue;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      blocks.push({
        kind: "heading",
        level: (heading[1] ?? "#").length,
        spans: parseInline(heading[2] ?? ""),
      });
      i += 1;
      continue;
    }
    if (line.trim() === "") {
      i += 1;
      continue;
    }
    blocks.push({ kind: "paragraph", spans: parseInline(line) });
    i += 1;
  }
  return blocks;
}
