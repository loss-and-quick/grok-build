import { For, Show, createSignal, type JSX } from "solid-js";

import { blendToward, createTick, waitingBrightness } from "../animation.ts";
import { extractUserCode, interactiveMethods, shortAddress } from "../auth.ts";
import type { Gateway } from "../gateway.ts";
import { BULLET } from "../glyphs.ts";

/**
 * The three headers, in the terminal's words.
 *
 * Lifted from `xai-grok-pager/src/views/welcome/mod.rs` (`AUTH_HEADER`,
 * `DEVICE_AUTH_HEADER`, `DEVICE_CODE_CAPTION`) rather than paraphrased. The
 * same login, described two ways, is how two clients start to feel like two
 * products.
 */
const HEADER = {
  loopback: "A browser window will open for authentication.",
  device: "Approve in your browser to finish signing in.",
  command: "A browser window will open for authentication.",
} as const;

const WAITING = {
  loopback: "Waiting for the callback…",
  device: "Waiting for approval…",
  command: "Waiting for login to complete…",
} as const;

const DEVICE_CODE_CAPTION = "Make sure your browser shows this code.";

/**
 * The sign-in card.
 *
 * It exists because a browser had no way to authenticate at all: with no
 * credential on disk the agent installs no auth method, and every `session/new`
 * and `session/load` is refused with `no auth method id provided`
 * (`agent_ops.rs:4494`). Until this card, the only cure was a terminal logging
 * in to the same leader first, which made the browser a companion to a terminal
 * rather than a client.
 *
 * Nothing here authenticates. The agent runs the flow, mints the credential and
 * writes it to `~/.grok/auth.json`; this page shows a link, sometimes a code,
 * sometimes takes a pasted one back, and can cancel. No credential passes
 * through the browser at any point — see `README.md`, "Signing in".
 *
 * The paste box is drawn for `loopback` and nowhere else, because only that
 * flow reads a pasted code (`oidc/login.rs`, `race_callback_and_client_ui`). A
 * box on the device or command flow would silently swallow what was typed into
 * it, which is a worse failure than not offering one.
 */
export function AuthCard(props: { gateway: Gateway }): JSX.Element {
  const tick = createTick();
  const [pasted, setPasted] = createSignal("");
  const [showRaw, setShowRaw] = createSignal(false);

  const auth = () => props.gateway.auth;
  const mode = () => auth().mode ?? "loopback";
  const userCode = () => (auth().mode === "device" ? extractUserCode(auth().url) : null);
  const choices = () => interactiveMethods(auth().methods);

  return (
    <div class="auth">
      <div class="auth-title">
        <span
          class="auth-diamond"
          aria-hidden="true"
          style={{
            color: blendToward(
              "var(--grok-bg-base)",
              "var(--grok-accent-user)",
              waitingBrightness(tick()),
            ),
          }}
        >
          {BULLET}
        </span>
        Sign in to start a session
      </div>

      {/* Why the rest of the page is inert: not a symptom to be guessed at
          from a failed click on the roster. */}
      <Show when={auth().status !== "running"}>
        <div class="auth-why">
          The agent has no credential, so it will refuse to open or attach a session.
        </div>
      </Show>

      {/* No login can be started from here at all. Says where it can be. */}
      <Show when={auth().blocked}>
        {(blocked) => <div class="auth-blocked">{blocked()}</div>}
      </Show>

      <Show when={auth().status === "needed" && !auth().blocked}>
        <div class="auth-methods">
          {/* Every entry here is one this client can carry through: the
              filter keeps the three interactive kinds, and all three are driven
              by the same URL/code/cancel channels. A method that is neither is
              refused as a whole — `blocked` above — rather than drawn as a
              button that cannot finish. */}
          <For each={choices()}>
            {(method) => (
              <button
                class="auth-method"
                type="button"
                title={method.description ?? ""}
                onClick={() => void props.gateway.login(method.id)}
              >
                <span class="auth-method-name">Sign in with {method.name}</span>
                <Show when={method.description}>
                  {(description) => <span class="auth-method-note">{description()}</span>}
                </Show>
              </button>
            )}
          </For>
        </div>
      </Show>

      <Show when={auth().status === "running"}>
        <div class="auth-running">
          <div class="auth-header">{HEADER[mode()]}</div>

          <Show when={userCode()}>
            {(code) => (
              <div class="auth-code-block">
                <div class="auth-code">{code()}</div>
                <div class="auth-code-caption">{DEVICE_CODE_CAPTION}</div>
              </div>
            )}
          </Show>

          {/* A real link, which is the one thing this client can do that the
              terminal cannot: the pager can only offer the URL to be copied.
              Labelled with the address rather than the whole query string, for
              the reason `shortAddress` gives — and revealable, as the pager's
              own raw-URL mode is, because a browser that cannot follow the link
              needs the text. */}
          <Show
            when={auth().url}
            fallback={<div class="auth-waiting">Asking the agent for the sign-in address…</div>}
          >
            {(url) => (
              <>
                <a
                  class="auth-url"
                  href={url()}
                  target="_blank"
                  rel="noreferrer noopener"
                  title={url()}
                >
                  {shortAddress(url())}
                </a>
                <button
                  class="auth-url-toggle"
                  type="button"
                  onClick={() => setShowRaw(!showRaw())}
                >
                  {showRaw() ? "hide the full address" : "show the full address"}
                </button>
                <Show when={showRaw()}>
                  <code class="auth-url-raw">{url()}</code>
                </Show>
              </>
            )}
          </Show>

          <div class="auth-waiting">{WAITING[mode()]}</div>

          <Show when={mode() === "loopback"}>
            <form
              class="auth-paste"
              onSubmit={(event) => {
                event.preventDefault();
                void props.gateway.submitAuthCode(pasted());
                setPasted("");
              }}
            >
              {/* The callback lands on the *agent's* loopback address. When the
                  page and the agent share a machine that completes itself; when
                  they do not — a tunnel, a bound address — the browser gets a
                  dead page and its URL is the answer. The agent takes either
                  that whole address or the bare code. */}
              <label class="auth-paste-label" for="auth-paste-input">
                If the browser does not come back here, paste the address it landed on:
              </label>
              <input
                id="auth-paste-input"
                class="auth-paste-input"
                type="text"
                placeholder="http://127.0.0.1:…/callback?code=… — or just the code"
                value={pasted()}
                onInput={(event) => setPasted(event.currentTarget.value)}
              />
              <button class="auth-paste-submit" type="submit">
                Submit
              </button>
            </form>
          </Show>

          <button
            class="auth-cancel"
            type="button"
            onClick={() => props.gateway.cancelLogin()}
          >
            Cancel
          </button>
        </div>
      </Show>

      <Show when={auth().error}>
        {(error) => <div class="auth-error">{error()}</div>}
      </Show>
    </div>
  );
}
