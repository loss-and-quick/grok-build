// The connection state, and what the page does when it goes bad.
//
// The defect these stand against is a specific one: `connection()` stayed
// `"connected"` after the socket closed, so a leader that had been restarted —
// or a laptop that had slept — left a page that looked alive, offered every
// control a live page offers, and refused all of them. A status that cannot go
// backwards is the worst thing a status can be.
import { describe, expect, test } from "bun:test";

import { createLink, type Link } from "../src/App.tsx";
import {
  GatewayClient,
  RETRY_CEILING_MS,
  RETRY_LIMIT,
  retryDelayMs,
  type SocketLike,
} from "../src/client.ts";
import type { Gateway } from "../src/gateway.ts";
import type { RosterEntry } from "../src/wire.ts";

class FakeSocket implements SocketLike {
  closed = false;
  private listeners = new Map<string, (() => void)[]>();

  send(): void {}

  close(): void {
    this.closed = true;
  }

  addEventListener(type: string, listener: (event: never) => void): void {
    const bucket = this.listeners.get(type) ?? [];
    bucket.push(listener as () => void);
    this.listeners.set(type, bucket);
  }

  fire(type: "open" | "close" | "error"): void {
    for (const listener of this.listeners.get(type) ?? []) listener();
  }
}

interface Harness {
  link: Link;
  /** Whether the next connect finds a gateway on the other end. */
  up: (answering: boolean) => void;
  /** The socket of the newest connection, so a test can kill it. */
  socket: () => FakeSocket;
  connects: () => number;
  attaches: () => string[];
  /** Every wait the ladder asked for, in order. */
  waits: () => number[];
  dispose: () => void;
}

function harness(over: Partial<Gateway> = {}): Harness {
  let answering = true;
  let sockets: FakeSocket[] = [];
  let connects = 0;
  const attaches: string[] = [];
  const waits: number[] = [];

  const gateway = {
    connect: async (): Promise<void> => {
      connects += 1;
      const socket = new FakeSocket();
      sockets.push(socket);
      const client = new GatewayClient(() => socket);
      const opening = client.connect();
      // A gateway that is not running answers the way a browser reports it: an
      // error, then a close, both before the socket ever opens.
      if (!answering) {
        socket.fire("error");
        socket.fire("close");
      } else {
        socket.fire("open");
      }
      await opening;
    },
    disconnect: (): void => {},
    attached: () => null,
    roster: { get: (): RosterEntry | undefined => undefined },
    attach: async (entry: RosterEntry): Promise<void> => {
      attaches.push(entry.sessionId);
    },
    ...over,
  } as unknown as Gateway;

  const link = createLink(gateway, async (ms) => {
    waits.push(ms);
  });

  return {
    link,
    up: (next) => {
      answering = next;
    },
    socket: () => sockets[sockets.length - 1]!,
    connects: () => connects,
    attaches: () => attaches,
    waits: () => waits,
    dispose: () => {
      link.dispose();
      sockets = [];
    },
  };
}

