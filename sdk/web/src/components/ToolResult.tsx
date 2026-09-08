import { For, Match, Show, Switch, type JSX } from "solid-js";

import { ellipsisFor, truncationFor, type DisplayMode } from "../toolcall.ts";
import type { ReadResult, TypedResult } from "../toolresult.ts";

/**
 * A tool call's result, drawn from the typed `rawOutput` rather than from prose.
 *
 * Every layout decision below is the pager's, cited where it is made; the ones
 * that are not are marked as this client's and given a reason. Nothing here
 * builds markup from a string — a file's text is arbitrary bytes from someone's
 * disk, and it reaches the page as text nodes for the same reason panel
 * markdown does.
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
