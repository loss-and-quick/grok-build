// An instance is a machine, not an address.
//
// That claim is the whole design, so most of what is below is written to fail
// if it stops being true: two records reached by two URLs that answer with one
// `agentId` have to become one record with two addresses, and nothing may
// connect because a URL said to.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { cleanup, render } from "@solidjs/testing-library";

import { App, Home, SessionRoute } from "../src/App.tsx";
import { InstanceMenu } from "../src/components/InstanceMenu.tsx";
import type { SocketLike } from "../src/client.ts";
import {
  describeSeen,
  forgetInstance,
  instanceState,
  loadInstances,
  newInstance,
  readIdentity,
  rememberConnection,
  rememberSecret,
  restarted,
  saveInstances,
  secretFor,
  sessionHref,
  storageFault,
  type Instance,
} from "../src/instances.ts";

function instance(over: Partial<Instance> & { id: string }): Instance {
  return { label: over.id, addresses: [], ...over };
}

describe("what `initialize` says about the machine", () => {
  test("the four keys are read, and anything else in `_meta` is not", () => {
    expect(
      readIdentity({
        agentId: "machine",
        agentInstanceId: "run",
        hostname: "magicbook",
        agentVersion: "1.0.16",
        currentWorkingDirectory: "/home/me",
      }),
    ).toEqual({
      agentId: "machine",
      agentInstanceId: "run",
      hostname: "magicbook",
      agentVersion: "1.0.16",
    });
  });

  test("a key that is not a string is absent, not empty", () => {
    // An older agent, or a newer one that changed a type. `undefined` means
    // "not told", and `""` would mean "told nothing" — the difference decides
    // whether a record gets merged or forked.
    expect(readIdentity({ agentId: 7, hostname: "" }).agentId).toBeUndefined();
    expect(readIdentity(undefined)).toEqual({
      agentId: undefined,
      agentInstanceId: undefined,
      hostname: undefined,
      agentVersion: undefined,
    });
  });
});

describe("two addresses, one machine", () => {
  const loopback = instance({
    id: "a",
    label: "127.0.0.1:2420",
    addresses: ["ws://127.0.0.1:2420/ws"],
    agentId: "machine",
    lastSeenUnixMs: 10,
  });
  const overTheNetwork = instance({
    id: "b",
    label: "the laptop",
    addresses: ["ws://192.168.1.5:2420/ws"],
    agentId: "machine",
    lastSeenUnixMs: 20,
  });

  test("collapse into one record holding both addresses", () => {
    // The failure this prevents is the one opencode had to migrate out of:
    // keyed on the URL, `127.0.0.1` and the machine's network address are two
    // servers with two rosters, and the same sessions are listed twice.
    const folded = rememberConnection([loopback, overTheNetwork], {
      id: "a",
      address: "ws://127.0.0.1:2420/ws",
      identity: { agentId: "machine", hostname: "magicbook" },
      at: 30,
    });
    expect(folded.instances).toHaveLength(1);
    expect(folded.id).toBe("a");
    expect(folded.merged).toEqual(["b"]);
    expect(folded.instances[0]!.addresses).toEqual([
      "ws://127.0.0.1:2420/ws",
      "ws://192.168.1.5:2420/ws",
    ]);
  });

  test("and the name a person typed survives the merge, from either side", () => {
    // "the laptop" was typed on the record being absorbed. A merge that kept
    // the surviving record's untouched hostname would silently undo a rename.
    const folded = rememberConnection([loopback, overTheNetwork], {
      id: "a",
      address: "ws://127.0.0.1:2420/ws",
      identity: { agentId: "machine", hostname: "magicbook" },
      at: 30,
    });
    expect(folded.instances[0]!.label).toBe("the laptop");
  });

  test("the address just used is the one a retry repeats", () => {
    const folded = rememberConnection([loopback, overTheNetwork], {
      id: "b",
      address: "ws://192.168.1.5:2420/ws",
      identity: { agentId: "machine" },
      at: 30,
    });
    expect(folded.instances[0]!.addresses[0]).toBe("ws://192.168.1.5:2420/ws");
  });

  test("a record nobody has connected to is never absorbed", () => {
    // It has no `agentId`, so there is no evidence it is the same machine —
    // and merging on an address would be the mistake this is all here to
    // avoid.
    const typed = instance({ id: "c", addresses: ["ws://10.0.0.9:2420/ws"] });
    const folded = rememberConnection([loopback, typed], {
      id: "a",
      address: "ws://127.0.0.1:2420/ws",
      identity: { agentId: "machine" },
      at: 30,
    });
    expect(folded.instances.map((each) => each.id)).toEqual(["a", "c"]);
  });

  test("two machines stay two", () => {
    const other = instance({ id: "b", addresses: ["ws://box:2420/ws"], agentId: "another" });
    const folded = rememberConnection([loopback, other], {
      id: "a",
      address: "ws://127.0.0.1:2420/ws",
      identity: { agentId: "machine" },
      at: 30,
    });
    expect(folded.instances).toHaveLength(2);
    expect(folded.merged).toEqual([]);
  });

  test("an unnamed record takes the machine's own name", () => {
    const folded = rememberConnection([loopback], {
      id: "a",
      address: "ws://127.0.0.1:2420/ws",
      identity: { agentId: "machine", hostname: "magicbook" },
      at: 30,
    });
    expect(folded.instances[0]!.label).toBe("magicbook");
  });
});

