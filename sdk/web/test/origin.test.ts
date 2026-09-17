// Served by the gateway it talks to.
//
// The page used to be a page from a dev server and the gateway a process
// somewhere else, so both halves of an address had to be typed in. Served by
// `grok web` they are one origin, and the startup banner hands over the secret
// in the URL — which is only acceptable if the client takes it straight back
// out of the address bar, and if none of this quietly turns the instance list
// into a list of addresses again.
import { describe, expect, test } from "bun:test";

import { instanceForAddress, newInstance, type Instance } from "../src/instances.ts";
import { servedFrom } from "../src/origin.ts";

function location(url: string) {
  const parsed = new URL(url);
  return {
    protocol: parsed.protocol,
    host: parsed.host,
    search: parsed.search,
    pathname: parsed.pathname,
    hash: parsed.hash,
  };
}

/** Records what the page would have rewritten its own URL to. */
function recorder() {
  const rewritten: string[] = [];
  return {
    rewritten,
    history: {
      replaceState: (_state: unknown, _title: string, url?: string | URL | null) => {
        rewritten.push(String(url));
      },
    },
  };
}

describe("the gateway that served the page", () => {
  test("the socket is this origin's own, so nothing has to be typed", () => {
    expect(servedFrom(location("http://127.0.0.1:2420/"), null)?.endpoint).toBe(
      "ws://127.0.0.1:2420/ws",
    );
  });

  test("https serves wss, because a ws: socket from an https: page is blocked outright", () => {
    expect(servedFrom(location("https://box.local:8443/s/abc"), null)?.endpoint).toBe(
      "wss://box.local:8443/ws",
    );
  });

  test("a page that no gateway served says so, rather than inventing an origin", () => {
    // `file:` has no host to build a socket from. The page then asks for an
    // address exactly as it did before any of this existed.
    expect(servedFrom(location("file:///home/me/index.html"), null)).toBeNull();
  });

  test("the key the banner printed is read once and taken out of the address bar", () => {
    const { rewritten, history } = recorder();
    const served = servedFrom(location("http://127.0.0.1:2420/?server-key=abc123"), history);
    expect(served?.secret).toBe("abc123");
    expect(rewritten).toEqual(["/"]);
  });

  test("stripping the key keeps the page it was on, and every other parameter", () => {
    // `?i=` names which instance a `/s/<id>` belongs to. Dropping it along with
    // the credential would send a reload to the wrong machine's session.
    const { rewritten, history } = recorder();
    const served = servedFrom(
      location("http://127.0.0.1:2420/s/sess-1?i=inst-1&server-key=abc123#top"),
      history,
    );
    expect(served?.secret).toBe("abc123");
    expect(rewritten).toEqual(["/s/sess-1?i=inst-1#top"]);
  });

  test("no key in the URL rewrites nothing", () => {
    // A reload after the first load is the common case, and a `replaceState`
    // per load would push a history entry's worth of noise for no reason.
    const { rewritten, history } = recorder();
    expect(servedFrom(location("http://127.0.0.1:2420/"), history)?.secret).toBeNull();
    expect(rewritten).toEqual([]);
  });
});

describe("the served address joins the list without forking it", () => {
  const ORIGIN = "ws://127.0.0.1:2420/ws";

  test("an address nobody remembers becomes a record", () => {
    const { instances, instance } = instanceForAddress([], ORIGIN);
    expect(instances).toHaveLength(1);
    expect(instance.addresses).toEqual([ORIGIN]);
    expect(instance.label).toBe("127.0.0.1:2420");
  });

  test("an address already remembered is that record, not a second one", () => {
    const known = newInstance(ORIGIN);
    const { instances, instance } = instanceForAddress([known], ORIGIN);
    expect(instances).toHaveLength(1);
    expect(instance.id).toBe(known.id);
  });

  test("the served address leads, because that is the one about to be dialled", () => {
    // A machine remembered from a laptop as `10.0.0.4` and loaded here as
    // loopback is one record with two addresses, and connecting through
    // `addresses[0]` would otherwise dial the address this browser cannot reach.
    const known: Instance = {
      id: "inst-1",
      label: "magicbook",
      addresses: ["ws://10.0.0.4:2420/ws", ORIGIN],
      agentId: "machine",
    };
    const { instances, instance } = instanceForAddress([known], ORIGIN);
    expect(instance.addresses).toEqual([ORIGIN, "ws://10.0.0.4:2420/ws"]);
    expect(instance.label).toBe("magicbook");
    expect(instances).toHaveLength(1);
  });

  test("other records are left alone", () => {
    const other = newInstance("ws://10.0.0.9:2420/ws");
    const { instances } = instanceForAddress([other], ORIGIN);
    expect(instances).toHaveLength(2);
    expect(instances[0]).toBe(other);
  });
});
