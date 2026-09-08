// Which sign-in a client may drive, and which it must refuse.
//
// Every rule here is the agent's, transcribed with its source named. The
// classification is `AuthMethodKind::from_id`
// (`crates/codegen/xai-grok-shell/src/agent/auth_method.rs:396`); the eager
// choice is `select_eager_auth_method` and the deferral to a login screen is
// `startup_auth_metadata` (`xai-grok-pager/src/acp/mod.rs`). None of it is
// re-invented, because deciding auth precedence in a client is precisely what
// the shell's own comment warns has regressed OIDC refresh before.
//
// The one judgement this file adds is `drivable`: whether *a browser* can carry
// a method through. See its comment — the answer is not "all of them".

import type { AuthMethod, AuthUrlMode } from "./wire.ts";

/** `auth_method.rs:525`, `:538`, `:549`, `OIDC_METHOD_ID`, `PLUGIN_OAUTH_METHOD_PREFIX`. */
export const XAI_API_KEY = "xai.api_key";
export const CACHED_TOKEN = "cached_token";
export const GROK_COM = "grok.com";
export const OIDC = "oidc";
export const PLUGIN_OAUTH_PREFIX = "plugin-oauth:";

/** Mirrors `AuthMethodKind` (`auth_method.rs:383`), including its `Unknown`. */
export type AuthMethodKind =
  | "xai_api_key"
  | "cached_token"
  | "grok_com"
  | "oidc"
  | "plugin_oauth"
  | "unknown";

/** `AuthMethodKind::from_id` (`auth_method.rs:396`), arm for arm. */
export function authMethodKind(id: string): AuthMethodKind {
  if (id === XAI_API_KEY) return "xai_api_key";
  if (id === CACHED_TOKEN) return "cached_token";
  if (id === GROK_COM) return "grok_com";
  if (id === OIDC) return "oidc";
  if (id.startsWith(PLUGIN_OAUTH_PREFIX)) return "plugin_oauth";
  return "unknown";
}

/** `AuthMethodKind::needs_interactive_login` (`auth_method.rs:426`). */
export function needsInteractiveLogin(kind: AuthMethodKind): boolean {
  return kind === "grok_com" || kind === "oidc" || kind === "plugin_oauth";
}

/**
 * Can *this* client carry `method` through to a credential?
 *
 * Yes for every method the agent ships, and the reason is structural rather
 * than generous: the agent does the authenticating. `grok.com`, `oidc` and a
 * plugin's sign-in all publish their URL into one single-flight attempt and
 * read a pasted code back out of it (`acp_agent.rs`, the `GROK_COM_METHOD_ID |
 * OIDC_METHOD_ID` and `plugin-oauth:` arms), so the client's part is a link, a
 * text box and a cancel button — three things a page has and a terminal has to
 * emulate. `cached_token` and `xai.api_key` take no interaction at all.
 *
 * No for anything else, and that refusal is the point. An id this build does
 * not recognise may want a device code, a paste box, a second round trip or
 * none of those, and there is no field on the wire that says which. Guessing
 * would produce a login screen that cannot finish — worse than one that says so
 * and names where the flow does work.
 */
export function drivable(method: AuthMethod): boolean {
  return authMethodKind(method.id) !== "unknown";
}

/**
 * What to say when the agent advertises a method this client cannot drive.
 *
 * Names the terminal, because the terminal is where an unrecognised first-party
 * method will already work: the pager builds its login screen from the same
 * advertised list and drives it through the same channels.
 */
export function undrivableMessage(id: string): string {
  return `This browser client does not know how to complete the “${id}” sign-in. Run \`grok login\` in a terminal on the machine running the agent, then reconnect.`;
}

/**
 * What to say when the agent advertises no method at all.
 *
 * That is fail-closed, not a bug: `[auth] preferred_method = "api_key"` with no
 * key available builds an empty list and a `None` default
 * (`auth_method.rs:173`, `build_pinned_api_key`). No client can log in from
 * here — the pager shows `PREFERRED_API_KEY_UNAVAILABLE` and stops — so this
 * says where the credential has to be put instead. An API-key box in the
 * browser would not help: see `README.md`, "Signing in".
 */
export const NO_METHODS =
  "The agent advertised no way to sign in, so no client can sign it in from here. Its `[auth] preferred_method` is pinned to a credential it cannot find: set `XAI_API_KEY`, or `api_key`/`env_key` in `~/.grok/config.toml`, on the machine running the agent, then reconnect.";

/**
 * `select_eager_auth_method` (`xai-grok-pager/src/acp/mod.rs:873`).
 *
 * The agent's `defaultAuthMethodId` wins whenever it is advertised. The legacy
 * fallback — `cached_token`, else the first entry — exists for agents too old
 * to send one.
 */
