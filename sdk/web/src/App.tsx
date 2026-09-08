import { useNavigate, useParams } from "@solidjs/router";
import { createEffect, createSignal, For, on, onMount, Show, type JSX } from "solid-js";
import { THEMES, type ThemeName } from "@grok-build/theme";

import { AuthCard } from "./components/AuthCard.tsx";
import { DirectoryPicker } from "./components/DirectoryPicker.tsx";
import { FolderTrustCard } from "./components/FolderTrustCard.tsx";
import { Roster } from "./components/Roster.tsx";
import { Session } from "./components/Session.tsx";
import { Settings } from "./components/Settings.tsx";
import { RETRY_LIMIT, retryDelayMs, watchLink, type GatewayClient } from "./client.ts";
import { createGateway, remember, remembered, type Gateway } from "./gateway.ts";
import { applyTheme, themeByName } from "./theme.ts";

const gateway: Gateway = createGateway();

/** The address a gateway answers on unless it was told otherwise. */
const DEFAULT_GATEWAY = "ws://127.0.0.1:2420/ws";

/**
 * Where the link stands, which is not the same question as where the last
 * request stands.
 *
 * `"live"` means a socket is open *and* the handshake behind it finished, so it
 * is the only state in which this page may offer to do anything. Everything
 * else is a way of not being connected, and each says which way: `"offline"` is
 * nobody trying, `"connecting"` is an attempt in flight, `"waiting"` is the gap
 * between attempts, and `"lost"` is the page having stopped.
 */
export type LinkPhase = "offline" | "connecting" | "live" | "waiting" | "lost";

export interface Link {
  phase: () => LinkPhase;
  /** Which attempt of {@link RETRY_LIMIT} is running or pending. */
  attempt: () => number;
  /** What happened to the link, in words, when that is not obvious. */
  note: () => string;
  /** The gateway address this link is on, or was last on. */
  endpoint: () => string;
  /** Connect deliberately. A failure here is final: it is not retried. */
  open: (url: string, secret: string) => Promise<boolean>;
  /** Hang up, and stop retrying. */
  hangUp: () => void;
  /** Start the ladder again from the top. */
  retryNow: () => void;
  dispose: () => void;
}

/**
 * Supervise the gateway link.
 *
 * Two things were wrong and they are one thing: the page called itself
 * connected after the socket closed, and it never tried to open another. A
 * status that cannot go backwards is worse than no status, because the screen
 * keeps every affordance a working connection has — a roster to click, a
 * composer to type into — over a link that will refuse all of them.
 *
 * So the socket, not the last successful request, is what this believes
 * (`watchLink` in `client.ts`), and a link that drops is chased on a bounded
 * ladder. Bounded is the point: a tab left open overnight against a stopped
 * leader must stop knocking, and a page that says "reconnecting…" for eight
 * hours has only replaced one lie with another. When the ladder runs out the
 * page says so and offers the button, and it wakes on its own the moment the
 * tab is looked at again or the machine's network comes back — which is the
 * case the ceiling would otherwise punish, a laptop that slept.
 */
