// The context window: the agent's numbers, drawn.
//
// The point of these tests is the line between the two clients. Everything
// `/context` decides — which rows exist, what the unlabelled remainder means,
// where the advisory band starts, how a partition that overruns `used` is
// squeezed — is resolved by the agent and arrives on `x.ai/session/info`. So
// the tests that matter most are the ones that would fail if this client
// started deciding any of it for itself: they hand the widget facts whose
// bands and flags disagree with what a re-derivation would produce, and assert
// that what is drawn is the wire's answer and not the derived one.
//
// The rest is formatting, and it is asserted against the pager's own rules
// rather than against what looks nice here, for the reason `context.ts` gives:
// two clients printing one window in different words is exactly the divergence
// moving the resolver out of the pager was meant to end.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";
import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";

import { ContextWidget } from "../src/components/ContextWidget.tsx";
import { Rail } from "../src/components/Rail.tsx";
import {
  autoCompactLine,
  barBands,
  compactTip,
  compactionRow,
  compactionSummary,
  formatTokens,
  formatTokensBig,
  headline,
  percentOfWindow,
  undetailedCompactions,
  usageChip,
} from "../src/context.ts";
import { createGateway, type Gateway } from "../src/gateway.ts";
import type { SocketLike } from "../src/client.ts";
import { createQueue } from "../src/queue.ts";
import { createSubagents } from "../src/subagents.ts";
import { createTranscript } from "../src/transcript.ts";
import type { ContextFacts, RosterEntry, SessionUpdate } from "../src/wire.ts";

const FACTS: ContextFacts = {
  used: 36_700,
  total: 1_000_000,
  usagePct: 3.67,
  contributors: [
    { kind: "systemPrompt", label: "System prompt", tokens: 1_200 },
    { kind: "messages", label: "Messages", tokens: 29_900 },
    { kind: "toolSchemas", label: "Tool schemas", tokens: 5_600, detail: "12 tools" },
    { kind: "unattributed", label: "Unattributed", tokens: 3_300 },
    { kind: "free", label: "Free", tokens: 963_300 },
  ],
  itemized: [
    { kind: "itemized", label: "Skills", tokens: 2_400, detail: "21 skills", text: "a skill body" },
  ],
  bar: { system: 0, messages: 3, tools: 1, unattributed: 0, free: 96 },
  autoCompact: {
    thresholdPercent: 85,
    thresholdTokens: 850_000,
    remainingTokens: 813_300,
    imminent: false,
    approaching: false,
  },
  turnCount: 5,
  toolCallCount: 12,
  compaction: {
    reportedCount: 0,
    records: [],
    recoveredTokens: 0,
    recordsWithoutRecovery: 0,
    elapsedMs: 0,
  },
};

function facts(over: Partial<ContextFacts> = {}): ContextFacts {
  return { ...FACTS, ...over };
}

describe("the numbers are printed the way the terminal prints them", () => {
  test("a token count rolls over where the pager rolls it over", () => {
    // The cut-over is 99_500 rather than 100_000 because `{:.1}k` would round
    // 99_999 to "100.0k" and the next bucket print "100k" — the same magnitude
    // two characters wider. The reason is a terminal's column alignment; the
    // rule is kept here because the two clients printing one number one way is
    // the whole point of taking the pager's formatter instead of writing one.
    expect(formatTokens(123)).toBe("123");
    expect(formatTokens(1_200)).toBe("1.2k");
    expect(formatTokens(99_499)).toBe("99.5k");
    expect(formatTokens(99_500)).toBe("100k");
    expect(formatTokens(963_300)).toBe("963k");
  });

  test("and the at-a-glance figures roll over again at a million", () => {
    // So a 1m / 2m / 4m window reads as "1.0m" and not "1000k". The legend rows
    // stay on the finer `k`, which is what makes a fractional-million
    // breakdown readable at all.
    expect(formatTokensBig(1_000_000)).toBe("1.0m");
    expect(formatTokensBig(963_300)).toBe("963k");
    expect(headline(FACTS)).toBe("36.7k / 1.0m tokens (3.67%)");
  });

  test("a share floors at a tenth of a percent rather than reading as nothing", () => {
    expect(percentOfWindow(0, 0)).toBe("-");
    expect(percentOfWindow(0, 1_000)).toBe("0.0%");
    expect(percentOfWindow(1, 1_000_000)).toBe("0.1%");
    expect(percentOfWindow(5_600, 1_000_000)).toBe("0.6%");
    expect(percentOfWindow(963_300, 1_000_000)).toBe("96%");
  });

  test("the headline reads the precise share, not the one the threshold uses", () => {
    // The snapshot carries a pre-rounded integer percentage as well, and the
    // resolver keeps them apart on purpose: the rounded one is what the agent
    // compares against the auto-compact trigger, this one is what a person
    // reads. Taking the wrong one puts a number on screen that disagrees with
    // the band beside it.
    expect(headline(facts({ usagePct: 3.674_9 }))).toContain("(3.67%)");
    expect(usageChip(FACTS)).toBe("3.7%");
  });
});

