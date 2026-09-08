// A turn arrives in pieces, and the page must not flicker while it does.
//
// The invariant every test here defends is one sentence: **a stream must draw
// what a finished document draws.** The pager gets that for free — its frozen
// prefix is output it already rendered — and this client has to prove it,
// because its freeze boundary is derived from a different parser's token
// stream than `pulldown-cmark`'s checkpoints.
import { describe, expect, test } from "bun:test";

import {
  createMarkdownStream,
  parseMarkdown,
  parseMarkdownBlocks,
  type MarkdownBlock,
} from "../src/markdown.ts";

const RUST = await Bun.file(
  new URL("../../../crates/codegen/xai-grok-markdown/src/checkpoint.rs", import.meta.url),
).text();

/** Feed a document in fixed-size chunks and answer with every intermediate result. */
function stream(text: string, chunk: number): MarkdownBlock[][] {
  const pushes: MarkdownBlock[][] = [];
  const md = createMarkdownStream();
  for (let at = chunk; at < text.length + chunk; at += chunk) {
    pushes.push(md.push(text.slice(0, Math.min(at, text.length))));
  }
  return pushes;
}

const flat = (blocks: readonly MarkdownBlock[]) => blocks.flatMap((block) => block.nodes);

const TURN = `# A model turn

It opens with a paragraph that has **bold**, \`code\` and a
[link](https://example.com) in it.

- one item
- two items

  with a second paragraph, so the list is loose

\`\`\`ts
export const answer = 42;
\`\`\`

> and a quote to finish
> across two lines

| col | col |
| --- | --- |
| a   | b   |
`;

describe("the freeze boundary is the pager's", () => {
  test("the boundary this client freezes on is the one `checkpoint.rs` names", () => {
    // Not a paraphrase of the rule but the rule itself: if the pager ever
    // freezes somewhere other than a top-level block boundary, this stops being
    // the same mechanism and starts being a lookalike.
    expect(RUST).toContain("Checkpoints are only created at **top-level** (depth=0) block boundaries");
    expect(RUST).toContain("cannot be checkpoints because the outer container might continue");
  });

  test("every prefix of a stream renders exactly what a one-shot parse renders", () => {
    for (const chunk of [1, 3, 17, 64]) {
      const pushes = stream(TURN, chunk);
      for (const [at, blocks] of pushes.entries()) {
        const prefix = TURN.slice(0, Math.min((at + 1) * chunk, TURN.length));
        expect(flat(blocks)).toEqual(parseMarkdown(prefix));
      }
    }
  });

  test("a block that has stopped changing keeps the object it was drawn from", () => {
    // The whole point. `<For>` reuses an item's DOM only when the item is the
    // same object, so identity here is what stops a streaming reply from
    // rebuilding — and reflowing, and deselecting — everything above the line
    // still being written.
    const pushes = stream(TURN, 16);
    const last = pushes.at(-1)!;
    const settled = pushes.at(-2)!;
    for (const [at, block] of settled.slice(0, -1).entries()) {
      expect(last[at]).toBe(block);
    }
  });

  test("the work a delta costs does not grow with the turn already on screen", () => {
    // Two blocks at most per delta, and never a number that climbs: the block
    // still being written, plus at most the one that closed in the same delta
    // and so was drawn once more with its final line. Reparsing the buffer whole
    // would put every block in this count on every chunk, which is the
    // quadratic the freeze exists to remove.
    const pushes = stream(TURN.repeat(4), 8);
    let total = 0;
    for (let at = 1; at < pushes.length; at += 1) {
      const before = new Set(pushes[at - 1]!);
      const rebuilt = pushes[at]!.filter((block) => !before.has(block));
      expect(rebuilt.length).toBeLessThanOrEqual(2);
      total += rebuilt.length;
    }
    const blocks = pushes.at(-1)!.length;
    expect(blocks).toBeGreaterThan(20);
    expect(total).toBeLessThan(pushes.length + blocks);
  });
});

describe("what a half-written document must not do", () => {
  test("an open fence is already a code block, and grows instead of reflowing", () => {
    const md = createMarkdownStream();
    md.push("text\n\n```ts\nconst a");
    const open = md.push("text\n\n```ts\nconst a = 1;\n");
    expect(open.at(-1)!.nodes).toEqual([{ kind: "code", text: "const a = 1;\n", language: "ts" }]);
    // And it is still the tail: a fence with no closing line can still turn out
    // to hold more, so freezing it would be freezing a guess.
    expect(md.push("text\n\n```ts\nconst a = 1;\nconst b = 2;\n").at(-1)!.nodes).toEqual([
      { kind: "code", text: "const a = 1;\nconst b = 2;\n", language: "ts" },
    ]);
  });

  test("a list that continues after a blank line stays one list", () => {
    // The case that makes a naive blank-line split wrong: the blank line does
    // not end the list, it makes it loose. `checkpoint.rs` refuses to freeze
    // inside a container for exactly this reason.
    const md = createMarkdownStream();
    md.push("- one\n\n");
    const blocks = md.push("- one\n\n- two\n");
    expect(blocks).toHaveLength(1);
    expect(blocks[0]!.nodes).toEqual(parseMarkdown("- one\n\n- two\n"));
  });

  test("a paragraph that turns into a setext heading is never frozen as a paragraph", () => {
    const md = createMarkdownStream();
    md.push("Title\n");
    const blocks = md.push("Title\n=====\n");
    expect(flat(blocks)).toEqual(parseMarkdown("Title\n=====\n"));
  });

  test("a table's header row is not frozen as the paragraph it looks like alone", () => {
    const md = createMarkdownStream();
    md.push("intro\n\n| a | b |\n");
    const blocks = md.push("intro\n\n| a | b |\n| - | - |\n| 1 | 2 |\n");
    expect(flat(blocks)).toEqual(parseMarkdown("intro\n\n| a | b |\n| - | - |\n| 1 | 2 |\n"));
  });

  test("a link definition in a frozen block still resolves in the tail", () => {
    // The parser `env` is shared across pushes precisely so this holds. The
    // reverse — a definition arriving after its use — cannot be repaired by any
    // frozen prefix, in this client or in the pager.
    const md = createMarkdownStream();
    md.push("[docs]: https://example.com\n\n");
    const blocks = md.push("[docs]: https://example.com\n\nsee [docs]\n");
    expect(flat(blocks)).toEqual(parseMarkdown("[docs]: https://example.com\n\nsee [docs]\n"));
    expect(JSON.stringify(flat(blocks))).toContain("https://example.com");
  });

  test("text that is not an extension of what was frozen starts over", () => {
    // A panel republishes whole documents rather than appending to one, and the
    // same component instance sees both.
    const md = createMarkdownStream();
    md.push("# first\n\nbody\n");
    const blocks = md.push("# second\n\nother body\n");
    expect(flat(blocks)).toEqual(parseMarkdown("# second\n\nother body\n"));
  });
});

describe("blocks and nodes say the same thing", () => {
  test("the blocks of a document concatenate to its nodes", () => {
    expect(parseMarkdownBlocks(TURN).flatMap((block) => block.nodes)).toEqual(parseMarkdown(TURN));
  });

  test("each block's source is the lines it was parsed from", () => {
    for (const block of parseMarkdownBlocks(TURN)) {
      expect(TURN).toContain(block.source);
      expect(parseMarkdown(block.source)).toEqual(block.nodes);
    }
  });
});
