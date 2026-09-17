// What the page knows because of where it was served from.
//
// This client began as a page from a dev server, talking to a gateway that
// happened to be somewhere else — so it had to ask for both halves of an
// address, and remember them. `grok web` serves the page and the socket from
// one process on one origin, and a page served that way already knows the
// answer to the first question and is usually handed the second.
//
// **None of this replaces the instance manager.** An instance is a machine, not
// an address (`instances.ts`), and the origin is just one more address that
// reaches one — folded in by `agentId` on `initialize` like any other, which is
// what makes `127.0.0.1:2420` and the address the same box answers on over the
// network one record rather than two. What this removes is the typing, not the
// list: a page served by a gateway on another machine still remembers that
// machine, still switches to it, and still merges it correctly.

/** What the address bar says about the gateway that served this page. */
export interface Served {
  /** This origin's own `/ws`, as a WebSocket URL. */
  endpoint: string;
  /**
   * The secret the launcher put in the URL, if it did.
   *
   * Read once and taken out of the address bar by {@link servedFrom}, so it is
   * not left in browser history, not in the `Referer` of anything the page
   * links to (the document is `no-referrer` besides), and not in a link anyone
   * copies out of the bar to send to someone else.
   */
  secret: string | null;
}

/** The query parameter the gateway accepts, in the URL it prints at startup. */
const SECRET_PARAM = "server-key";

/**
 * The gateway that served this page, or `null` when one did not.
 *
 * `null` for `file:` and anything else that is not http — there is no origin to
 * infer a socket from, so the page falls back to asking, exactly as before.
 *
 * Under `vite dev` this still answers, and correctly: the dev server proxies
 * `/ws` to the gateway (`vite.config.ts`), so development and the shipped
 * binary run the same code path rather than one path that only production
 * takes.
 */
export function servedFrom(
  location: Pick<Location, "protocol" | "host" | "search" | "pathname" | "hash"> = window.location,
  history: Pick<History, "replaceState"> | null = window.history,
): Served | null {
  if (location.protocol !== "http:" && location.protocol !== "https:") return null;
  if (!location.host) return null;

  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  const params = new URLSearchParams(location.search);
  const secret = params.get(SECRET_PARAM);

  if (secret !== null && history) {
    params.delete(SECRET_PARAM);
    const query = params.toString();
    try {
      history.replaceState(
        null,
        "",
        `${location.pathname}${query ? `?${query}` : ""}${location.hash}`,
      );
    } catch {
      // A page that cannot rewrite its own URL is still a working page; the
      // only cost is a credential left in the bar, which is where it already
      // was.
    }
  }

  return { endpoint: `${scheme}//${location.host}/ws`, secret: secret || null };
}