describe("what the wire decided is rendered, not decided again", () => {
  test("the bar's bands are the wire's units, verbatim", () => {
    // The resolver clamps in legend order, so the measured bands keep their
    // true width and the unattributed remainder is what gets squeezed when the
    // independent estimates overrun `used`. These units say the remainder was
    // squeezed to nothing; re-deriving anything from the token counts below
    // would give a different bar and disagree with its own legend.
    const squeezed = facts({ bar: { system: 20, messages: 50, tools: 30, unattributed: 0, free: 0 } });
    expect(barBands(squeezed.bar)).toEqual([
      { kind: "systemPrompt", units: 20 },
      { kind: "messages", units: 50 },
      { kind: "toolSchemas", units: 30 },
    ]);
  });

  test("a zero band is dropped rather than drawn with no width", () => {
    // An element of zero width still occupies whatever sits between bands, and
    // a bar with holes in it has stopped summing to the window.
    expect(barBands(FACTS.bar).map((band) => band.kind)).toEqual([
      "messages",
      "toolSchemas",
      "free",
    ]);
    expect(barBands(FACTS.bar).reduce((sum, band) => sum + band.units, 0)).toBe(100);
  });

  test("the advisory band is the agent's flag and not a comparison made here", () => {
    // 80% is a policy number with no other expression on the wire, which is why
    // it is resolved by the agent. A client comparing `usagePct` against an 80
    // of its own would be a second policy that drifts from the first — so a
    // window at 92% with the flag off gets no tip, and one at 10% with the flag
    // on does.
    const past = facts({
      usagePct: 92,
      autoCompact: { ...FACTS.autoCompact, imminent: true, approaching: false },
    });
    expect(compactTip(past.autoCompact)).toBeNull();
    const advised = facts({
      usagePct: 10,
      autoCompact: { ...FACTS.autoCompact, approaching: true },
    });
    expect(compactTip(advised.autoCompact)).toContain("/compact");
  });

  test("the trigger line says the agent's threshold, in both of its two states", () => {
    expect(autoCompactLine(FACTS.autoCompact)).toBe(
      "Auto-compact at 85% · ~813k tokens remaining",
    );
    expect(autoCompactLine({ ...FACTS.autoCompact, imminent: true })).toBe(
      "Auto-compact triggers next turn (at 85%)",
    );
  });
});

describe("compaction is reported as two numbers, because it is two things", () => {
  test("a count with no record behind it is said out loud", () => {
    // `reportedCount` is the agent's count and `records` is what the session's
    // log still holds. A session resumed without a full replay has more of the
    // first than the second, and letting `records.length` stand in for the
    // total would under-report what compaction did to the conversation.
    const compaction = {
      reportedCount: 3,
      records: [{ ordinal: 3, tokensBefore: 858_000, tokensAfter: 43_000, elapsedMs: 500 }],
      recoveredTokens: 815_000,
      recordsWithoutRecovery: 0,
      elapsedMs: 500,
    };
    expect(undetailedCompactions(compaction)).toBe(2);
    expect(compactionSummary(compaction)).toBe("3 compactions · 815k tokens recovered · 0.5s spent");
  });

  test("a total that omits records says which ones it omits", () => {
    expect(
      compactionSummary({
        reportedCount: 2,
        records: [],
        recoveredTokens: 815_000,
        recordsWithoutRecovery: 1,
        elapsedMs: 0,
      }),
    ).toBe("2 compactions · 815k tokens recovered (excludes 1 event)");
  });

  test("a record with no before count is drawn without one, never with a zero", () => {
    expect(compactionRow({ ordinal: 1, tokensAfter: 43_000 })).toBe("→ 43.0k tokens");
    expect(compactionRow({ ordinal: 1, tokensBefore: 858_000, tokensAfter: 43_000 })).toBe(
      "858k → 43.0k tokens  ·  815k recovered",
    );
  });
});

