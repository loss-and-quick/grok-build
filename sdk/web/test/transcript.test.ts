import { describe, expect, test } from "bun:test";

import {
  createTranscript,
  panelKey,
  type MessageEntry,
  type ToolCallEntry,
} from "../src/transcript.ts";
import type { SessionUpdate } from "../src/wire.ts";

function chunk(role: "agent" | "user" | "agent_thought", text: string): SessionUpdate {
  const tag =
    role === "agent"
      ? "agent_message_chunk"
      : role === "user"
        ? "user_message_chunk"
        : "agent_thought_chunk";
  return { sessionUpdate: tag, content: { type: "text", text } } as SessionUpdate;
}

describe("session update fold", () => {
  test("consecutive chunks of one role become one message", () => {
    const t = createTranscript();
    t.apply(chunk("agent", "Hel"));
    t.apply(chunk("agent", "lo"));
    expect(t.entries).toHaveLength(1);
    expect((t.entries[0] as MessageEntry).text).toBe("Hello");
  });

  test("a change of role starts a new message", () => {
    const t = createTranscript();
    t.apply(chunk("user", "hi"));
    t.apply(chunk("agent_thought", "thinking"));
    t.apply(chunk("agent", "hello"));
    expect(t.entries.map((e) => (e as MessageEntry).role)).toEqual([
      "user",
      "thought",
      "assistant",
    ]);
  });

  test("a tool call is updated in place, not appended twice", () => {
    const t = createTranscript();
    t.apply({
      sessionUpdate: "tool_call",
      toolCallId: "tc-1",
      title: "Read main.rs",
      status: "pending",
    } as SessionUpdate);
    t.apply({
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-1",
      status: "completed",
      content: [{ type: "content", content: { type: "text", text: "fn main() {}" } }],
    } as SessionUpdate);
    expect(t.entries).toHaveLength(1);
    const call = t.entries[0] as ToolCallEntry;
    expect(call.status).toBe("completed");
    expect(call.output).toBe("fn main() {}");
    expect(call.title).toBe("Read main.rs");
  });

  test("a tool call keeps the arguments it was announced with", () => {
    // A `tool_call_update` carries what changed. The title is derived from
    // arguments that arrive once, so a later frame without them must not be
    // read as a frame clearing them.
    const t = createTranscript();
    t.apply({
      sessionUpdate: "tool_call",
      toolCallId: "tc-1",
      title: "Execute `cargo build`",
      kind: "execute",
      status: "pending",
      rawInput: { command: "cargo build" },
    } as SessionUpdate);
    t.apply({
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-1",
      status: "completed",
    } as SessionUpdate);
    const call = t.entries[0] as ToolCallEntry;
    expect(call.toolKind).toBe("execute");
    expect(call.rawInput).toEqual({ command: "cargo build" });
  });

  test("output comes off the raw bytes, which are the only ones still able to erase", () => {
    // All three channels as they arrive from a real `cargo build`. The stripped
    // copy lost its `CSI K` and kept its `\r`, so its progress bar is smeared
    // across the line it was meant to erase and nothing downstream can undo it.
    const bar = "    Building [==>  ] 2/4: libc                        ";
    const raw = `\x1b[1m\x1b[96m${bar}\x1b[0m\r\x1b[K\x1b[92m   Compiling\x1b[0m ansidemo\n`;
    const t = createTranscript();
    t.apply({
      sessionUpdate: "tool_call",
      toolCallId: "tc-1",
      title: "Execute",
      kind: "execute",
      status: "completed",
      rawOutput: {
        type: "Bash",
        exit_code: 0,
        output: [...new TextEncoder().encode(raw)],
        output_for_prompt: `exit: 0\n${bar}   Compiling ansidemo\n`,
      },
      content: [{ type: "content", content: { type: "text", text: raw } }],
    } as SessionUpdate);
    expect((t.entries[0] as ToolCallEntry).output).toBe("   Compiling ansidemo");
  });

  test("with no bytes at all, the stripped copy is better than nothing", () => {
    const t = createTranscript();
    t.apply({
      sessionUpdate: "tool_call",
      toolCallId: "tc-1",
      title: "Execute",
      kind: "execute",
      status: "completed",
      rawOutput: { type: "Bash", exit_code: 0, output_for_prompt: "exit: 0\nhello\n" },
    } as SessionUpdate);
    // Minus the `exit:` line, which is framing written for the model and is
    // already said by the status this client draws.
    expect((t.entries[0] as ToolCallEntry).output).toBe("hello");
  });

  test("a command that printed nothing shows nothing, not the description", () => {
    // Live defect: the first frame of a shell call carries its description on
    // `content`, and `exit 3` printed nothing — so an update that only ever
    // wrote non-empty output left the description standing as the command's
    // output.
    const t = createTranscript();
    t.apply({
      sessionUpdate: "tool_call",
      toolCallId: "tc-1",
      title: "Execute `exit 3`",
      kind: "execute",
      status: "pending",
      rawInput: { command: "exit 3", description: "Exit with status 3." },
      content: [{ type: "content", content: { type: "text", text: "Exit with status 3." } }],
    } as SessionUpdate);
    t.apply({
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-1",
      status: "completed",
      rawOutput: { type: "Bash", exit_code: 3, output: [], output_for_prompt: "exit: 3\n" },
      content: [{ type: "content", content: { type: "text", text: "" } }],
    } as SessionUpdate);
    expect((t.entries[0] as ToolCallEntry).output).toBe("");
  });

  test("an update for an unknown tool call is dropped, not synthesized", () => {
    const t = createTranscript();
    t.apply({ sessionUpdate: "tool_call_update", toolCallId: "ghost" } as SessionUpdate);
    expect(t.entries).toHaveLength(0);
  });

  test("panels are keyed by (plugin, id), because ids are only plugin-local", () => {
    const t = createTranscript();
    const vm = { id: "p", title: "A", blocks: [] };
    t.apply({ sessionUpdate: "plugin_panel", plugin: "one", view_model: vm } as SessionUpdate);
    t.apply({
      sessionUpdate: "plugin_panel",
      plugin: "two",
      view_model: { ...vm, title: "B" },
    } as SessionUpdate);
    expect(Object.keys(t.panels)).toHaveLength(2);
    expect(t.panels[panelKey("one", "p")]?.viewModel.title).toBe("A");
    expect(t.panels[panelKey("two", "p")]?.viewModel.title).toBe("B");
  });

  test("re-publishing one id replaces that panel, latest wins", () => {
    const t = createTranscript();
    t.apply({
      sessionUpdate: "plugin_panel",
      plugin: "one",
      view_model: { id: "p", title: "first", blocks: [] },
    } as SessionUpdate);
    t.apply({
      sessionUpdate: "plugin_panel",
      plugin: "one",
      view_model: { id: "p", title: "second", blocks: [] },
    } as SessionUpdate);
    expect(Object.keys(t.panels)).toHaveLength(1);
    expect(t.panels[panelKey("one", "p")]?.viewModel.title).toBe("second");
  });

  test("panel_closed removes only its own (plugin, id)", () => {
    const t = createTranscript();
    for (const plugin of ["one", "two"]) {
      t.apply({
        sessionUpdate: "plugin_panel",
        plugin,
        view_model: { id: "p", title: plugin, blocks: [] },
      } as SessionUpdate);
    }
    t.apply({ sessionUpdate: "panel_closed", plugin: "one", id: "p" } as SessionUpdate);
    expect(Object.keys(t.panels)).toEqual([panelKey("two", "p")]);
  });

  test("an unknown sessionUpdate tag is ignored rather than guessed at", () => {
    const t = createTranscript();
    t.apply({ sessionUpdate: "something_new_next_year", payload: 1 } as SessionUpdate);
    expect(t.entries).toHaveLength(0);
    expect(Object.keys(t.panels)).toHaveLength(0);
  });

  test("an empty chunk does not open an empty message", () => {
    const t = createTranscript();
    t.apply(chunk("agent", ""));
    expect(t.entries).toHaveLength(0);
  });
});
