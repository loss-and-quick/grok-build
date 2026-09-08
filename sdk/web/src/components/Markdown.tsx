import { For, Match, Switch, createMemo, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";

import { createMarkdownStream, type MarkdownNode } from "../markdown.ts";

/**
 * One node, and its children.
 *
 * `Dynamic` takes the tag from the node, and the node's tag can only be one of
 * `MARKDOWN_TAGS` — the parser drops the wrapper for anything else — so the
 * element names this component can build are a list somebody reviewed, not
 * whatever a plugin's text happened to name.
 */
function Node(props: { node: MarkdownNode }): JSX.Element {
  return (
    <Switch>
      <Match when={props.node.kind === "text" ? props.node : null}>{(n) => n().text}</Match>
      <Match when={props.node.kind === "break"}>
        <br />
      </Match>
      <Match when={props.node.kind === "inline_code" ? props.node : null}>
        {(n) => <code class="grok-md-inline-code">{n().text}</code>}
      </Match>
      <Match when={props.node.kind === "code" ? props.node : null}>
        {(n) => (
          // The language is carried on the element rather than used to
          // highlight: the terminal highlights with syntect against its own
          // theme, and a second highlighter here would be a second opinion
          // about the same code.
          <pre class="grok-md-code" data-language={n().language ?? undefined}>
            {n().text}
          </pre>
        )}
      </Match>
      <Match when={props.node.kind === "element" ? props.node : null}>
        {(n) => (
          <Dynamic
            component={n().tag}
            class={`grok-md-${n().tag}`}
            // A link whose scheme was refused keeps its text and loses its
            // href; `safeHref` already made that decision.
            href={n().tag === "a" ? (n().href ?? undefined) : undefined}
            target={n().tag === "a" && n().href ? "_blank" : undefined}
            rel={n().tag === "a" && n().href ? "noreferrer noopener" : undefined}
            start={n().start ?? undefined}
          >
            <Nodes nodes={n().children} />
          </Dynamic>
        )}
      </Match>
    </Switch>
  );
}

function Nodes(props: { nodes: MarkdownNode[] }): JSX.Element {
  return <For each={props.nodes}>{(node) => <Node node={node} />}</For>;
}

/**
 * A markdown document, whether finished or still arriving.
 *
 * One component for both, because the two must not diverge in what they draw:
 * the stream's frozen prefix is the same parse a finished document gets, so a
 * turn looks the same mid-flight as it does when it lands. `<For>` over the
 * blocks is what makes the reuse visible to the renderer — an unchanged block
 * is the same object, so its DOM is not rebuilt.
 */
export function Markdown(props: { text: string }): JSX.Element {
  const stream = createMarkdownStream();
  const blocks = createMemo(() => stream.push(props.text));
  return <For each={blocks()}>{(block) => <Nodes nodes={block.nodes} />}</For>;
}