export function selectEagerMethod(
  methods: AuthMethod[],
  defaultAuthMethodId: string | null | undefined,
): string | null {
  if (defaultAuthMethodId && methods.some((m) => m.id === defaultAuthMethodId)) {
    return defaultAuthMethodId;
  }
  const cached = methods.find((m) => authMethodKind(m.id) === "cached_token");
  return (cached ?? methods[0])?.id ?? null;
}

/**
 * `startup_auth_metadata` (`xai-grok-pager/src/acp/mod.rs:657`).
 *
 * The *first* entry is the signal, and the order is the agent's contract:
 * `build_auth_methods` puts every non-interactive credential it found ahead of
 * the interactive login, so an interactive method in first place means nothing
 * else was found. Authenticating eagerly on it would open a browser nobody
 * asked for.
 */
export function startupNeedsLogin(methods: AuthMethod[]): boolean {
  const first = methods[0];
  return first !== undefined && needsInteractiveLogin(authMethodKind(first.id));
}

/** The advertised methods a person can start a login with. */
export function interactiveMethods(methods: AuthMethod[]): AuthMethod[] {
  return methods.filter((m) => needsInteractiveLogin(authMethodKind(m.id)));
}

/**
 * The mode a login with `method` starts in, before `x.ai/auth/get_url` refines
 * it. `auth_start_mode_for` (`pager/src/app/dispatch/auth.rs:69`): an external
 * provider opens its own browser and has nothing to paste; everything else
 * starts pending and is corrected by the reported mode.
 */
export function startMode(method: AuthMethod): AuthUrlMode | null {
  return method._meta?.["external_provider"] === true ? "command" : null;
}

/**
 * `extract_user_code` (`xai-grok-pager/src/views/welcome/mod.rs:1023`).
 *
 * Shown so a person can check it against the page they are approving, which is
 * the whole anti-phishing value of the device flow. Transcribed rather than
 * rewritten, down to the two rejections that look like nits and are not: a
 * parameter merely *ending* in `user_code` is not this one, and a percent-escape
 * means the value was not a bare code.
 */
/**
 * A sign-in address short enough to read, for the link's text.
 *
 * An authorize URL carries a PKCE challenge, a state nonce and the full scope
 * list, and runs to several hundred characters — printed in full it becomes the
 * loudest thing on the page and buries the code and the paste box under it. The
 * terminal has the same problem and answers it the same way: the welcome screen
 * offers a copy link and keeps the raw URL behind `auth_show_raw_url`
 * (`pager/src/views/welcome/mod.rs`).
 *
 * Nothing is hidden by this — the link's `href` is the whole URL, and the card
 * can reveal the rest — so it is a label, not a truncation. A URL that will not
 * parse is returned unchanged rather than replaced with a guess.
 */
export function shortAddress(url: string): string {
  try {
    const parsed = new URL(url);
    return parsed.origin + parsed.pathname;
  } catch {
    return url;
  }
}

export function extractUserCode(url: string | null | undefined): string | null {
  if (!url) return null;
  const query = url.split("?")[1];
  if (query === undefined) return null;
  const found = query.split("&").find((kv) => kv.startsWith("user_code="));
  if (found === undefined) return null;
  const code = found.slice("user_code=".length);
  const valid = code.length > 0 && /^[A-Za-z0-9-]+$/.test(code);
  return valid ? code : null;
}

/**
 * The agent advertised credentials it reads for itself and nothing a person can
 * start. Distinct from {@link NO_METHODS}, which is an empty list: here there
 * were methods, they just all resolve without a login screen — so when one of
 * them fails there is no button to offer, and saying so beats an empty card.
 */
export const NO_INTERACTIVE_METHOD =
  "The agent advertised only credentials it reads for itself, and none of them worked. No client can start a login here: fix the credential where the agent runs — `grok login` in a terminal on that machine, or `XAI_API_KEY` / `api_key` in `~/.grok/config.toml` — then reconnect.";

/**
 * Why no login can be started, or `null` when one can.
 *
 * Checked wherever this client concludes a person has to sign in, so the card
 * is never a title with nothing under it. The three answers are three different
 * situations and each names a different remedy: an empty list is a
 * `preferred_method` pin, an unrecognised id is a method this build cannot
 * drive, and the last is an agent whose non-interactive credentials all failed.
 */
export function noLoginAvailable(methods: AuthMethod[]): string | null {
  const candidates = interactiveMethods(methods);
  if (candidates.some(drivable)) return null;
  if (methods.length === 0) return NO_METHODS;
  const undrivable = methods.find((method) => !drivable(method));
  return undrivable ? undrivableMessage(undrivable.id) : NO_INTERACTIVE_METHOD;
}