describe("the widget on screen", () => {
  const draw = (over: Partial<ContextFacts> = {}, full = false) =>
    render(() => ContextWidget({ facts: facts(over), model: "grok-4", full })).container;

  test("draws one band per non-empty unit run, at the wire's widths", () => {
    const bands = [...draw().querySelectorAll<HTMLElement>(".context-band")];
    expect(bands.map((band) => band.style.width)).toEqual(["3%", "1%", "96%"]);
  });

  test("lists the contributors in the agent's order, with its labels", () => {
    const container = draw();
    expect([...container.querySelectorAll(".context-legend .context-label")].map((n) => n.textContent)).toEqual([
      "System prompt",
      "Messages",
      "Tool schemas",
      "Unattributed",
      "Free",
      // The itemized row is a second list below the legend, not a sixth row in
      // it: its tokens are already counted inside Messages.
      "Skills",
    ]);
    expect(container.querySelector(".context-itemized .context-label")?.textContent).toBe("Skills");
  });

  test("says what the unattributed row holds, and only when it is there", () => {
    // The label alone would imply this client knows what is in it. It does not:
    // the row holds reasoning the provider billed but did not itemize,
    // per-request scaffolding, and estimator drift, and nothing can separate
    // them.
    expect(draw().querySelector(".context-note")?.textContent).toContain("Unattributed =");
    const measured = draw({
      contributors: FACTS.contributors.filter((row) => row.kind !== "unattributed"),
    });
    expect(measured.querySelector(".context-note")).toBeNull();
  });

  test("the injected text is in the dialog and not in the column", () => {
    // A 360px column is not where a block of injected context is read. The
    // pager makes the same split: a compact tab, and a separate view for the
    // text each size was measured over.
    expect(draw().querySelector(".context-injection-text")).toBeNull();
    expect(draw({}, true).querySelector(".context-injection-text")?.textContent).toBe(
      "a skill body",
    );
  });

  test("an agent that sends a size without its text says which half is missing", () => {
    const container = draw(
      { itemized: [{ kind: "itemized", label: "AGENTS.md", tokens: 1_100 }] },
      true,
    );
    expect(container.querySelector(".context-injection-text")?.textContent).toContain(
      "reports the size but not the text",
    );
  });

  test("the counters and the trigger are on screen, in the agent's words", () => {
    const container = draw();
    expect(container.querySelector(".context-footer")?.textContent).toBe(
      "Turns: 5 · Tool calls: 12 · Compactions: 0",
    );
    expect(container.querySelector(".context-auto")?.textContent).toContain("Auto-compact at 85%");
  });
});

// ---------------------------------------------------------------------------
// In the rail
// ---------------------------------------------------------------------------

const ENTRY: RosterEntry = {
  sessionId: "s1",
  cwd: "/home/me/repo",
  isWorktree: false,
  yolo: false,
  activity: "idle",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

function panelModel(id: string, title: string): PanelViewModel {
  return {
    id,
    title,
    blocks: [{ kind: "status", items: [{ label: "state", value: "up", tone: "neutral" }] }],
  };
}

function railWith(info: { contextFacts?: ContextFacts } | null, panels: [string, PanelViewModel][]) {
  const transcript = createTranscript();
  for (const [plugin, viewModel] of panels) {
    transcript.apply({
      sessionUpdate: "plugin_panel",
      plugin,
      view_model: viewModel,
    } as SessionUpdate);
  }
  const gateway = {
    attached: () => ({ entry: ENTRY, transcript, subagents: createSubagents(ENTRY.sessionId), queue: createQueue() }),
    sessionInfo: () => info,
    refreshSessionInfo: async () => {},
    panelAction: async () => {},
  } as unknown as Gateway;
  return render(() => Rail({ gateway })).container;
}

describe("the context section in the rail", () => {
  test("stands above the plugins, and no publication can move it", () => {
    // Built-ins are always higher: publishing a panel is a plugin asking for
    // the space, not taking it.
    const container = railWith({ contextFacts: FACTS }, [["acme", panelModel("p", "acme: OAuth")]]);
    expect([...container.querySelectorAll(".rail-label")].map((n) => n.textContent)).toEqual([
      "Context",
      "acme: OAuth",
    ]);
  });

  test("is absent until the agent has answered, rather than drawn as an empty window", () => {
    // `null` is nobody having asked yet, which is not a window of zero — and a
    // breakdown of zeros is a lie a person cannot tell from a fresh session.
    expect(railWith(null, []).querySelector(".rail")).toBeNull();
    expect(railWith({}, []).querySelector(".rail")).toBeNull();
  });

  test("folded, it still says the share, because that is what it is asked", () => {
    const container = railWith({ contextFacts: FACTS }, []);
    expect(container.querySelector(".rail-note")?.textContent).toBe("3.7%");
    container.querySelector<HTMLButtonElement>(".rail-disclosure")!.click();
    expect(container.querySelector(".context")).toBeNull();
    expect(container.querySelector(".rail-note")?.textContent).toBe("3.7%");
  });

  test("Open raises the whole picture, injected text and all", () => {
    const container = railWith({ contextFacts: FACTS }, []);
    container.querySelector<HTMLButtonElement>(".rail-open")!.click();
    const dialog = document.querySelector(".overlay")!;
    expect(dialog.getAttribute("aria-label")).toBe("Context");
    expect(dialog.querySelector(".context-injection-text")?.textContent).toBe("a skill body");
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(document.querySelector(".overlay")).toBeNull();
  });

  test("the keyboard walk includes it, ahead of the panels", () => {
    const container = railWith({ contextFacts: FACTS }, [["acme", panelModel("p", "acme: OAuth")]]);
    const headers = [...container.querySelectorAll<HTMLButtonElement>(".rail-disclosure")];
    headers[0]!.focus();
    document.activeElement!.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }),
    );
    expect(document.activeElement).toBe(headers[1]!);
  });
});

