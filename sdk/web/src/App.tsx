import { useNavigate, useParams } from "@solidjs/router";
import { createEffect, createSignal, For, on, onMount, Show, type JSX } from "solid-js";
import { THEMES, type ThemeName } from "@grok-build/theme";

import { DirectoryPicker } from "./components/DirectoryPicker.tsx";
import { Roster } from "./components/Roster.tsx";
import { Session } from "./components/Session.tsx";
import { Settings } from "./components/Settings.tsx";
import { createGateway, remember, remembered, type Gateway } from "./gateway.ts";
import { applyTheme, themeByName } from "./theme.ts";

const gateway: Gateway = createGateway();

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
    if (url && secret) void gateway.connect(url, secret).catch(() => undefined);
  });

  return (
    <div class="layout">
      <aside class="sidebar">
        <ConnectForm />
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
      <main class="main">{props.children}</main>
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
        disabled={gateway.connection() !== "connected"}
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

function ConnectForm(): JSX.Element {
  const [url, setUrl] = createSignal(remembered("url", "ws://127.0.0.1:2420/ws"));
  const [secret, setSecret] = createSignal(remembered("secret"));

  return (
    <form
      class="connect"
      onSubmit={(event) => {
        event.preventDefault();
        remember("url", url());
        remember("secret", secret());
        void gateway.connect(url(), secret()).catch(() => undefined);
      }}
    >
      <input
        class="connect-url"
        type="text"
        placeholder="ws://127.0.0.1:2420/ws"
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