describe("the leader behind an instance", () => {
  test("a new process id means the sessions may be gone", () => {
    // `agentInstanceId` is a generation, not an identity: it is minted afresh
    // on every launch. A client that reopened `lastSessionId` on faith after
    // one changed would be asking for a session that no longer exists.
    const known = instance({ id: "a", agentInstanceId: "run-1" });
    expect(restarted(known, { agentInstanceId: "run-2" })).toBe(true);
    expect(restarted(known, { agentInstanceId: "run-1" })).toBe(false);
    expect(restarted(instance({ id: "a" }), { agentInstanceId: "run-2" })).toBe(false);
  });
});

describe("the state the line shows", () => {
  test("a socket that is up with no credential behind it is not `live`", () => {
    // The two have always been distinguishable — the sign-in card's own
    // condition is exactly this pair — and the line above the roster wrote
    // "connected" for both, which is the one state where nothing on the roster
    // can be opened.
    expect(instanceState("live", true, true)).toBe("live");
    expect(instanceState("live", false, true)).toBe("unauthenticated");
  });

  test("and not being connected says which kind of not-connected", () => {
    expect(instanceState("connecting", false, true)).toBe("connecting");
    expect(instanceState("waiting", false, true)).toBe("waiting");
    expect(instanceState("lost", false, true)).toBe("lost");
    expect(instanceState("offline", false, true)).toBe("away");
    expect(instanceState("offline", false, false)).toBe("never");
  });
});

describe("a link to a session", () => {
  test("names the instance it was written on, and works without one", () => {
    // Old bookmarks keep working and read as "whichever instance this tab is
    // on", which is what they have always meant.
    expect(sessionHref("s1", "abc")).toBe("/s/s1?i=abc");
    expect(sessionHref("s1", null)).toBe("/s/s1");
  });
});

describe("when it was last seen", () => {
  test("is a phrase, not a timestamp — and says so when there is none", () => {
    expect(describeSeen(undefined, 1_000)).toBe("never connected");
    expect(describeSeen(1_000_000 - 300_000, 1_000_000)).toContain("5 minutes");
  });
});

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

