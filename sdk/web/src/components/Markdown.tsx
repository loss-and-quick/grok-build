import { For, Match, Switch, type JSX } from "solid-js";

import { parseMarkdown, type MarkdownSpan } from "../markdown.ts";

function Spans(props: { spans: MarkdownSpan[] }): JSX.Element {
  return (
    <For each={props.spans}>
      {(span) => (
        <Switch>
          <Match when={span.kind === "text" ? span : null}>{(s) => s().text}</Match>
          <Match when={span.kind === "code" ? span : null}>
            {(s) => <code class="grok-md-inline-code">{s().text}</code>}
          </Match>
          <Match when={span.kind === "strong" ? span : null}>
            {(s) => <strong>{s().text}</strong>}
          </Match>
          <Match when={span.kind === "link" ? span : null}>
            {(s) => (
              // A link whose scheme was refused keeps its text and loses its
              // href; `parseInline` already made that decision.
              <a
                class="grok-md-link"
                href={s().href ?? undefined}
                target={s().href ? "_blank" : undefined}
                rel={s().href ? "noreferrer noopener" : undefined}
              >
                {s().text}
              </a>
            )}
          </Match>
        </Switch>
      )}
    </For>
  );
}

export function Markdown(props: { text: string }): JSX.Element {
  return (
    <For each={parseMarkdown(props.text)}>
      {(block) => (
        <Switch>
          <Match when={block.kind === "code" ? block : null}>
            {(b) => <pre class="grok-md-code">{b().text}</pre>}
          </Match>
          <Match when={block.kind === "heading" ? block : null}>
            {(b) => (
              <div class={`grok-md-h grok-md-h${b().level}`}>
                <Spans spans={b().spans} />
              </div>
            )}
          </Match>
          <Match when={block.kind === "paragraph" ? block : null}>
            {(b) => (
              <p class="grok-md-p">
                <Spans spans={b().spans} />
              </p>
            )}
          </Match>
        </Switch>
      )}
    </For>
  );
}
