// Folds `session/update` notifications into something renderable.
//
// Pure and DOM-free so it can be tested without a browser, and so the same fold
// serves the replayed history and the live stream: `session/load` replays the
// transcript to the attaching client as the very same notifications, which is
// why there is no second code path for "history"
// (`crates/codegen/xai-grok-shell/src/agent/web_gateway.rs` module docs).
import { createStore, produce } from "solid-js/store";

import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";

import { plainText } from "./ansi.ts";
import type { ContentBlock, SessionUpdate, ToolCallStatus } from "./wire.ts";
import { textOf } from "./wire.ts";

export interface MessageEntry {
  kind: "message";
  role: "user" | "assistant" | "thought";
  text: string;
}

export interface ToolCallEntry {
  kind: "tool_call";
  toolCallId: string;
  /** The ACP `title`, kept as the fallback the pager treats it as. */
  title: string;
  /** ACP `kind`: what decides how the terminal titles and folds this call. */
  toolKind: string;
  status: ToolCallStatus;
  /** The tool's arguments, which is what a title is really derived from. */
  rawInput: unknown;
  /** The tool's typed result, which carries the counts and the clean output. */
  rawOutput: unknown;
  output: string;
}

export type TranscriptEntry = MessageEntry | ToolCallEntry;

/** A published panel, keyed the way the pager keys it: by `(plugin, id)`. */
export interface PanelEntry {
  plugin: string;
  viewModel: PanelViewModel;
}

/**
 * The composite key. Separated by NUL because every printable character is
 * legal in both a plugin name and a plugin-local panel id, so any visible
 * separator could be forged into a collision between two plugins.
 */
export function panelKey(plugin: string, id: string): string {
  return `${plugin}\u0000${id}`;
}

/**
 * Everything one attached session shows.
 *
 * Backed by a Solid store rather than plain arrays, so the components that read
 * it subscribe per field. The fold itself is unchanged from the plain version:
 * the same `sessionUpdate` tags, the same coalescing, the same unknown-tag
 * silence.
 *
 * Panels live outside the transcript because they are not events: republishing
 * the same `(plugin, id)` replaces the panel, latest wins
 * (`PanelViewModel` docs in `sdk/plugin/src/generated/PanelViewModel.ts`).
 */
export interface Transcript {
  readonly entries: readonly TranscriptEntry[];
  /** Keyed by {@link panelKey}; a closed panel is deleted, not blanked. */
  readonly panels: Readonly<Record<string, PanelEntry>>;
  apply(update: SessionUpdate): void;
}