export function createLink(
  target: Gateway,
  // The ladder's own waits, injectable only so a test can walk the whole of it
  // without spending the minute and a half it is deliberately designed to take.
  wait: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
): Link {
  const [phase, setPhase] = createSignal<LinkPhase>("offline");
  const [attempt, setAttempt] = createSignal(0);
  const [note, setNote] = createSignal("");
  const [endpoint, setEndpoint] = createSignal(remembered("url", DEFAULT_GATEWAY));

  // The socket this supervisor is responsible for. A superseded socket's
  // `close` arrives *after* its replacement has opened, so the identity is what
  // separates "the link died" from "the link was replaced".
  let watched: GatewayClient | null = null;
  // What the last attempt used, so a retry repeats what worked rather than
  // re-reading storage a second tab may have changed underneath it.
  let credentials: { url: string; secret: string } | null = null;
  // Bumped by anything that supersedes a ladder in progress.
  let generation = 0;

  /**
   * Attach again to the session that was on screen.
   *
   * Automatic, and safe to be automatic: `attach` builds a *new* transcript and
   * `session/load` replays the session into it, so the reconnected page shows
   * the session as the leader now has it rather than the read transcript with a
   * second copy appended. What a person loses is their scroll position; what
   * they would lose by not re-attaching is every message the session produced
   * while the link was down, with nothing on screen to say they were missing.
   */
  const resume = async (): Promise<void> => {
    const id = target.attached()?.entry.sessionId;
    if (!id) return;
    const entry = target.roster.get(id);
    if (!entry) {
      setNote(`Session ${id} is no longer on the leader; the transcript above is the old one.`);
      return;
    }
    await target.attach(entry);
  };

  const tryOnce = async (url: string, secret: string): Promise<boolean> => {
    credentials = { url, secret };
    setEndpoint(url);
    setPhase("connecting");
    try {
      await target.connect(url, secret);
    } catch {
      return false;
    }
    setPhase("live");
    setAttempt(0);
    setNote("");
    await resume();
    return true;
  };

  const ladder = async (): Promise<void> => {
    const mine = ++generation;
    const using = credentials;
    if (!using) {
      setPhase("offline");
      return;
    }
    for (let n = 1; n <= RETRY_LIMIT; n += 1) {
      setAttempt(n);
      const delay = retryDelayMs(n);
      if (delay > 0) {
        setPhase("waiting");
        await wait(delay);
        if (mine !== generation) return;
      }
      if (await tryOnce(using.url, using.secret)) return;
      if (mine !== generation) return;
    }
    setPhase("lost");
  };

  const open = async (url: string, secret: string): Promise<boolean> => {
    generation += 1;
    setNote("");
    const opened = await tryOnce(url, secret);
    if (!opened) {
      // A deliberate connect that fails is not retried: the likeliest reason is
      // a wrong address or a wrong secret, and nine attempts at a wrong secret
      // is nine rejections and no new information. `status()` carries the
      // agent's own words for it.
      setPhase("offline");
      setAttempt(0);
    }
    return opened;
  };

  const hangUp = (): void => {
    generation += 1;
    setPhase("offline");
    setAttempt(0);
    setNote("");
    credentials = null;
    target.disconnect();
  };

  const stopWatching = watchLink((client, state) => {
    if (state === "opening") {
      watched = client;
      return;
    }
    if (state !== "closed" || client !== watched) return;
    watched = null;
    // Only a link that was working is chased. A socket that closed while an
    // attempt was still in flight is that attempt's own failure, and the caller
    // driving it decides what happens next.
    if (phase() !== "live") return;
    setNote("The gateway stopped answering.");
    void ladder();
  });

  const wake = (): void => {
    if (phase() === "waiting" || phase() === "lost") void ladder();
  };
  const onVisible = (): void => {
    if (!document.hidden) wake();
  };
  document.addEventListener("visibilitychange", onVisible);
  window.addEventListener("online", wake);

  return {
    phase,
    attempt,
    note,
    endpoint,
    open,
    hangUp,
    retryNow: () => void ladder(),
    dispose: () => {
      generation += 1;
      stopWatching();
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("online", wake);
    },
  };
}

const link: Link = createLink(gateway);

/**
 * The screen.
 *
 * The route is the attached session, which is what makes this a web client
 * rather than a page: `/s/<id>` survives a reload, a bookmark and a second tab,
 * and the leader treats each tab as its own client, so two tabs on one session
 * is an ordinary multi-client case rather than a special one.
 */
export function App(props: { children?: JSX.Element }): JSX.Element {
  const [theme, setTheme] = createSignal<ThemeName>(
    (remembered("theme", "groknight") as ThemeName) ?? "groknight",
  );

  createEffect(() => {
    applyTheme(document.documentElement, themeByName(theme()));
  });

  onMount(() => {
    // Reconnect on load when this browser already knows a gateway, so a
    // bookmarked session opens attached instead of at a login form.
    const url = remembered("url");
    const secret = remembered("secret");
    if (url && secret) void link.open(url, secret);
  });

  return (
    <div class="layout">
      <aside class="sidebar">
        <ConnectForm />
        <LinkLine />
        <div class="status">{gateway.status()}</div>
        <select
          class="theme-picker"
          value={theme()}
          onChange={(event) => {
            const name = event.currentTarget.value as ThemeName;
            remember("theme", name);
            setTheme(name);
          }}
        >
          <For each={Object.keys(THEMES) as ThemeName[]}>
            {(name) => <option value={name}>{THEMES[name].display_name}</option>}
          </For>
        </select>
        <NewSessionButton />
        <RosterPane />
        <Settings gateway={gateway} />
      </aside>
      <main class="main">
        {/* First, and above everything: with no credential of its own the
            agent refuses every session/new and session/load, so nothing below
            this can work until it is answered. */}
        <Show when={link.phase() === "live" && gateway.auth.status !== "settled"}>
          <AuthCard gateway={gateway} />
        </Show>
        {/* Above the session rather than inside it. The leader routes a
            folder-trust request to the client that opened the session and
            never replays it, so it can arrive before that session is attached
            or while another is on screen — and a card dropped for either
            reason is a project whose MCP servers, hooks, plugins and LSP go
            off without a word. */}
        <div class="trusts">
          <For each={gateway.folderTrusts}>
            {(pending) => <FolderTrustCard pending={pending} />}
          </For>
        </div>
        {props.children}
      </main>
    </div>
  );
}

