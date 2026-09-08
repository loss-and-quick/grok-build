import { useNavigate, useParams, useSearchParams } from "@solidjs/router";
import {
  createEffect,
  createSignal,
  For,
  on,
  onCleanup,
  onMount,
  Show,
  untrack,
  type JSX,
} from "solid-js";
import { THEMES, type ThemeName } from "@grok-build/theme";

import { AuthCard } from "./components/AuthCard.tsx";
import { DirectoryPicker } from "./components/DirectoryPicker.tsx";
import { FolderTrustCard } from "./components/FolderTrustCard.tsx";
import { InstanceMenu } from "./components/InstanceMenu.tsx";
import { Roster } from "./components/Roster.tsx";
import { Session } from "./components/Session.tsx";
import { Settings } from "./components/Settings.tsx";
import {
  RETRY_LIMIT,
  gatewayHost,
  retryDelayMs,
  watchLink,
  type GatewayClient,
} from "./client.ts";
import { createGateway, remember, remembered, type Gateway } from "./gateway.ts";
import {
  STATE_ROLE,
  currentInstanceId,
  defaultInstanceId,
  forgetInstance,
  forgetSecret,
  hostOf,
  instanceState,
  loadInstances,
  newInstance,
  rememberConnection,
  rememberSecret,
  saveInstances,
  sessionHref,
  secretFor,
  setCurrentInstanceId,
  setDefaultInstanceId,
  storageFault,
  type Instance,
  type InstanceState,
} from "./instances.ts";
import { railEnabled } from "./rail.ts";
import { shortSessionName } from "./roster.ts";
import { applyTheme, cssVarName, themeByName } from "./theme.ts";

const gateway: Gateway = createGateway();

/** The address a gateway answers on unless it was told otherwise. */
const DEFAULT_GATEWAY = "ws://127.0.0.1:2420/ws";

/**
 * The width below which the navigator stops being a column.
 *
 * Measured, not borrowed: the navigator holds at 240px by its own `minmax`, and
 * what runs out first is the transcript beside it — a session title, a path and
 * a wrapped tool line stop being readable before the column does. The number is
 * repeated in `styles.css`, where the media query lives; it is here because the
 * drawer has to close itself when the window crosses it.
 */
const DRAWER_MAX_PX = 760;

/**
 * The address the link names before anything has connected.
 *
 * The instance a fresh tab opens on, falling back to whichever is remembered
 * first and then to the address a gateway answers on unless it was told
 * otherwise. This used to be a single stored `url`, which is the whole reason
 * this client could remember exactly one machine.
 */