export function createTranscript(): Transcript {
  const [entries, setEntries] = createStore<TranscriptEntry[]>([]);
  const [panels, setPanels] = createStore<Record<string, PanelEntry>>({});
  // Index into `entries`, so a `tool_call_update` reaches its entry without a
  // scan and without a second copy of the entry to keep in step.
  const toolCallAt = new Map<string, number>();

  const apply = (update: SessionUpdate): void => {
    switch (update.sessionUpdate) {
      case "user_message_chunk":
        appendMessage("user", textOf(update["content"] as ContentBlock | undefined));
        return;
      case "agent_message_chunk":
        appendMessage("assistant", textOf(update["content"] as ContentBlock | undefined));
        return;
      case "agent_thought_chunk":
        appendMessage("thought", textOf(update["content"] as ContentBlock | undefined));
        return;
      case "tool_call": {
        const call: ToolCallEntry = {
          kind: "tool_call",
          toolCallId: String(update["toolCallId"]),
          title: String(update["title"] ?? ""),
          toolKind: String(update["kind"] ?? "other"),
          status: (update["status"] as ToolCallStatus | undefined) ?? "pending",
          rawInput: update["rawInput"],
          rawOutput: update["rawOutput"],
          output: toolOutput(update["rawOutput"], update["content"]),
        };
        toolCallAt.set(call.toolCallId, entries.length);
        setEntries(entries.length, call);
        return;
      }
      case "tool_call_update": {
        const at = toolCallAt.get(String(update["toolCallId"]));
        if (at === undefined) return;
        const title = update["title"];
        const toolKind = update["kind"];
        const status = update["status"];
        const rawInput = update["rawInput"];
        const rawOutput = update["rawOutput"];
        const content = update["content"];
        const output = toolOutput(rawOutput, content);
        // One `produce` per update, so a status change touches only the nodes
        // bound to `status` and leaves the output text node alone.
        setEntries(
          at,
          produce((entry) => {
            if (entry.kind !== "tool_call") return;
            if (typeof title === "string") entry.title = title;
            if (typeof toolKind === "string") entry.toolKind = toolKind;
            if (typeof status === "string") entry.status = status as ToolCallStatus;
            // A later frame carrying no `rawInput` is not a frame clearing it:
            // the agent sends the fields that changed, and the title is derived
            // from arguments that were only ever sent once.
            if (rawInput !== undefined) entry.rawInput = rawInput;
            if (rawOutput !== undefined) entry.rawOutput = rawOutput;
            // A frame that carries an output field is authoritative about the
            // output, *including* when it says there was none. Only writing
            // non-empty text left the placeholder the first frame carried —
            // a shell call's description — standing as the output of a command
            // that printed nothing.
            if (rawOutput !== undefined || content !== undefined) entry.output = output;
          }),
        );
        return;
      }
      case "plugin_panel": {
        const plugin = String(update["plugin"]);
        const viewModel = update["view_model"] as PanelViewModel;
        setPanels(panelKey(plugin, viewModel.id), { plugin, viewModel });
        return;
      }
      case "panel_closed": {
        setPanels(panelKey(String(update["plugin"]), String(update["id"])), undefined!);
        return;
      }
      default:
        // Unknown tags are dropped rather than guessed at. They are still on the
        // socket for the next person who needs one; nothing here rewrites them.
        return;
    }
  }

  /**
   * Chunks of the same role coalesce into one message.
   *
   * The agent streams a reply as many `agent_message_chunk`s; appending each as
   * its own entry would render one paragraph per token. Growing the existing
   * entry's `text` is also the whole reason this client is fine-grained: the
   * store updates one text node, where a virtual DOM would reconcile the entire
   * transcript on every chunk of a streaming reply.
   */
  const appendMessage = (role: MessageEntry["role"], text: string): void => {
    if (!text) return;
    const at = entries.length - 1;
    const last = entries[at];
    if (last && last.kind === "message" && last.role === role) {
      setEntries(
        at,
        produce((entry) => {
          if (entry.kind === "message") entry.text += text;
        }),
      );
      return;
    }
    setEntries(entries.length, { kind: "message", role, text });
  };

  return { entries, panels, apply };
}

/**
 * The text a tool call shows.
 *
 * `content` is the raw one — the shell puts the untouched PTY stream on it
 * (`session/acp_conversion.rs`, `tools/notification_bridge.rs`) — and it is the
 * one used, because it is the only channel that still has everything a
 * terminal needs. The obvious alternative loses: `ToolOutput::Bash` also
 * carries `output_for_prompt`, which the shell built by stripping ANSI for the
 * model, and `strip_str` takes the `CSI K` **out** while leaving the `\r` in.
 * A `cargo build` therefore arrives on that channel with its progress bar
 * smeared across the line it was meant to erase, and no client can put it back:
 * the erase is gone. Read live, the two channels for one build:
 *
 *     output_for_prompt  "…Building [====>] 2/4: libc      Compiling ansidemo…"
 *     content            "…Building [====>] 2/4: libc  \r\x1b[K…Compiling ansidemo…"
 *
 * So the raw channel is the better source, `plainText` is what makes it
 * readable, and `output_for_prompt` is a fallback for a call that sent no
 * content at all.
 */
function toolOutput(rawOutput: unknown, content: unknown): string {
  const raw =
    typeof rawOutput === "object" && rawOutput !== null
      ? (rawOutput as Record<string, unknown>)
      : {};

  if (raw["type"] === "Bash") {
    // The typed bytes, which is where the pager reads a shell call's output
    // from as well (`extract_bash_output_from_value`, `tracker.rs`) — and it has
    // to be read from here, because `content` on the *first* frame of a shell
    // call is the description rather than any output at all.
    if (Array.isArray(raw["output"])) {
      return plainText(new TextDecoder().decode(Uint8Array.from(raw["output"] as number[])));
    }
    if (typeof raw["output_for_prompt"] === "string") {
      // The `exit: N` line the shell prefixes for the model is not terminal
      // output, and the status this client draws already says it.
      return plainText(raw["output_for_prompt"].replace(/^exit: -?\d+\n/, ""));
    }
  }

  const parts: string[] = [];
  if (Array.isArray(content)) {
    for (const item of content) {
      if (typeof item !== "object" || item === null) continue;
      const inner = (item as { content?: unknown }).content;
      if (typeof inner !== "object" || inner === null) continue;
      const block = inner as { type?: string; text?: string };
      if (block.type === "text" && typeof block.text === "string") parts.push(block.text);
    }
  }
  return plainText(parts.join(""));
}