/**
 * Open a session in a directory the roster has never mentioned.
 *
 * The roster's own "+ session here" can only reach a root some client already
 * opened, which made the browser a viewer of directories rather than a chooser
 * of them. This is the same gesture without that limit — and, like it, creating
 * and opening are one act, but only the route opens: navigating is what
 * attaches, so a new session arrives at a URL like every other.
 */
function NewSessionButton(): JSX.Element {
  const [picking, setPicking] = createSignal(false);
  const navigate = useNavigate();

  return (
    <>
      <button
        class="new-session"
        type="button"
        disabled={link.phase() !== "live" || gateway.auth.status !== "settled"}
        onClick={() => setPicking(true)}
      >
        New session…
      </button>
      <Show when={picking()}>
        <DirectoryPicker
          gateway={gateway}
          onClose={() => setPicking(false)}
          onOpen={(cwd) => {
            setPicking(false);
            void gateway.createSession(cwd).then((sessionId) => {
              if (sessionId) navigate(`/s/${sessionId}`);
            });
          }}
        />
      </Show>
    </>
  );
}

/** How the link reads in one line, including what it will do next. */
function linkLine(): string {
  const n = link.attempt();
  switch (link.phase()) {
    case "live":
      return "connected";
    case "connecting":
      return n > 1 ? `reconnecting — try ${n} of ${RETRY_LIMIT}` : "connecting…";
    case "waiting":
      return `reconnecting — try ${n} of ${RETRY_LIMIT}`;
    case "lost":
      return `gave up after ${RETRY_LIMIT} tries`;
    default:
      return "not connected";
  }
}

/**
 * Where the link stands, on screen.
 *
 * The screen has to be able to say "this is not working" and to say what it is
 * doing about it. A dot alone cannot: "reconnecting" and "gave up" look the
 * same in every colour, and the difference between them is the only thing a
 * person can act on.
 */
function LinkLine(): JSX.Element {
  const retrying = (): boolean => link.phase() === "waiting" || link.phase() === "lost";
  return (
    <>
      <div class="link" data-phase={link.phase()}>
        <span class="link-dot" aria-hidden="true" />
        <span class="link-state">{linkLine()}</span>
        <Show when={retrying()}>
          <button class="link-retry" type="button" onClick={() => link.retryNow()}>
            Retry now
          </button>
        </Show>
      </div>
      <Show when={link.note() || retrying()}>
        <p class="link-note">
          {link.note()}{" "}
          <Show when={link.phase() === "lost"}>
            It will try again when this tab is focused or the network returns.
          </Show>
        </p>
      </Show>
    </>
  );
}

function ConnectForm(): JSX.Element {
  const [url, setUrl] = createSignal(remembered("url", DEFAULT_GATEWAY));
  const [secret, setSecret] = createSignal(remembered("secret"));

  return (
    <form
      class="connect"
      onSubmit={(event) => {
        event.preventDefault();
        remember("url", url());
        remember("secret", secret());
        void link.open(url(), secret());
      }}
    >
      <input
        class="connect-url"
        type="text"
        placeholder={DEFAULT_GATEWAY}
        value={url()}
        onInput={(event) => setUrl(event.currentTarget.value)}
      />
      <input
        class="connect-secret"
        type="password"
        placeholder="gateway secret"
        value={secret()}
        onInput={(event) => setSecret(event.currentTarget.value)}
      />
      <button class="connect-button" type="submit">
        Connect
      </button>
    </form>
  );
}

function RosterPane(): JSX.Element {
  const params = useParams<{ sessionId?: string }>();
  return <Roster gateway={gateway} current={params.sessionId} />;
}

/** `/` — connected but not attached. */
export function Home(): JSX.Element {
  return <Session gateway={gateway} />;
}

/**
 * `/s/:sessionId` — attach to the session named in the URL.
 *
 * The attach is driven by the route, not by the click, so arriving by
 * bookmark, reload or back-button behaves exactly like arriving by click. It
 * waits for the roster because `session/load` needs the row's `cwd`, which
 * only the roster carries.
 */
export function SessionRoute(): JSX.Element {
  const params = useParams<{ sessionId: string }>();
  createEffect(
    on([() => params.sessionId, () => gateway.roster.get(params.sessionId)], ([id, entry]) => {
      if (!id || !entry) return;
      if (gateway.attached()?.entry.sessionId === id) return;
      void gateway.attach(entry);
    }),
  );
  return <Session gateway={gateway} />;
}

/** `/d/:cwd` — a directory. Attaches to its most recently changed session. */
export function DirectoryRoute(): JSX.Element {
  const params = useParams<{ cwd: string }>();
  const navigate = useNavigate();
  createEffect(
    on(
      () => gateway.roster.groups().find((g) => g.cwd === decodeURIComponent(params.cwd)),
      (group) => {
        const first = group?.sessions[0];
        if (first) navigate(`/s/${first.sessionId}`, { replace: true });
      },
    ),
  );
  return <Session gateway={gateway} />;
}
