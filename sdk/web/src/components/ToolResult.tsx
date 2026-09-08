import { For, Show, Switch, Match, type JSX } from "solid-js";

import { ellipsisFor, truncationFor, type DisplayMode } from "../toolcall.ts";
import {
  gutterWidth,
  hunkSeparator,
  resultPath,
  type DiffHunk,
  type DiffResult,
  type ReadResult,
  type SearchResult,
  type TypedResult,
} from "../toolresult.ts";

/**
 * A tool call's result, drawn from the typed `rawOutput` rather than from prose.
 *
 * Every layout decision below is the pager's, cited where it is made; the ones
 * that are not are marked as this client's and given a reason. Nothing here
 * builds markup from a string — a hit line, a file's text and a diff row are
 * all arbitrary bytes from someone's disk, and they reach the page as text
 * nodes for the same reason panel markdown does.
 */
export function ToolResult(props: {
  result: TypedResult;
  cwd: string;
  mode: DisplayMode;
}): JSX.Element {
  return (
    <Switch>
      <Match when={props.result.kind === "read" ? (props.result as ReadResult) : null}>
        {(read) => <ReadBody read={read()} mode={props.mode} />}
      </Match>
      <Match when={props.result.kind === "search" ? (props.result as SearchResult) : null}>
        {(search) => <SearchBody search={search()} cwd={props.cwd} />}
      </Match>
      <Match when={props.result.kind === "diff" ? (props.result as DiffResult) : null}>
        {(diff) => <DiffBody diff={diff()} />}
      </Match>
    </Switch>
  );
}

/**
 * A read, with the file's own line numbers down the left.
 *
 * `render_content_lines` (`blocks/tool/read.rs`): the number right-aligned to
 * the width of the largest one shown, two spaces, then the line. The panel is
 * `bg_dark`, the gutter is `Theme::dim` and the text `Theme::primary` — the
 * pager uses those two helpers rather than the raw gray so `terminal-native`
 * reaches SGR dim instead of a colour it does not have, which is exactly what
 * `--grok-muted-opacity` reproduces here.
 *
 * The head-and-tail elision is the same 5 and 3 the prose body used, so
 * pressing the row still walks the pager's own two states. What is new is that
 * the numbers now say what the `…` is hiding.
 */
function ReadBody(props: { read: ReadResult; mode: DisplayMode }): JSX.Element {
  const bounds = truncationFor("read") ?? { first: 0, last: 0 };
  const width = (): number =>
    String(props.read.base + Math.max(0, props.read.lines.length - 1)).length;
  const rows = (): { at: number; text: string }[] =>
    props.read.lines.map((text, at) => ({ at, text }));
  const folded = (): boolean =>
    props.mode === "truncated" && props.read.lines.length > bounds.first + bounds.last;
  const head = (): { at: number; text: string }[] =>
    folded() ? rows().slice(0, bounds.first) : rows();
  const tail = (): { at: number; text: string }[] =>
    folded() ? rows().slice(rows().length - bounds.last) : [];

  return (
    <div class="tool-panel tool-read">
      <For each={head()}>
        {(row) => <ReadRow row={row} base={props.read.base} width={width()} />}
      </For>
      {/* Bare, no count — the pager's read marker carries none
          (`ellipsisFor`), and with the numbers in the gutter the gap states
          itself: the row above the marker and the row below it are both
          labelled. */}
      <Show when={folded()}>
        <div class="tool-fold-note">{ellipsisFor("read", 0)}</div>
      </Show>
      <For each={tail()}>
        {(row) => <ReadRow row={row} base={props.read.base} width={width()} />}
      </For>
    </div>
  );
}

function ReadRow(props: {
  row: { at: number; text: string };
  base: number;
  width: number;
}): JSX.Element {
  return (
    <div class="tool-row">
      <span class="tool-gutter" style={{ "min-width": `${props.width}ch` }}>
        {props.base + props.row.at}
      </span>
      <span class="tool-line">{props.row.text}</span>
    </div>
  );
}

/**
 * A search, as the hits it found.
 *
 * `SearchToolCallBlock::output` (`blocks/tool/search.rs`): a metadata line, a
 * blank, then one group per file — the path in the `path` role, and under it
 * each hit as a right-aligned line number in the muted role, two spaces, and
 * the line. `(no results)` when there were none, which is the second half of
 * the `(no matches)` the header already carries.
 *
 * The pager prints a hit's path exactly as the tool reported it, absolute and
 * with the `/./` a scope of `.` leaves in. This client relativises it against
 * the session root, which is what its titles already do and the only root a
 * browser can honestly measure against.
 */
