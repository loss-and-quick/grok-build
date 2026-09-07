import { describe, expect, test } from "bun:test";

import { panelKey, Transcript, type MessageEntry, type ToolCallEntry } from "../src/transcript.ts";
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
    const t = new Transcript();
    t.apply(chunk("agent", "Hel"));
    t.apply(chunk("agent", "lo"));
    expect(t.entries).toHaveLength(1);
    expect((t.entries[0] as MessageEntry).text).toBe("Hello");
  });

  test("a change of role starts a new message", () => {
    const t = new Transcript();
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
    const t = new Transcript();
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

  test("an update for an unknown tool call is dropped, not synthesized", () => {
    const t = new Transcript();
    t.apply({ sessionUpdate: "tool_call_update", toolCallId: "ghost" } as SessionUpdate);
    expect(t.entries).toHaveLength(0);
  });

  test("panels are keyed by (plugin, id), because ids are only plugin-local", () => {
    const t = new Transcript();
    const vm = { id: "p", title: "A", blocks: [] };
    t.apply({ sessionUpdate: "plugin_panel", plugin: "one", view_model: vm } as SessionUpdate);
    t.apply({
      sessionUpdate: "plugin_panel",
      plugin: "two",
      view_model: { ...vm, title: "B" },
    } as SessionUpdate);
    expect(t.panels.size).toBe(2);
    expect(t.panels.get(panelKey("one", "p"))?.viewModel.title).toBe("A");
    expect(t.panels.get(panelKey("two", "p"))?.viewModel.title).toBe("B");
  });

  test("re-publishing one id replaces that panel, latest wins", () => {
    const t = new Transcript();
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
    expect(t.panels.size).toBe(1);
    expect(t.panels.get(panelKey("one", "p"))?.viewModel.title).toBe("second");
  });

  test("panel_closed removes only its own (plugin, id)", () => {
    const t = new Transcript();
    for (const plugin of ["one", "two"]) {
      t.apply({
        sessionUpdate: "plugin_panel",
        plugin,
        view_model: { id: "p", title: plugin, blocks: [] },
      } as SessionUpdate);
    }
    t.apply({ sessionUpdate: "panel_closed", plugin: "one", id: "p" } as SessionUpdate);
    expect([...t.panels.keys()]).toEqual([panelKey("two", "p")]);
  });

  test("an unknown sessionUpdate tag is ignored rather than guessed at", () => {
    const t = new Transcript();
    t.apply({ sessionUpdate: "something_new_next_year", payload: 1 } as SessionUpdate);
    expect(t.entries).toHaveLength(0);
    expect(t.panels.size).toBe(0);
  });

  test("an empty chunk does not open an empty message", () => {
    const t = new Transcript();
    t.apply(chunk("agent", ""));
    expect(t.entries).toHaveLength(0);
  });
});