/** Let the supervisor's own promises run to their end. */
function settle(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

describe("the retry ladder", () => {
  test("the first attempt is immediate and every later one doubles to a ceiling", () => {
    // Immediate first, because the commonest drop is momentary; doubling after,
    // because a leader that is down should be asked less often, not more.
    expect(retryDelayMs(1)).toBe(0);
    const ladder = Array.from({ length: RETRY_LIMIT }, (_, i) => retryDelayMs(i + 1));
    expect(ladder).toEqual([0, 500, 1000, 2000, 4000, 8000, 16_000, 30_000, 30_000]);
  });

  test("it has a ceiling, so a tab left open overnight cannot hammer a gateway", () => {
    for (let attempt = 1; attempt < 100; attempt += 1) {
      expect(retryDelayMs(attempt)).toBeLessThanOrEqual(RETRY_CEILING_MS);
    }
  });

  test("it is finite, so the page can say it stopped rather than lie about trying", () => {
    expect(RETRY_LIMIT).toBeGreaterThan(1);
    expect(Number.isFinite(RETRY_LIMIT)).toBe(true);
  });
});

describe("a link that drops", () => {
  test("stops calling itself live the moment the socket closes", async () => {
    const h = harness();
    await h.link.open("ws://127.0.0.1:2420/ws", "s3cret");
    expect(h.link.phase()).toBe("live");

    // The leader goes away: the socket closes and nothing answers after it.
    h.up(false);
    h.socket().fire("close");
    await settle();

    expect(h.link.phase()).not.toBe("live");
    expect(h.link.note()).toContain("stopped answering");
    h.dispose();
  });

  test("is chased, and the first chase is immediate", async () => {
    const h = harness();
    await h.link.open("ws://127.0.0.1:2420/ws", "s3cret");
    const before = h.connects();
    h.socket().fire("close");
    await settle();

    // Still up, so the immediate attempt succeeds and the page is live again
    // without anyone pressing anything.
    expect(h.connects()).toBe(before + 1);
    expect(h.link.phase()).toBe("live");
    expect(h.waits()).toEqual([]);
    h.dispose();
  });

  test("gives up after the ladder, and walks exactly the ladder", async () => {
    const h = harness();
    await h.link.open("ws://127.0.0.1:2420/ws", "s3cret");
    h.up(false);
    h.socket().fire("close");
    await settle();

    expect(h.link.phase()).toBe("lost");
    expect(h.waits()).toEqual([500, 1000, 2000, 4000, 8000, 16_000, 30_000, 30_000]);
    h.dispose();
  });

  test("a gateway that comes back is picked up mid-ladder", async () => {
    const h = harness();
    await h.link.open("ws://127.0.0.1:2420/ws", "s3cret");
    h.up(false);
    h.socket().fire("close");
    await settle();
    expect(h.link.phase()).toBe("lost");

    // The leader is restarted and the person presses the button the "lost"
    // state offers. The same path runs when the tab is focused again.
    h.up(true);
    h.link.retryNow();
    await settle();
    expect(h.link.phase()).toBe("live");
    h.dispose();
  });

  test("hanging up is not a drop, so nothing chases it", async () => {
    const h = harness();
    await h.link.open("ws://127.0.0.1:2420/ws", "s3cret");
    const before = h.connects();
    h.link.hangUp();
    // `disconnect` closes the socket, and the close event arrives after.
    h.socket().fire("close");
    await settle();

    expect(h.link.phase()).toBe("offline");
    expect(h.connects()).toBe(before);
    h.dispose();
  });

  test("a deliberate connect that fails is not retried nine times", async () => {
    // A wrong secret is not a dropped link. Nine attempts at it are nine
    // rejections and no new information, and the ladder would hide the error.
    const h = harness();
    h.up(false);
    const opened = await h.link.open("ws://127.0.0.1:2420/ws", "wrong");
    expect(opened).toBe(false);
    expect(h.link.phase()).toBe("offline");
    expect(h.connects()).toBe(1);
    h.dispose();
  });
});

describe("the session that was on screen", () => {
  const entry = { sessionId: "s1", cwd: "/home/me/repo" } as unknown as RosterEntry;

  test("is attached again once the link is back", async () => {
    // ACP v1 replays nothing that happened while a client was away, so a
    // transcript left alone is a transcript missing the whole outage. `attach`
    // builds a fresh one and `session/load` refills it, which is why doing this
    // automatically cannot duplicate what the person already read.
    const h = harness({
      attached: () => ({ entry }) as never,
      roster: { get: (id: string) => (id === "s1" ? entry : undefined) } as never,
    });
    await h.link.open("ws://127.0.0.1:2420/ws", "s3cret");
    const before = h.attaches().length;
    h.socket().fire("close");
    await settle();

    expect(h.attaches().slice(before)).toEqual(["s1"]);
    h.dispose();
  });

  test("is named, not silently replaced, when the leader no longer has it", async () => {
    const h = harness({
      attached: () => ({ entry }) as never,
      roster: { get: () => undefined } as never,
    });
    await h.link.open("ws://127.0.0.1:2420/ws", "s3cret");
    h.socket().fire("close");
    await settle();

    expect(h.attaches()).toEqual([]);
    expect(h.link.note()).toContain("s1");
    h.dispose();
  });
});
