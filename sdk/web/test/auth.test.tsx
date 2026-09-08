import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { render } from "@solidjs/testing-library";

import {
  authMethodKind,
  drivable,
  extractUserCode,
  interactiveMethods,
  NO_INTERACTIVE_METHOD,
  shortAddress,
  NO_METHODS,
  selectEagerMethod,
  startMode,
  startupNeedsLogin,
} from "../src/auth.ts";
import type { SocketLike } from "../src/client.ts";
import { AuthCard } from "../src/components/AuthCard.tsx";
import { createGateway, type Gateway } from "../src/gateway.ts";
import type { AuthMethod } from "../src/wire.ts";

const method = (id: string, name = id, meta?: Record<string, unknown>): AuthMethod => ({
  id,
  name,
  ...(meta ? { _meta: meta } : {}),
});

const GROK_COM = method("grok.com", "Grok");
const CACHED = method("cached_token", "cached_token");
const API_KEY = method("xai.api_key", "xai.api_key");

// ---------------------------------------------------------------------------
// The classification, transcribed from the agent
// ---------------------------------------------------------------------------

describe("classifying an advertised method", () => {
  test("every id the agent can mint, and one it cannot", () => {
    expect(authMethodKind("xai.api_key")).toBe("xai_api_key");
    expect(authMethodKind("cached_token")).toBe("cached_token");
    expect(authMethodKind("grok.com")).toBe("grok_com");
    expect(authMethodKind("oidc")).toBe("oidc");
    expect(authMethodKind("plugin-oauth:acme")).toBe("plugin_oauth");
    // Account-scoped ids carry the selector after `#` and are still the same
    // kind (`auth_method.rs`, `plugin_oauth_method_id`).
    expect(authMethodKind("plugin-oauth:acme#work")).toBe("plugin_oauth");
    expect(authMethodKind("webauthn.passkey")).toBe("unknown");
  });

  test("only the interactive kinds can start a login", () => {
    expect(interactiveMethods([API_KEY, CACHED, GROK_COM, method("plugin-oauth:acme")])).toEqual([
      GROK_COM,
      method("plugin-oauth:acme"),
    ]);
  });

  test("a method this build does not recognise is refused, not guessed at", () => {
    // The refusal is the honest half of the answer: an unknown id may want a
    // device code, a paste box or neither, and nothing on the wire says which.
    expect(drivable(method("webauthn.passkey"))).toBe(false);
    expect(drivable(GROK_COM)).toBe(true);
    expect(drivable(method("plugin-oauth:acme#work"))).toBe(true);
  });

  test("an external provider starts in command mode, everything else pending", () => {
    expect(startMode(method("grok.com", "Corp", { external_provider: true }))).toBe("command");
    expect(startMode(GROK_COM)).toBeNull();
  });
});

describe("choosing what to authenticate on", () => {
  test("the agent's own default wins when it is advertised", () => {
    // Re-deriving this precedence client-side is what the shell's comment says
    // has regressed OIDC refresh before.
    expect(selectEagerMethod([API_KEY, CACHED, GROK_COM], "cached_token")).toBe("cached_token");
  });

  test("a default the agent did not advertise is not used", () => {
    expect(selectEagerMethod([API_KEY, GROK_COM], "cached_token")).toBe("xai.api_key");
  });

  test("without a default, cached_token beats the first entry", () => {
    expect(selectEagerMethod([API_KEY, CACHED, GROK_COM], null)).toBe("cached_token");
  });

  test("an interactive method in first place means no credential was found", () => {
    // `build_auth_methods` orders every non-interactive credential ahead of the
    // login, so this is the agent saying it has nothing.
    expect(startupNeedsLogin([GROK_COM])).toBe(true);
    expect(startupNeedsLogin([CACHED, GROK_COM])).toBe(false);
    expect(startupNeedsLogin([])).toBe(false);
  });
});

describe("the sign-in address, as a link's label", () => {
  test("an authorize URL is labelled by where it goes, not by its query", () => {
    // Several hundred characters of PKCE challenge, nonce and scope list would
    // be the loudest thing on the card. The href keeps all of it.
    expect(
      shortAddress(
        "https://auth.x.ai/oauth2/authorize?response_type=code&code_challenge=abc&state=xyz",
      ),
    ).toBe("https://auth.x.ai/oauth2/authorize");
  });

  test("something that is not a URL is left alone rather than guessed at", () => {
    expect(shortAddress("not a url")).toBe("not a url");
  });
});