function SearchBody(props: { search: SearchResult; cwd: string }): JSX.Element {
  const meta = (): { key: string; value: string }[] => {
    const parts: { key: string; value: string }[] = [
      { key: "mode: ", value: props.search.meta.mode },
    ];
    if (props.search.meta.fileType) parts.push({ key: "type: ", value: props.search.meta.fileType });
    if (props.search.meta.caseInsensitive) parts.push({ key: "case-insensitive: ", value: "true" });
    if (props.search.meta.multiline) parts.push({ key: "multiline: ", value: "true" });
    return parts;
  };
  const empty = (): boolean =>
    props.search.files.length === 0 && props.search.paths.length === 0;

  return (
    <div class="tool-search">
      <div class="tool-search-meta">
        <For each={meta()}>
          {(part, at) => (
            <>
              <Show when={at() > 0}>
                <span class="tool-meta-key">, </span>
              </Show>
              <span class="tool-meta-key">{part.key}</span>
              <span class="tool-meta-value">{part.value}</span>
            </>
          )}
        </For>
      </div>
      <Show when={empty() && props.search.matchCount === 0}>
        <div class="tool-fold-note">(no results)</div>
      </Show>
      <For each={props.search.files}>
        {(file) => (
          <div class="tool-panel tool-hits">
            <div class="tool-hit-path">{resultPath(props.cwd, file.path)}</div>
            <For each={file.matches}>
              {(hit) => (
                <div class="tool-row">
                  {/* Width four, the pager's own fixed field (`{:>4}`), rather
                      than one sized to the widest hit: its groups are laid out
                      independently and a shared column is what keeps them
                      reading as one list. */}
                  <span class="tool-gutter tool-hit-line">{hit.line}</span>
                  <span class="tool-line">{hit.text}</span>
                </div>
              )}
            </For>
          </div>
        )}
      </For>
      <Show when={props.search.paths.length > 0}>
        <div class="tool-panel tool-hits">
          <For each={props.search.paths}>
            {(path) => <div class="tool-hit-path">{resultPath(props.cwd, path)}</div>}
          </For>
        </div>
      </Show>
      <Show when={props.search.hidden > 0}>
        <div class="tool-fold-note">
          {"…"} {props.search.hidden} more not shown
        </div>
      </Show>
    </div>
  );
}

/**
 * An edit, as the diff the terminal draws.
 *
 * `render_diff_hunks_core` (`blocks/tool/edit.rs`) with its default
 * `DiffRenderConfig`: a two-space indent, one line-number column, two spaces,
 * then the row. The number is the new-file line for an equal or inserted row
 * and the old-file line for a deleted one, and there is deliberately no `+`/`-`
 * glyph — the sign is carried by the gutter's colour and the row's band, which
 * is why the band and the gutter must both be there.
 *
 * Between hunks the separator counts what it skipped: `… 12 unchanged lines`.
 */
function DiffBody(props: { diff: DiffResult }): JSX.Element {
  return (
    <div class="tool-diff">
      <For each={props.diff.hunks}>
        {(hunk, at) => (
          <>
            <Show when={at() > 0}>
              <div class="tool-fold-note">
                {hunkSeparator(props.diff.hunks[at() - 1]!, hunk)}
              </div>
            </Show>
            <Hunk hunk={hunk} />
          </>
        )}
      </For>
      <Show when={props.diff.hidden > 0}>
        <div class="tool-fold-note">
          {"…"} {props.diff.hidden} more lines
        </div>
      </Show>
    </div>
  );
}

function Hunk(props: { hunk: DiffHunk }): JSX.Element {
  const width = (): number => gutterWidth(props.hunk);
  return (
    <For each={props.hunk}>
      {(line) => (
        <div class={`tool-row tool-diff-${line.tag}`}>
          <span class="tool-gutter" style={{ "min-width": `${width()}ch` }}>
            {line.tag === "delete" ? line.lo : line.ln}
          </span>
          {/* A blank changed line still has to show its band, so an empty row
              paints a space — the pager's `painted` helper, same reason. */}
          <span class="tool-line">{line.text.replace(/\r?\n$/, "") || " "}</span>
        </div>
      )}
    </For>
  );
}