// ---------------------------------------------------------------------------
// When the client asks
// ---------------------------------------------------------------------------

/** A leader that answers the handshake, one load, one turn and one info. */
class Leader implements SocketLike {
  asked: string[] = [];
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    const frame = JSON.parse(data) as { id?: number; method?: string };
    if (frame.id === undefined || frame.method === undefined) return;
    this.asked.push(frame.method);
    const results: Record<string, unknown> = {
      initialize: {
        protocolVersion: 1,
        authMethods: [{ id: "xai.api_key", name: "api key" }],
        _meta: { restoredAuthMeta: {} },
      },
      "session/load": {},
      "session/prompt": { stopReason: "end_turn" },
      "_x.ai/sessions/list": { result: { sessions: [ENTRY] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
      "_x.ai/session/info": {
        result: { sessionId: ENTRY.sessionId, cwd: ENTRY.cwd, contextFacts: FACTS },
      },
    };
    const result = results[frame.method];
    if (result === undefined) return;
    queueMicrotask(() => this.receive(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result })));
  }

  close(): void {}

  addEventListener(type: string, listener: (event: never) => void): void {
    const bucket = this.listeners.get(type) ?? [];
    bucket.push(listener);
    this.listeners.set(type, bucket);
    if (type === "open") queueMicrotask(() => (listener as () => void)());
  }

  receive(text: string): void {
    for (const listener of this.listeners.get("message") ?? []) {
      (listener as (event: { data: unknown }) => void)({ data: text });
    }
  }
}

let leader: Leader;
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  leader = new Leader();
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return leader;
  };
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

const infoCalls = (): number => leader.asked.filter((m) => m === "_x.ai/session/info").length;

/** Long enough for a reply that was queued as a microtask to have landed. */
const settle = (ms = 20): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

describe("when the window is re-read", () => {
  test("on attaching, at the end of a turn, and on nothing else", async () => {
    // `contextFacts` rides a request/response method with no notification
    // carrier of its own — the pager debounces its own asking rather than
    // subscribing to anything — so the cadence is the only thing a client gets
    // to choose, and a timer would be a cadence this product has not got. The
    // window moves when a turn does; that is why the end of one is the trigger.
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
    await gateway.attach(ENTRY);
    // The ask leaves with the attach; the answer lands a round trip later,
    // which is why the window is not part of what attaching waits for.
    await settle();
    expect(infoCalls()).toBe(1);
    expect(gateway.sessionInfo()?.contextFacts?.used).toBe(FACTS.used);

    await gateway.prompt("hello");
    expect(infoCalls()).toBe(2);

    await new Promise((resolve) => setTimeout(resolve, 250));
    expect(infoCalls()).toBe(2);
  });

  test("and a move to another gateway drops the window rather than carrying it", async () => {
    // A breakdown is nothing but numbers, so one left over from another leader
    // reads as this one's. `null` says nobody has asked yet, which is the
    // honest state between two sessions and between two machines.
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
    await gateway.attach(ENTRY);
    await settle();
    expect(gateway.sessionInfo()).not.toBeNull();
    leader = new Leader();
    await gateway.connect("ws://127.0.0.1:2421/ws", "secret");
    expect(gateway.sessionInfo()).toBeNull();
  });
});