describe("the device code, parsed the way the terminal parses it", () => {
  test("read from the verification URL", () => {
    expect(extractUserCode("https://accounts.x.ai/oauth2/device?user_code=ABCD-EFGH")).toBe(
      "ABCD-EFGH",
    );
    expect(extractUserCode("https://x.ai/oauth2/device?user_code=WXYZ-1234&foo=bar")).toBe(
      "WXYZ-1234",
    );
  });

  test("a parameter merely ending in user_code is not this one", () => {
    expect(extractUserCode("https://x.ai/d?foo_user_code=BAD&user_code=GOOD")).toBe("GOOD");
  });

  test("absent, empty and escaped values yield nothing rather than a wrong code", () => {
    expect(extractUserCode("https://x.ai/oauth2/device")).toBeNull();
    expect(extractUserCode("https://x.ai/d?user_code=")).toBeNull();
    expect(extractUserCode("https://x.ai/d?user_code=AB%20CD")).toBeNull();
    expect(extractUserCode(null)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// The wiring, driven through the real gateway
// ---------------------------------------------------------------------------

interface Frame {
  id?: number;
  method?: string;
  params?: Record<string, unknown>;
}

/** A socket whose `initialize` reply each test composes. */
class FakeSocket implements SocketLike {
  sent: Frame[] = [];
  initialize: Record<string, unknown> = { protocolVersion: 1 };
  /** Method name to reply, or to an `Error` that is answered as a JSON-RPC error. */
  answers = new Map<string, unknown>();
  /** Requests the test wants to answer by hand, keyed by method. */
  held = new Map<string, number[]>();
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    const frame = JSON.parse(data) as Frame;
    this.sent.push(frame);
    if (frame.id === undefined || frame.method === undefined) return;
    if (this.held.has(frame.method)) {
      this.held.get(frame.method)!.push(frame.id);
      return;
    }
    const defaults: Record<string, unknown> = {
      initialize: this.initialize,
      authenticate: {},
      "_x.ai/sessions/list": { result: { sessions: [] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
    };
    const answer = this.answers.has(frame.method)
      ? this.answers.get(frame.method)
      : defaults[frame.method];
    if (answer === undefined) return;
    this.reply(frame.id, answer);
  }

  reply(id: number, answer: unknown): void {
    queueMicrotask(() => {
      const body =
        answer instanceof Error
          ? { error: { code: -32000, message: answer.message } }
          : { result: answer };
      this.receive(JSON.stringify({ jsonrpc: "2.0", id, ...body }));
    });
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

  calls(name: string): Frame[] {
    return this.sent.filter((f) => f.method === name);
  }
}

let socket: FakeSocket;
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  socket = new FakeSocket();
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return socket;
  };
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

const settle = (ms = 20): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

async function connect(
  authMethods: AuthMethod[],
  meta: Record<string, unknown> = {},
): Promise<Gateway> {
  socket.initialize = { protocolVersion: 1, authMethods, _meta: meta };
  const gateway = createGateway();
  await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
  await settle();
  return gateway;
}

describe("settling authentication on connect", () => {
  test("a cached credential is adopted on the agent's own choice", async () => {
    const gateway = await connect([CACHED, GROK_COM], { defaultAuthMethodId: "cached_token" });
    expect(socket.calls("authenticate")[0]?.params).toEqual({ methodId: "cached_token" });
    expect(gateway.auth.status).toBe("settled");
  });

  test("no credential means the card, not a browser nobody asked for", async () => {
    // `grok.com` first is the agent saying it found nothing. Authenticating
    // eagerly here would open a browser window on an unattended page load.
    const gateway = await connect([GROK_COM], { defaultAuthMethodId: null });
    expect(socket.calls("authenticate")).toHaveLength(0);
    expect(gateway.auth.status).toBe("needed");
    expect(gateway.auth.methods).toEqual([GROK_COM]);
  });

  test("no advertised method at all names where the credential has to go", async () => {
    // Fail-closed by construction: a `preferred_method` pin with nothing to
    // pin to builds an empty list, and no client can log in from here.
    const gateway = await connect([], { defaultAuthMethodId: null });
    expect(gateway.auth.blocked).toBe(NO_METHODS);
    expect(gateway.auth.blocked).toContain("XAI_API_KEY");
    expect(socket.calls("authenticate")).toHaveLength(0);
  });

  test("a restored plugin sign-in is adopted, never re-driven", async () => {
    // Authenticating on the restored method would re-run the plugin's
    // interactive flow, which is the login the restoration exists to spare.
    const gateway = await connect([GROK_COM, method("plugin-oauth:acme")], {
      restoredAuthMeta: { email: "someone@example.com" },
    });
    expect(socket.calls("authenticate")).toHaveLength(0);
    expect(gateway.auth.status).toBe("settled");
  });

  test("a failed eager authenticate with an api key does not open a login", async () => {
    // The shell owns the fallthrough between non-interactive methods, and an
    // advertised `xai.api_key` means `initialize` already installed a default,
    // so sessions still open. Promoting this to a browser login would be wrong.
    socket.answers.set("authenticate", new Error("probe failed"));
    const gateway = await connect([API_KEY, GROK_COM], { defaultAuthMethodId: "xai.api_key" });
    expect(gateway.auth.status).toBe("settled");
    expect(gateway.auth.error).toContain("probe failed");
  });

  test("a failed eager authenticate without one falls back to the login", async () => {
    socket.answers.set("authenticate", new Error("session expired"));
    const gateway = await connect([CACHED, GROK_COM], { defaultAuthMethodId: "cached_token" });
    expect(gateway.auth.status).toBe("needed");
  });
});

describe("driving an interactive login", () => {
  async function loggingIn(): Promise<Gateway> {
    const gateway = await connect([GROK_COM], { defaultAuthMethodId: null });
    socket.held.set("authenticate", []);
    void gateway.login();
    await settle();
    return gateway;
  }

  test("authenticate carries the attempt's sequence and forces interaction", async () => {
    const gateway = await loggingIn();
    expect(socket.calls("authenticate")[0]?.params).toEqual({
      methodId: "grok.com",
      _meta: { request_seq: 1, force_interactive: true },
    });
    // `use_oauth` is not sent: it forces the loopback transport, and the
    // transport is the deployment's choice.
    expect(
      JSON.stringify(socket.calls("authenticate")[0]?.params),
    ).not.toContain("use_oauth");
    expect(gateway.auth.status).toBe("running");
  });

  test("the URL is collected alongside the call, because the call does not return", async () => {
    socket.answers.set("_x.ai/auth/get_url", {
      auth_url: "https://accounts.x.ai/oauth2/auth?state=abc",
      mode: "loopback",
    });
    const gateway = await loggingIn();
    expect(socket.calls("_x.ai/auth/get_url").length).toBeGreaterThan(0);
    expect(gateway.auth.url).toBe("https://accounts.x.ai/oauth2/auth?state=abc");
    expect(gateway.auth.mode).toBe("loopback");
  });

  test("an older agent's external_provider flag still means command mode", async () => {
    socket.answers.set("_x.ai/auth/get_url", {
      auth_url: "https://sso.corp/start",
      external_provider: true,
    });
    const gateway = await loggingIn();
    expect(gateway.auth.mode).toBe("command");
  });

  test("a pasted code goes back as the agent's own submit", async () => {
    const gateway = await loggingIn();
    socket.answers.set("_x.ai/auth/submit_code", { submitted: true });
    await gateway.submitAuthCode("  http://127.0.0.1:5051/callback?code=xyz  ");
    expect(socket.calls("_x.ai/auth/submit_code")[0]?.params).toEqual({
      code: "http://127.0.0.1:5051/callback?code=xyz",
    });
  });

  test("cancelling is scoped to the attempt, and discards its late result", async () => {
    const gateway = await loggingIn();
    socket.answers.set("_x.ai/auth/cancel", { cancelled: true });
    gateway.cancelLogin();
    await settle();
    // Scoped by `request_seq`, so a cancel that arrives late cannot tear down a
    // login that already replaced this one.
    expect(socket.calls("_x.ai/auth/cancel")[0]?.params).toEqual({ request_seq: 1 });
    expect(gateway.auth.status).toBe("needed");

    // The abandoned attempt now succeeds anyway. It must not resurrect itself.
    socket.reply(socket.held.get("authenticate")![0]!, {});
    await settle();
    expect(gateway.auth.status).toBe("needed");
  });

  test("a login the agent refuses reports the agent's own words", async () => {
    const gateway = await connect([GROK_COM], { defaultAuthMethodId: null });
    socket.answers.set("authenticate", new Error("Authentication cancelled"));
    await gateway.login();
    await settle();
    expect(gateway.auth.status).toBe("needed");
    expect(gateway.auth.error).toContain("Authentication cancelled");
  });

  test("a method this client cannot drive is refused before any call", async () => {
    const gateway = await connect([GROK_COM, method("webauthn.passkey", "Passkey")], {
      defaultAuthMethodId: null,
    });
    await gateway.login("webauthn.passkey");
    expect(socket.calls("authenticate")).toHaveLength(0);
    expect(gateway.auth.blocked).toContain("grok login");
    expect(gateway.auth.blocked).toContain("webauthn.passkey");
  });

  test("an agent advertising only a method this build cannot drive says so", async () => {
    // Not a hypothetical: the shell adds auth methods, and a browser built
    // before one of them exists must not offer a login screen that cannot
    // finish. It classifies as non-interactive, so the eager path tries it —
    // and when that fails there is no button to draw.
    socket.answers.set("authenticate", new Error("unsupported auth method"));
    const gateway = await connect([method("webauthn.passkey", "Passkey")], {
      defaultAuthMethodId: null,
    });
    expect(gateway.auth.status).toBe("needed");
    expect(gateway.auth.blocked).toContain("webauthn.passkey");
  });

  test("an agent whose only credentials are its own says where to fix them", async () => {
    socket.answers.set("authenticate", new Error("session expired"));
    const gateway = await connect([CACHED], { defaultAuthMethodId: "cached_token" });
    expect(gateway.auth.blocked).toBe(NO_INTERACTIVE_METHOD);
  });
});

// ---------------------------------------------------------------------------
// The card
// ---------------------------------------------------------------------------

describe("the sign-in card", () => {
  function mount(gateway: Gateway) {
    return render(() => <AuthCard gateway={gateway} />);
  }

  test("offers each advertised login by the agent's own name for it", async () => {
    const gateway = await connect([GROK_COM, method("plugin-oauth:acme", "Acme")], {
      defaultAuthMethodId: null,
    });
    const { container } = mount(gateway);
    expect([...container.querySelectorAll(".auth-method-name")].map((n) => n.textContent)).toEqual(
      ["Sign in with Grok", "Sign in with Acme"],
    );
  });

  test("a method it cannot drive is never drawn as a button", async () => {
    // The refusal is whole rather than per-row: an unrecognised method is not a
    // login this card can offer at all, so it says so once and offers nothing.
    socket.answers.set("authenticate", new Error("unsupported auth method"));
    const gateway = await connect([method("webauthn.passkey", "Passkey")], {
      defaultAuthMethodId: null,
    });
    const { container } = mount(gateway);
    expect(container.querySelector(".auth-method")).toBeNull();
    expect(container.querySelector(".auth-blocked")?.textContent).toContain("webauthn.passkey");
  });

  test("the device flow shows the code and offers no paste box", async () => {
    // Only the loopback flow reads a pasted code. A box here would swallow
    // whatever was typed into it.
    socket.answers.set("_x.ai/auth/get_url", {
      auth_url: "https://accounts.x.ai/oauth2/device?user_code=WXYZ-1234",
      mode: "device",
    });
    const gateway = await connect([GROK_COM], { defaultAuthMethodId: null });
    socket.held.set("authenticate", []);
    void gateway.login();
    await settle();
    const { container } = mount(gateway);
    expect(container.querySelector(".auth-code")?.textContent).toBe("WXYZ-1234");
    expect(container.querySelector(".auth-paste")).toBeNull();
    expect(container.querySelector<HTMLAnchorElement>(".auth-url")?.href).toContain(
      "user_code=WXYZ-1234",
    );
  });

  test("the loopback flow offers the paste box the agent actually reads", async () => {
    socket.answers.set("_x.ai/auth/get_url", {
      auth_url: "https://accounts.x.ai/oauth2/auth?state=abc",
      mode: "loopback",
    });
    const gateway = await connect([GROK_COM], { defaultAuthMethodId: null });
    socket.held.set("authenticate", []);
    void gateway.login();
    await settle();
    const { container } = mount(gateway);
    expect(container.querySelector(".auth-paste")).not.toBeNull();
    expect(container.querySelector(".auth-code")).toBeNull();
  });

  test("when no login is possible it says so instead of offering one", async () => {
    const gateway = await connect([], { defaultAuthMethodId: null });
    const { container } = mount(gateway);
    expect(container.querySelector(".auth-blocked")?.textContent).toContain("config.toml");
    expect(container.querySelector(".auth-method")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

test("no credential ever passes through this client", () => {
  // Two methods on the same wire would hand a browser the live bearer
  // (`x.ai/auth/getBearerToken`) or let it install one (`x.ai/setApiKey`).
  // Calling either would make a browser a second holder of the user's
  // credential, and `setApiKey` would additionally write `~/.grok/auth.json`
  // and the agent's process environment for every client on the leader. This
  // package must name neither.
  const forbidden = ["getBearerToken", "setApiKey"];
  const root = new URL("../src", import.meta.url).pathname;
  const walk = (dir: string): string[] =>
    readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
      entry.isDirectory() ? walk(join(dir, entry.name)) : [join(dir, entry.name)],
    );
  for (const file of walk(root)) {
    const text = readFileSync(file, "utf8");
    for (const name of forbidden) {
      // The wire module names both in a comment saying why they are unused.
      const mentions = text.split(name).length - 1;
      const explained = file.endsWith("wire.ts") ? mentions : 0;
      expect({ file, name, calls: mentions - explained }).toEqual({ file, name, calls: 0 });
    }
  }
});
