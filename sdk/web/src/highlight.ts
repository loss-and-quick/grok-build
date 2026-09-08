// Matched-character runs, for every list in this client that highlights a query.
//
// One helper, two callers, and the difference between them is the point. The
// slash menu computes its own indices, because the command catalog on the wire
// carries none (`commands.ts`, `matchIndices`). The file search computes
// nothing: `x.ai/search/fuzzy/status` carries `indices` for every match
// (`xai-fuzzy-file-search/src/lib.rs`, the `Serialize` impl), because the agent
// ran the matcher. What the two share is only what is left after that — turning
// a set of positions into spans — which the pager also shares between them
// (`build_highlighted_spans`).

/** A stretch of text that is either all matched or all unmatched. */
export interface HighlightRun {
  text: string;
  match: boolean;
}

/**
 * Split `text` into runs of matched and unmatched characters.
 *
 * `indices` are **character** positions, not UTF-16 code units: nucleo scores a
 * `Utf32String` and reports offsets into it (`FuzzyMatchResult.indices`, "matched
 * indices of characters"), and the pager consumes them against
 * `char_indices().enumerate()`. So the walk is over code points, and a path with
 * an astral character stays highlighted on the right characters instead of
 * halfway through a surrogate pair.
 *
 * Runs are coalesced rather than emitted one span per character, which is what
 * the pager does and what keeps a thousand-row list from becoming tens of
 * thousands of DOM nodes.
 */
export function highlightRuns(text: string, indices: readonly number[]): HighlightRun[] {
  const marked = new Set(indices);
  const out: HighlightRun[] = [];
  for (const [at, character] of [...text].entries()) {
    const match = marked.has(at);
    const last = out[out.length - 1];
    if (last && last.match === match) last.text += character;
    else out.push({ text: character, match });
  }
  return out;
}