describe("what is kept in this browser", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  test("the single remembered address becomes an instance, and the secret moves", () => {
    // The old shape was three flat keys, and the flat `url` is the whole
    // reason this client could remember one machine. Moving rather than
    // copying matters: a secret left under the old key would survive being
    // forgotten.
    localStorage.setItem("grok-gateway", "ws://127.0.0.1:2420/ws");
    localStorage.setItem("grok-secret", "hunter2");
    const migrated = loadInstances();
    expect(migrated).toHaveLength(1);
    expect(migrated[0]!.addresses).toEqual(["ws://127.0.0.1:2420/ws"]);
    expect(migrated[0]!.label).toBe("127.0.0.1:2420");
    expect(secretFor(migrated[0]!.id)).toBe("hunter2");
    expect(localStorage.getItem("grok-secret")).toBeNull();
    // And it is a migration, not a re-read: the second call finds a list.
    expect(loadInstances()[0]!.id).toBe(migrated[0]!.id);
  });

  test("forgetting a machine forgets the way into it", () => {
    const one = newInstance("ws://127.0.0.1:2420/ws");
    saveInstances([one]);
    rememberSecret(one.id, "hunter2");
    expect(forgetInstance([one], one.id)).toEqual([]);
    expect(secretFor(one.id)).toBe("");
    expect(loadInstances()).toEqual([]);
  });

  test("a store that will not take the list says so instead of forgetting quietly", () => {
    // A theme that fails to save is retyped in a second. A list of machines
    // that fails to save is a machine somebody added and cannot find again,
    // which is why this one is reported rather than swallowed.
    const real = globalThis.localStorage;
    const full = new Error("full");
    full.name = "QuotaExceededError";
    const stub = (): void => {
      throw full;
    };
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      get: () => ({ setItem: stub, getItem: () => null, removeItem: stub }),
    });
    saveInstances([newInstance("ws://127.0.0.1:2420/ws")]);
    expect(storageFault()).toBe("quota");
    Object.defineProperty(globalThis, "localStorage", { configurable: true, get: () => real });
    saveInstances([]);
    expect(storageFault()).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// The menu
// ---------------------------------------------------------------------------

describe("the instance menu", () => {
  const list: Instance[] = [
    instance({
      id: "a",
      label: "magicbook",
      addresses: ["ws://127.0.0.1:2420/ws", "ws://192.168.1.5:2420/ws"],
      agentId: "machine",
      agentVersion: "1.0.16",
      sessionCount: 3,
      lastSeenUnixMs: 900_000,
    }),
    instance({ id: "b", label: "the box", addresses: ["ws://box:2420/ws"], agentId: "other" }),
  ];

  function open(handlers: Partial<Parameters<typeof InstanceMenu>[0]> = {}) {
    const seen: string[] = [];
    const { container } = render(() =>
      InstanceMenu({
        instances: list,
        currentId: "a",
        defaultId: "a",
        state: "live",
        now: 1_000_000,
        onSwitch: (chosen) => seen.push(`switch:${chosen.id}`),
        onRename: (id, label) => seen.push(`rename:${id}:${label}`),
        onForget: (id) => seen.push(`forget:${id}`),
        onMakeDefault: (id) => seen.push(`default:${id}`),
        onAdd: () => seen.push("add"),
        onClose: () => seen.push("close"),
        ...handlers,
      }),
    );
    return { container, seen };
  }

  test("shows every address a machine answers on, which is the merge made visible", () => {
    const { container } = open();
    expect(container.querySelector(".instance-hosts")?.textContent).toBe(
      "127.0.0.1:2420 · 192.168.1.5:2420",
    );
  });

  test("dates a row and names the agent's version", () => {
    const { container } = open();
    expect(container.querySelector(".instance-detail")?.textContent).toContain("3 sessions");
    expect(container.querySelector(".instance-detail")?.textContent).toContain("1.0.16");
  });

  test("marks where the page is, for a reader and not only for the eye", () => {
    const { container } = open();
    const rows = [...container.querySelectorAll(".instance")];
    expect(rows[0]!.getAttribute("aria-current")).toBe("true");
    expect(rows[1]!.getAttribute("aria-current")).toBeNull();
  });

  test("switching is a press, and it is the only thing that starts one", () => {
    const { container, seen } = open();
    const names = [...container.querySelectorAll<HTMLButtonElement>(".instance-name")];
    names[1]!.click();
    expect(seen).toEqual(["switch:b"]);
  });

  test("renaming happens in place and keeps what was typed", () => {
    const { container, seen } = open();
    container.querySelector<HTMLButtonElement>(".instance-rename-button")!.click();
    const field = container.querySelector<HTMLInputElement>(".instance-rename")!;
    field.value = "the laptop";
    field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(seen).toEqual(["rename:a:the laptop"]);
  });

  test("the instance new tabs open on is named apart from the one in view", () => {
    const { container, seen } = open({ currentId: "b" });
    // `a` is still the default, so its row says so rather than offering to
    // become one, and `b` — where the page is — offers.
    expect(container.querySelector(".instance-default")?.textContent).toBe("opens new tabs");
    container.querySelector<HTMLButtonElement>(".instance-default-button")!.click();
    expect(seen).toEqual(["default:b"]);
  });

  test("Escape closes it, and it never claimed to be a dialog", () => {
    const { container, seen } = open();
    expect(container.querySelector(".instances")?.getAttribute("role")).toBe("menu");
    expect(container.querySelector(".instances")?.getAttribute("aria-modal")).toBeNull();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(seen).toEqual(["close"]);
  });
});

// ---------------------------------------------------------------------------
// A route that names a session on another machine
// ---------------------------------------------------------------------------

const SESSION = "sess-a";
const CWD = "/home/me/repo";
const A = "ws://127.0.0.1:2420/ws";
const B = "ws://127.0.0.1:2430/ws";

const ROW = {
  sessionId: SESSION,
  cwd: CWD,
  isWorktree: false,
  yolo: false,
  activity: "idle",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

/** A leader that answers the handshake and names itself. */
class Leader implements SocketLike {
  private listeners = new Map<string, ((event: never) => void)[]>();

  constructor(private readonly agentId: string) {}

  send(data: string): void {
    const frame = JSON.parse(data) as { id?: number; method?: string };
    if (frame.id === undefined || frame.method === undefined) return;
    const results: Record<string, unknown> = {
      initialize: {
        protocolVersion: 1,
        authMethods: [{ id: "xai.api_key", name: "api key" }],
        _meta: {
          restoredAuthMeta: {},
          agentId: this.agentId,
          agentInstanceId: `${this.agentId}-run`,
          hostname: this.agentId,
          agentVersion: "1.0.16",
        },
      },
      "session/load": {},
      "_x.ai/sessions/list": { result: { sessions: [ROW] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
      "_x.ai/session/info": { result: {} },
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

let opened: string[] = [];
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  cleanup();
  opened = [];
  (globalThis as { WebSocket: unknown }).WebSocket = function (url: string) {
    opened.push(url);
    return new Leader(url.includes("2430") ? "machine-b" : "machine-a");
  };
  localStorage.clear();
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

function openAt(path: string) {
  const history = createMemoryHistory();
  history.set({ value: path, replace: true });
  const { container } = render(() => (
    <MemoryRouter root={App} history={history}>
      <Route path="/" component={Home} />
      <Route path="/s/:sessionId" component={SessionRoute} />
    </MemoryRouter>
  ));
  return container;
}

const settle = (ms = 150): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Reach the address form, from wherever the page is.
 *
 * Offline it is the whole of the connection line; connected it is one press
 * away, behind the instance menu. Both are how a person gets there.
 */
function addressForm(root: Element): void {
  if (root.querySelector(".connect-url")) return;
  root.querySelector<HTMLButtonElement>(".link-where")!.click();
  root.querySelector<HTMLButtonElement>(".instance-add")!.click();
}

describe("a `/s/<id>` that belongs to another instance", () => {
  test("says which machine was searched, and does not connect to the other one", async () => {
    // The rule this pins: a route never starts a connection to an instance the
    // page is not on. Connecting here *is* signing in — `connect` settles
    // authentication — so a URL that could do it would be an action wearing
    // navigation's clothes.
    const here = newInstance(A, "here");
    const there = { ...newInstance(B, "there"), label: "the box", agentId: "machine-b" };
    saveInstances([here, there]);
    rememberSecret(here.id, "secret");
    rememberSecret(there.id, "secret");
    localStorage.setItem("grok.instance.default", here.id);

    const root = openAt(`/s/somewhere-else?i=there`);
    await settle();

    expect(opened).toEqual([`${A}?server-key=secret`]);
    const missing = root.querySelector(".missing-session")?.textContent ?? "";
    expect(missing).toContain("No session");
    expect(missing).toContain("the box");
    expect(root.querySelector(".missing-switch")).not.toBeNull();
  });

  test("and the offer is a button, which is what opens the other socket", async () => {
    const here = newInstance(A, "here");
    const there = { ...newInstance(B, "there"), label: "the box", agentId: "machine-b" };
    saveInstances([here, there]);
    rememberSecret(here.id, "secret");
    rememberSecret(there.id, "secret");
    localStorage.setItem("grok.instance.default", here.id);

    const root = openAt(`/s/somewhere-else?i=there`);
    await settle();
    root.querySelector<HTMLButtonElement>(".missing-switch")!.click();
    await settle();
    expect(opened).toEqual([`${A}?server-key=secret`, `${B}?server-key=secret`]);
  });

  test("a link with no instance on it is not treated as a link to another one", async () => {
    // Every bookmark written before instances existed lands here, and it means
    // "on whichever instance this tab is on" — which is exactly what it always
    // meant.
    const here = newInstance(A, "here");
    saveInstances([here]);
    rememberSecret(here.id, "secret");

    const root = openAt("/s/somewhere-else");
    await settle();
    expect(root.querySelector(".missing-switch")).toBeNull();
    expect(root.querySelector(".missing-session")?.textContent).toContain("No session");
  });

  test("a merge takes the pointers that named the record it absorbed", async () => {
    // Two records, one machine: the second is about to swallow the first, and
    // "the instance new tabs open on" names the first. Left alone it would name
    // an id that is no longer in the list, and the next tab would open on
    // nothing at all.
    const first = { ...newInstance(A, "first"), label: "first", agentId: "machine-a" };
    const second = newInstance("ws://localhost:2420/ws", "second");
    saveInstances([first, second]);
    localStorage.setItem("grok.instance.default", first.id);

    const root = openAt("/");
    await settle();
    addressForm(root);
    (root.querySelector(".connect-url") as HTMLInputElement).value = "ws://localhost:2420/ws";
    (root.querySelector(".connect-secret") as HTMLInputElement).value = "secret";
    (root.querySelector(".connect-button") as HTMLButtonElement)!.click();
    await settle();

    const stored = loadInstances();
    expect(stored).toHaveLength(1);
    expect(stored[0]!.addresses).toEqual(["ws://localhost:2420/ws", A]);
    expect(localStorage.getItem("grok.instance.default")).toBe(stored[0]!.id);
    expect(localStorage.getItem("grok.instance.current")).toBe(stored[0]!.id);
  });

  test("the machine the page reached is remembered by its own id, not its address", async () => {
    const here = newInstance(A, "here");
    saveInstances([here]);
    rememberSecret(here.id, "secret");

    openAt("/");
    await settle();
    const stored = loadInstances();
    expect(stored).toHaveLength(1);
    expect(stored[0]!.agentId).toBe("machine-a");
    expect(stored[0]!.label).toBe("machine-a");
    expect(stored[0]!.sessionCount).toBe(1);
  });
});