function startingEndpoint(): string {
  const list = loadInstances();
  const preferred = defaultInstanceId() ?? currentInstanceId();
  const instance = list.find((candidate) => candidate.id === preferred) ?? list[0];
  return instance?.addresses[0] ?? DEFAULT_GATEWAY;
}

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
  const [endpoint, setEndpoint] = createSignal(startingEndpoint());

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
   *
   * It runs after *every* successful connect, including the first, so on a page
   * opened at `/s/<id>` it asks for a session {@link SessionRoute} has already
   * asked for. That is a no-op because `attach` refuses a session it is already
   * on *on this socket*; a plain "is it attached" test would refuse the
   * reconnect this exists for.
   *
   * The id is taken *before* the connect, because the connect drops the session
   * on screen — it belonged to the socket that closed. Reading it afterwards is
   * what made this the only thing standing between a reconnect and a blank
   * page, and it is also why the note below can be honest: a session missing
   * from the roster of the *same* gateway is a session the leader no longer
   * has, while one missing after an address change is simply somewhere else.
   */
  const resume = async (id: string | undefined, moved: boolean): Promise<void> => {
    if (!id) return;
    const entry = target.roster.get(id);
    if (!entry) {
      if (!moved) {
        setNote(`Session ${id} is no longer on the leader.`);
      }
      return;
    }
    await target.attach(entry);
  };

  const tryOnce = async (url: string, secret: string): Promise<boolean> => {
    const was = target.attached()?.entry.sessionId;
    const moved = url !== endpoint();
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
    await resume(was, moved);
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

// ---------------------------------------------------------------------------
// The instances this browser knows
//
// One live socket, switched in sequence, because on this wire a connection is a
// sign-in and not a subscription (`instances.ts`). What is held here is
// therefore a *list of places* and a pointer at one of them, not a set of live
// connections.
// ---------------------------------------------------------------------------

const [instances, setInstances] = createSignal<Instance[]>(loadInstances());
const [currentId, setCurrentId] = createSignal<string | null>(currentInstanceId());
const [defaultId, setDefaultId] = createSignal<string | null>(defaultInstanceId());

const currentInstance = (): Instance | undefined =>
  instances().find((instance) => instance.id === currentId());

const instanceById = (id: string | undefined): Instance | undefined =>
  id ? instances().find((instance) => instance.id === id) : undefined;

/**
 * Connect to one instance, deliberately.
 *
 * The only path to a socket. Switching sets the pointer *before* the attempt,
 * so a failure leaves the page saying which machine it failed to reach rather
 * than still naming the one it left — and `connect` drops the attached session
 * on its first line, so instance A's transcript can never sit under instance
 * B's roster.
 */
async function openInstance(instance: Instance, secret?: string): Promise<boolean> {
  const address = instance.addresses[0] ?? "";
  if (!address) return false;
  setCurrentId(instance.id);
  setCurrentInstanceId(instance.id);
  return link.open(address, secret ?? secretFor(instance.id));
}

/** The state the instance line and the menu draw. */
const currentState = (): InstanceState =>
  instanceState(link.phase(), gateway.auth.status === "settled", currentInstance()?.agentId !== undefined);

/**
 * What to call one instance in a sentence that names another.
 *
 * Two machines can carry one hostname — two agents on one box, or two boxes
 * named the same — and "no session here, it is on <name>" reads as nonsense
 * when both halves are the same word. The address is what tells them apart, so
 * it is added exactly when the name does not.
 */
function instanceName(instance: Instance): string {
  const shared = instances().some(
    (other) => other.id !== instance.id && other.label === instance.label,
  );
  return shared ? `${instance.label} (${hostOf(instance.addresses[0] ?? "")})` : instance.label;
}

function renameInstance(id: string, label: string): void {
  if (!label) return;
  const next = instances().map((instance) =>
    instance.id === id ? { ...instance, label } : instance,
  );
  setInstances(next);
  saveInstances(next);
}

function makeDefault(id: string): void {
  setDefaultId(id);
  setDefaultInstanceId(id);
}

function forget(id: string): void {
  const left = forgetInstance(instances(), id);
  setInstances(left);
  if (currentId() === id) setCurrentId(null);
  if (defaultId() === id) setDefaultId(null);
}

/**
 * This browser's own answer about the widget rail, or `null` for "not asked".
 *
 * The gate itself belongs to the agent: `dock_enabled` rides
 * `x.ai/settings/update` to every attached client, and until now this page threw
 * that notification away, so a cohort the dock was turned on for saw nothing of
 * it here. This is the layer beneath it, and the pager has the same one — it
 * resolves the feature through pin, environment, config file, then the cohort
 * flag (`pager/src/app/mod.rs:199-209`), and a `[features] dock` in a file on
 * that machine outranks the rollout. A browser has no such file; storage is what
 * it has instead, and it sits in the same place in the order.
 */
const [railLocal, setRailLocal] = createSignal<boolean | null>(storedRail());

function storedRail(): boolean | null {
  const stored = remembered("rail");
  return stored === "" ? null : stored === "on";
}

const railShown = (): boolean => railEnabled(railLocal(), gateway.dockEnabled());

/**
 * The screen.
 *
 * The route is the attached session, which is what makes this a web client
 * rather than a page: `/s/<id>` survives a reload, a bookmark and a second tab,
 * and the leader treats each tab as its own client, so two tabs on one session
 * is an ordinary multi-client case rather than a special one.
 */
export function App(props: { children?: JSX.Element }): JSX.Element {
  // Storage is the record and these signals are a view of it, so the view is
  // taken here — before the effects below are created, not inside `onMount`
  // after they have already run once. An effect that writes the list before
  // anything has read it saves a view of a store it has not looked at.
  setInstances(loadInstances());
  setCurrentId(currentInstanceId());
  setDefaultId(defaultInstanceId());

  const [theme, setTheme] = createSignal<ThemeName>(
    (remembered("theme", "groknight") as ThemeName) ?? "groknight",
  );

  // Whether the navigator is open as a drawer. Only reachable below
  // `DRAWER_MAX_PX`, where the stylesheet shows the button that sets it.
  const [drawer, setDrawer] = createSignal(false);
  let content: HTMLElement | undefined;
  let navigator: HTMLElement | undefined;
  let opener: HTMLButtonElement | undefined;
  // The previous value, so focus is handed back on a close and not on every
  // unrelated re-run of the effect below.
  let was = false;

  createEffect(() => {
    applyTheme(document.documentElement, themeByName(theme()));
  });

  /**
   * Fold what `initialize` said into the remembered list.
   *
   * This is where an address becomes a machine. Two records carrying one
   * `agentId` collapse here — `127.0.0.1:2420` and the address the same
   * machine answers on over the network are one instance with two addresses —
   * and the credential of the record that was absorbed goes with it, since a
   * machine is one place to sign in to.
   *
   * It runs on reconnects too, not only on deliberate switches, which is what
   * keeps "when we last saw it" honest for a tab that has been open all day.
   */
  createEffect(() => {
    const identity = gateway.identity();
    if (!identity.agentId || link.phase() !== "live") return;
    untrack(() => {
      const id = currentId();
      if (!id) return;
      const folded = rememberConnection(instances(), {
        id,
        address: link.endpoint(),
        identity,
        at: Date.now(),
        sessionCount: gateway.roster.all().length,
      });
      for (const absorbed of folded.merged) forgetSecret(absorbed);
      setInstances(folded.instances);
      saveInstances(folded.instances);
      // A record that has just been absorbed is not a place any more, so
      // anything pointing at it has to move to the record that survived —
      // otherwise "the instance new tabs open on" names an id that is no
      // longer in the list, and the next tab opens on nothing.
      if (folded.merged.includes(defaultId() ?? "")) makeDefault(folded.id);
      if (folded.merged.includes(currentId() ?? "")) {
        setCurrentId(folded.id);
        setCurrentInstanceId(folded.id);
      }
      // The first machine this browser reaches is the one a fresh tab should
      // open on. After that it is a choice, and choices are made in the menu.
      if (defaultId() === null) makeDefault(folded.id);
    });
  });

  // Which session was last open on this instance, so a switch back can offer
  // it and a `/s/<id>` for another machine can name where it belongs.
  createEffect(() => {
    const sessionId = gateway.attached()?.entry.sessionId;
    if (!sessionId) return;
    untrack(() => {
      const id = currentId();
      const next = instances().map((instance) =>
        instance.id === id ? { ...instance, lastSessionId: sessionId } : instance,
      );
      setInstances(next);
      saveInstances(next);
    });
  });

  onMount(() => {
    // Reconnect on load, to the instance this browser opens tabs on rather
    // than to whichever one the last tab happened to be looking at — a
    // distinction that matters because connecting here means signing in.
    // With no pointer at all — the first load after the single remembered
    // address was migrated into a list — the only instance there is, is the one
    // meant. With several and no answer, none: connecting is signing in, and
    // guessing which machine to sign in to is not a guess this page may make.
    const list = instances();
    const opening =
      instanceById(defaultId() ?? currentId() ?? undefined) ??
      (list.length === 1 ? list[0] : undefined);
    if (opening && secretFor(opening.id)) void openInstance(opening);

    // A drawer that is open when the window grows past the breakpoint would
    // leave the page it made `inert` inert for good, with nothing on screen to
    // undo it. So the breakpoint is watched rather than only styled.
    const wide = window.matchMedia(`(min-width: ${DRAWER_MAX_PX}px)`);
    const onWide = (): void => {
      if (wide.matches) setDrawer(false);
    };
    wide.addEventListener("change", onWide);
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape" && drawer()) setDrawer(false);
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      wide.removeEventListener("change", onWide);
      document.removeEventListener("keydown", onKey);
    });
  });

  /**
   * What the drawer does to the rest of the page.
   *
   * `inert` rather than `aria-modal`: the navigator is the same list of
   * sessions it is at any other width, not a dialog, and `aria-modal` is a
   * promise about focus that would have to be paid for with a trap
   * (see `focus.ts`). `inert` is not a promise — it is the browser actually
   * taking the content out of reach of both Tab and the reader — so the drawer
   * contains focus without claiming to be something it is not.
   */
  createEffect(() => {
    content?.toggleAttribute("inert", drawer());
    if (drawer()) navigator?.querySelector<HTMLElement>("button, input, select")?.focus();
    else if (was && !drawer()) opener?.focus();
    was = drawer();
  });

  return (
    <div class="layout" classList={{ "drawer-open": drawer() }}>
      {/* Below 760px the navigator is a drawer, and this is what opens it. It
          exists at every width and is hidden by the stylesheet above that one,
          because a button that comes and goes with the window changes the tab
          order under a keyboard user's hands. */}
      <button
        class="drawer-button"
        type="button"
        aria-expanded={drawer()}
        aria-controls="navigator"
        ref={opener}
        onClick={() => setDrawer(!drawer())}
      >
        Sessions
      </button>
      <aside class="sidebar" id="navigator" ref={navigator}>
        <Connection />
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
        <RailToggle />
        <Settings gateway={gateway} />
      </aside>
      <Show when={drawer()}>
        <div class="drawer-scrim" onClick={() => setDrawer(false)} />
      </Show>
      <main class="main" ref={content}>
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
              if (sessionId) navigate(sessionHref(sessionId, currentId()));
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
 * The connection, as a line rather than a form.
 *
 * The form was on screen permanently — address, secret and a button above the
 * status line, the theme picker and the new-session button — which pushed the
 * roster, the thing this page is for, a third of the way down the window on a
 * client that had no further use for any of it. Connected, this is one line: a
 * dot, where the connection goes, and the way out of it. The form comes back
 * when it is wanted, so changing endpoint stays a deliberate act rather than
 * two fields that are always one keystroke from being wrong.
 *
 * The state is written as well as coloured. "Retrying" and "gave up" look the
 * same in every palette, and the difference between them is the only thing a
 * person can act on.
 */
function Connection(): JSX.Element {
  const [editing, setEditing] = createSignal(false);
  const [menu, setMenu] = createSignal(false);
  const collapsed = (): boolean => !editing() && link.phase() !== "offline";
  const retrying = (): boolean => link.phase() === "waiting" || link.phase() === "lost";
  /**
   * What to call the machine on the other end.
   *
   * The hostname when there is one, because that is what a person calls a
   * machine; the host and port only until `initialize` has said. The address
   * stays in the tooltip — it is how you reach it, not what it is.
   */
  const name = (): string =>
    currentInstance()?.label ?? gatewayHost(link.endpoint());

  return (
    <Show when={collapsed()} fallback={<ConnectForm onDone={() => setEditing(false)} />}>
      <div class="link" data-phase={link.phase()}>
        <span
          class="link-dot"
          aria-hidden="true"
          style={{ background: `var(${cssVarName(STATE_ROLE[currentState()])})` }}
        />
        <button
          class="link-where"
          type="button"
          aria-haspopup="menu"
          aria-expanded={menu()}
          title={`${link.endpoint()} — press for the instance menu`}
          onClick={() => setMenu(!menu())}
        >
          {name()}
        </button>
        {/* Two states this line has always conflated. A socket that is up with
            no credential behind it refuses every session on the roster under
            it, and the line said "connected" for both. */}
        <Show when={link.phase() !== "live" || currentState() === "unauthenticated"}>
          <span class="link-state">
            {currentState() === "unauthenticated" ? "signed out" : linkLine()}
          </span>
        </Show>
        <Show when={retrying()}>
          <button class="link-retry" type="button" onClick={() => link.retryNow()}>
            Retry now
          </button>
        </Show>
        <button
          class="link-hangup"
          type="button"
          onClick={() => {
            setEditing(false);
            link.hangUp();
          }}
        >
          {link.phase() === "live" ? "Disconnect" : "Stop"}
        </button>
      </div>
      <Show when={menu()}>
        <InstanceMenu
          instances={instances()}
          currentId={currentId()}
          defaultId={defaultId()}
          state={currentState()}
          onSwitch={(instance) => {
            setMenu(false);
            // A switch is a connect, and a connect is a sign-in. Nothing here
            // happens without this press.
            void openInstance(instance);
          }}
          onRename={(id, label) => renameInstance(id, label)}
          onForget={(id) => forget(id)}
          onMakeDefault={(id) => makeDefault(id)}
          onAdd={() => {
            setMenu(false);
            setEditing(true);
          }}
          onClose={() => setMenu(false)}
        />
      </Show>
      <Show when={link.note() || retrying() || storageFault()}>
        <p class="link-note">
          {link.note()}{" "}
          <Show when={link.phase() === "lost"}>
            It will try again when this tab is focused or the network returns.
          </Show>
          {/* A theme that fails to save is forgotten and retyped in a second; a
              list of machines that fails to save is a machine somebody added
              and will not find again. So this one is said out loud rather than
              swallowed the way the old three keys were. */}
          <Show when={storageFault()}>
            {" "}
            This browser refused to save the instance list, so it will be
            forgotten when the tab closes.
          </Show>
        </p>
      </Show>
    </Show>
  );
}

/**
 * An address and its secret: how a machine is added, and how one is re-entered.
 *
 * The secret is remembered and never rendered back. Remembering it is what
 * makes `/s/<id>` a real URL — a bookmark opened in a second tab has to reach
 * the gateway before it can attach to anything — and `localStorage` is the only
 * store that survives that, `sessionStorage` being per tab. But a filled
 * password field on every reload puts a credential for the whole machine's
 * agent into the DOM of every page load, and buys nothing: the value is already
 * known, and typing is only needed when it changes. So the field starts empty
 * and says that a secret is remembered, and forgetting one is its own button.
 *
 * A typed address does not decide whether this is a new machine. That is
 * settled after `initialize`, by `agentId`: this form only ever creates a
 * record when no remembered one already carries the address.
 */
function ConnectForm(props: { onDone: () => void }): JSX.Element {
  const known = (): Instance | undefined =>
    currentInstance() ?? instances().find((instance) => instance.addresses.length > 0);
  const [saved, setSaved] = createSignal(secretFor(known()?.id ?? "") !== "");
  let urlField!: HTMLInputElement;
  let secretField!: HTMLInputElement;

  return (
    <form
      class="connect"
      onSubmit={(event) => {
        event.preventDefault();
        const url = urlField.value.trim();
        // An address already known is that machine being reconnected, not a
        // second one. Which machines are the same is `agentId`'s answer, but
        // this much is free and keeps a re-typed address from forking the list
        // before the merge can unfork it.
        const existing = instances().find((instance) => instance.addresses.includes(url));
        const instance = existing ?? newInstance(url);
        const secret = secretField.value || secretFor(instance.id);
        if (!existing) {
          const next = [...instances(), instance];
          setInstances(next);
          saveInstances(next);
        }
        rememberSecret(instance.id, secret);
        setSaved(secret !== "");
        props.onDone();
        void openInstance(instance, secret);
      }}
    >
      <input
        class="connect-url"
        type="text"
        ref={urlField}
        placeholder={DEFAULT_GATEWAY}
        value={known()?.addresses[0] ?? DEFAULT_GATEWAY}
      />
      <input
        class="connect-secret"
        type="password"
        ref={secretField}
        autocomplete="off"
        placeholder={saved() ? "using the remembered secret" : "gateway secret"}
      />
      <div class="connect-buttons">
        <button class="connect-button" type="submit">
          Connect
        </button>
        <Show when={saved()}>
          <button
            class="connect-forget"
            type="button"
            onClick={() => {
              const instance = known();
              if (instance) forgetSecret(instance.id);
              setSaved(false);
              secretField.focus();
            }}
          >
            Forget secret
          </button>
        </Show>
        {/* Opening the form must not commit anyone to reconnecting: on a live
            link, pressing Connect would hang the socket up and build another. */}
        <Show when={link.phase() !== "offline"}>
          <button class="connect-cancel" type="button" onClick={() => props.onDone()}>
            Cancel
          </button>
        </Show>
      </div>
    </form>
  );
}

/**
 * This browser's say in whether the rail is drawn.
 *
 * Not a settings row: `x.ai/settings/list` does not carry one, and a control
 * that pretended to write a setting the agent has no key for would be a lie
 * about where the state lives. What it is instead is the browser's copy of
 * `[features] dock` in a `config.toml` — a local answer that outranks the
 * cohort flag, exactly as the pager's own ladder has it. The label says which
 * way the agent has voted, because a switch whose default comes from elsewhere
 * is unreadable without that.
 */
function RailToggle(): JSX.Element {
  const remote = (): boolean | null => gateway.dockEnabled();
  const said = (): string =>
    remote() === null ? "the agent has not said" : remote() ? "the agent says on" : "the agent says off";

  return (
    <label class="rail-toggle">
      <input
        type="checkbox"
        checked={railShown()}
        onChange={(event) => {
          const on = event.currentTarget.checked;
          remember("rail", on ? "on" : "off");
          setRailLocal(on);
        }}
      />
      Widget rail <span class="rail-toggle-note">({said()})</span>
    </label>
  );
}

function RosterPane(): JSX.Element {
  const params = useParams<{ sessionId?: string }>();
  return <Roster gateway={gateway} current={params.sessionId} instance={currentId()} />;
}

/**
 * `/s/<id>` naming a session this instance does not have.
 *
 * Session ids are unique on a leader, not across leaders, so a link is only
 * half an address: it says which session and not which machine. New links carry
 * `?i=<instance>` and old ones do not, which is the whole reason that form was
 * chosen over `/i/<instance>/s/<session>` — every bookmark already written keeps
 * working, and reads as "on whichever instance this tab is on".
 *
 * **The hint never connects.** A URL that could make a browser sign in to an
 * agent on a machine is not navigation, it is an action, so `?i=` changes what
 * this says and offers a button. The press is a person's.
 */
function MissingSession(props: { sessionId: string; wanted: string | undefined }): JSX.Element {
  const named = (): Instance | undefined => instanceById(props.wanted);
  const elsewhere = (): Instance | undefined => {
    const instance = named();
    return instance && instance.id !== currentId() ? instance : undefined;
  };

  return (
    <div class="empty missing-session">
      <p>
        No session {shortSessionName(props.sessionId)} on{" "}
        {(() => {
          const here = currentInstance();
          return here ? instanceName(here) : gatewayHost(link.endpoint());
        })()}
        .
      </p>
      <Show
        when={elsewhere()}
        fallback={<p>Pick a session on the left, or start one with New session.</p>}
      >
        {(instance) => (
          <p>
            This link was written on {instanceName(instance())}.{" "}
            <button
              class="missing-switch"
              type="button"
              onClick={() => void openInstance(instance())}
            >
              Switch to {instanceName(instance())}
            </button>
          </p>
        )}
      </Show>
    </div>
  );
}

/** `/` — connected but not attached. */
export function Home(): JSX.Element {
  return <Session gateway={gateway} rail={railShown()} />;
}

/**
 * `/s/:sessionId` — attach to the session named in the URL.
 *
 * The attach is driven by the route, not by the click, so arriving by
 * bookmark, reload or back-button behaves exactly like arriving by click. It
 * waits for the roster because `session/load` needs the row's `cwd`, which
 * only the roster carries — and it re-runs on every roster upsert for this
 * session, because the row is a store entry and the effect tracks it.
 *
 * **Whether that ask is a no-op is not decided here.** This route and
 * {@link Link.resume} are two independent reasons to attach, and they overlap
 * exactly once — a page opened straight at `/s/<id>`, where the roster arrives
 * inside `connect` and fires this effect before `resume` runs. Both then asked,
 * and two `session/load`s on one socket are two replays of the same transcript
 * into one view: every turn on screen twice. Guarding it here as well would put
 * the same rule in two places and let them disagree, which is how it went wrong
 * the first time — a guard reading `attached()` cannot tell "already on this
 * session" from "was on this session, on a socket that has since died". The
 * gateway owns it, keyed on the socket as well as the session.
 */
export function SessionRoute(): JSX.Element {
  const params = useParams<{ sessionId: string }>();
  const [search] = useSearchParams<{ i?: string }>();
  createEffect(
    on([() => params.sessionId, () => gateway.roster.get(params.sessionId)], ([id, entry]) => {
      if (!id || !entry) return;
      void gateway.attach(entry);
    }),
  );
  // Only once the link is up: an empty roster during a connect is not evidence
  // that the session is missing, it is evidence that nothing has been asked
  // yet.
  const missing = (): boolean =>
    link.phase() === "live" && gateway.roster.get(params.sessionId) === undefined;
  return (
    <Session
      gateway={gateway}
      rail={railShown()}
      fallback={
        <Show when={missing()} fallback={<p class="empty">Pick a session on the left.</p>}>
          <MissingSession sessionId={params.sessionId} wanted={search.i} />
        </Show>
      }
    />
  );
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
        if (first) navigate(sessionHref(first.sessionId, currentId()), { replace: true });
      },
    ),
  );
  return <Session gateway={gateway} rail={railShown()} />;
}
