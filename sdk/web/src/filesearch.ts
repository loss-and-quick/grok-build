// `@`-completion over `x.ai/search/fuzzy/*`.
//
// The agent already runs this search for the terminal: one `nucleo` matcher over
// an `ignore` walk, `CaseMatching::Smart`, `Normalization::Smart`, the helix
// score floor of `7 + 14·len`, and — the part that decides this file's shape —
// **`indices` on the wire** for every match
// (`crates/codegen/xai-fuzzy-file-search/src/lib.rs`, the hand-written
// `Serialize` impl). So this client ranks nothing, scores nothing and matches
// nothing. It sends the query and draws the answer.
//
// ## Three things about the wire that are not guessable from the method names
//
// 1. **`open` returns no results.** It only builds the matcher and starts the
//    walk (`FuzzyOpenReq::execute`). The status stream is spawned by *`change`*
//    (`workspace_ops.rs`), so an empty query still has to be sent as a `change`
//    before anything arrives.
// 2. **`path` is absolutised but `indices` are not.** The matcher indexes paths
//    relative to the search root (`check_entry` strips it) and scores those, so
//    `indices` are offsets into the *relative* path — but `fuzzy_poll` rewrites
//    each `path` to `root.join(path)` before it goes out
//    (`xai-grok-workspace/src/handle.rs`). Highlighting the delivered string
//    with the delivered offsets therefore paints the wrong characters, by
//    exactly the length of the root. {@link relativeTo} is where that is undone,
//    and it is also what the pager displays: its rows are relative paths.
// 3. **Nothing cleans up a dropped client.** There is no disconnect hook; a
//    search is freed by `close`, or by going 300s idle *and* somebody else
//    calling `open`, which is the only caller of `cleanup_stale`
//    (`file_system/mod.rs`). That is why the lifetime below is the session's and
//    not the modal's, and why a socket that dies with a search open is reaped by
//    this client's own next `open`.

import { createStore } from "solid-js/store";

import { highlightRuns, type HighlightRun } from "./highlight.ts";

/** The extension methods, spelled once. */
export const FUZZY_OPEN = "x.ai/search/fuzzy/open";
export const FUZZY_CHANGE = "x.ai/search/fuzzy/change";
export const FUZZY_CLOSE = "x.ai/search/fuzzy/close";
/** The notification carrying every result batch. */
export const FUZZY_STATUS = "x.ai/search/fuzzy/status";

/**
 * Rows visible before the list scrolls: the pager's `MAX_DROPDOWN_ROWS`, and
 * the page size its PageUp/PageDown move by.
 */
export const MAX_VISIBLE_ROWS = 8;

/**
 * Matches asked for per batch.
 *
 * The pager asks its in-process matcher for a thousand (`MATCHER_TOP_K`), where
 * a row costs a pointer. Here a row costs bytes in a websocket frame on every
 * keystroke, so this is the agent's own default for the parameter
 * (`FuzzyChangeReq.limit`, "default 100") rather than a number invented here.
 * The count hint says when it bit, the way the pager's says `1k+`.
 */
export const RESULT_LIMIT = 100;

/** One match, exactly as `FuzzyMatchResult` serializes it. */
export interface FuzzyMatch {
  /** File name only — the pager does not draw it, but the wire carries it. */
  name: string;
  /** `"file"` or `"directory"`; the field is named `type` on the wire. */
  kind: "file" | "directory";
  /** Absolute path: `fuzzy_poll` joined it onto the search root. */
  path: string;
  score: number;
  /** Character offsets into the path **relative to the root**. Ascending. */
  indices: readonly number[];
}

/** One `x.ai/search/fuzzy/status` batch. */
export interface FuzzyStatus {
  searchId: string;
  matches: FuzzyMatch[];
  /** Entries in the index, not matches in this batch — `Snapshot::item_count`. */
  total: number;
  /** The walk has finished; no further batch follows for this query. */
  done: boolean;
  /** Monotonic across queries, so a late batch is recognisable as late. */
  generation: number;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : null;
}

function parseMatch(value: unknown): FuzzyMatch | null {
  const row = asRecord(value);
  if (!row) return null;
  const path = row["path"];
  if (typeof path !== "string") return null;
  const rawIndices = row["indices"];
  const indices = Array.isArray(rawIndices)
    ? rawIndices.filter((index): index is number => typeof index === "number")
    : [];
  return {
    name: typeof row["name"] === "string" ? row["name"] : path,
    kind: row["type"] === "directory" ? "directory" : "file",
    path,
    score: typeof row["score"] === "number" ? row["score"] : 0,
    indices,
  };
}

/**
 * Read a status notification, or `null` if it is not one this client can use.
 *
 * Defensive because the same notification reaches every client subscribed to the
 * session (the leader routes it by the `sessionId` in its own params), so a
 * second tab's batch — or a terminal's — arrives here too. The caller matches
 * `searchId` against its own; this only guarantees the shape.
 */
export function parseFuzzyStatus(params: unknown): FuzzyStatus | null {
  const body = asRecord(params);
  if (!body) return null;
  const searchId = body["searchId"];
  if (typeof searchId !== "string") return null;
  const rawMatches = body["matches"];
  const matches = Array.isArray(rawMatches)
    ? rawMatches.map(parseMatch).filter((match): match is FuzzyMatch => match !== null)
    : [];
  return {
    searchId,
    matches,
    total: typeof body["total"] === "number" ? body["total"] : matches.length,
    done: body["done"] === true,
    generation: typeof body["generation"] === "number" ? body["generation"] : 0,
  };
}

/**
 * The path as the pager draws it, and as `indices` are numbered.
 *
 * `normalize_display_path` strips a leading `./`; the root strip undoes
 * `fuzzy_poll`'s `root.join`. A path that is not under the root is returned
 * whole rather than mangled — that cannot happen today, but silently dropping a
 * prefix that is not there would highlight nonsense if it ever did.
 */
export function relativeTo(root: string, path: string): string {
  const shorten = (text: string): string => (text.startsWith("./") ? text.slice(2) : text);
  // No session, so no root to be relative to: the path is all there is.
  if (root === "") return shorten(path);
  // `/` trims to the empty string, which is right — every path under it starts
  // with the separator alone.
  const trimmed = root.endsWith("/") ? root.slice(0, -1) : root;
  if (path === trimmed) return "";
  if (path.startsWith(`${trimmed}/`)) return shorten(path.slice(trimmed.length + 1));
  return shorten(path);
}

/** The runs to paint for one row: the relative path, marked by the wire's own indices. */
export function matchRuns(root: string, match: FuzzyMatch): HighlightRun[] {
  return highlightRuns(relativeTo(root, match.path), match.indices);
}

/**
 * The count in the dropdown's top-right corner.
 *
 * `shown/indexed`, where the pager writes `1k+` once its own cap bites. Same
 * shape, this client's cap.
 */
export function countHint(shown: number, total: number, limit = RESULT_LIMIT): string {
  return shown >= limit ? `${limit}+/${total}` : `${shown}/${total}`;
}

/** An `@`-token the caret is inside. Offsets are into the composer's text. */
export interface AtContext {
  /** The whole token, `@` included. What a file acceptance replaces. */
  start: number;
  end: number;
  /** Text between `@` and the caret, `!` included. */
  query: string;
}

/** Does the query ask for directories only? `AtContext::is_dir_mode`. */
export function isDirMode(context: AtContext): boolean {
  return context.query.endsWith("/");
}

/** Does it ask for hidden and ignored files? `AtContext::is_hidden_mode`. */
export function isHiddenMode(context: AtContext): boolean {
  return context.query.startsWith("!");
}

/** The query the matcher gets: the `!` is a mode, not a character to match. */
export function matcherQuery(context: AtContext): string {
  return isHiddenMode(context) ? context.query.slice(1) : context.query;
}

/**
 * The path portion of the token — after the `@` and any `!`.
 *
 * A directory acceptance replaces this rather than the whole token, so the
 * hidden-mode marker survives being drilled through. `AtContext::path_range`.
 */
export function pathRange(context: AtContext): { start: number; end: number } {
  return { start: context.start + (isHiddenMode(context) ? 2 : 1), end: context.end };
}

function terminates(character: string): boolean {
  return character === "," || character === ";" || /\s/.test(character);
}

/**
 * Find the `@`-token the caret sits in. A port of `context::detect_with_drill`.
 *
 * - The **rightmost** `@` at or before the caret, so `@one @two` follows the
 *   caret rather than the first token.
 * - Not preceded by an alphanumeric or `_`, which is what keeps
 *   `user@example.com` from opening a file picker.
 * - The token runs to the first whitespace, `,` or `;`, and the caret must be
 *   inside it.
 * - `drill` is the path last accepted by drilling into a directory: whitespace
 *   *inside* it is content, so `@my dir/` stays one token. It stops applying the
 *   moment the typed text no longer starts with it.
 */
export function detectAt(text: string, caret: number, drill?: string | null): AtContext | null {
  if (caret < 0 || caret > text.length) return null;
  const at = text.lastIndexOf("@", caret - 1);
  if (at < 0) return null;

  const before = text.slice(0, at).at(-1);
  if (before !== undefined && (/[\p{L}\p{N}]/u.test(before) || before === "_")) return null;

  const contentStart = at + 1;
  const afterBang = text.startsWith("!", contentStart) ? contentStart + 1 : contentStart;
  const internalUntil =
    drill && text.startsWith(drill, afterBang) ? afterBang + drill.length : null;

  let end = text.length;
  for (let index = at + 1; index < text.length; index += 1) {
    const character = text[index]!;
    if (terminates(character) && (internalUntil === null || index >= internalUntil)) {
      end = index;
      break;
    }
  }
  if (caret > end) return null;

  return { start: at, end, query: text.slice(at + 1, caret) };
}

/** What accepting a row does to the composer. */
export interface Acceptance {
  text: string;
  caret: number;
  /** The dropdown stays up: a directory was drilled into, not committed. */
  keepOpen: boolean;
  /** The path now anchoring {@link detectAt}'s `drill`, if the dropdown stays up. */
  drill: string | null;
}

/**
 * Accept a row into the composer, the way the pager's Tab and Enter do.
 *
 * Two branches, and they are the pager's, not a simplification of it:
 *
 * - **A directory chosen in dir mode** replaces only the token's path portion
 *   with `path/` and *stays open*, so the next batch lists that directory. If
 *   the `/`-appended text is what is already there, the choice is a re-confirm:
 *   it commits, takes a trailing space when the token ends the line, and closes
 *   (`FileSearchState::try_replace`, `no_op`).
 * - **Anything else** replaces the whole token with `@path` plus a space and
 *   closes (`accept_file_search_result_inner`). The `!` does not survive that,
 *   because the pager's element text is `@{path}` — a hidden file, once picked,
 *   is just a path.
 *
 * The terminal inserts a file reference as an atomic text-area element it can
 * draw shortened. A `<textarea>` has no such thing, so this client inserts the
 * path as text: the same characters the pager's element *contains*, which is
 * also what the prompt sends either way.
 */
export function acceptInto(
  text: string,
  context: AtContext,
  match: FuzzyMatch,
  root: string,
): Acceptance {
  const path = relativeTo(root, match.path);

  if (match.kind === "directory" && isDirMode(context)) {
    const range = pathRange(context);
    const atEnd = range.end === text.length;
    const existing = text.slice(range.start, range.end);
    const commit = existing === `${path}/`;
    const inserted = commit && atEnd ? `${path}/ ` : `${path}/`;
    let caret = range.start + inserted.length;
    // A committed directory that is not at the end of the line keeps the
    // terminator already there; stepping past it resumes typing after the path.
    if (commit && !atEnd) caret += 1;
    return {
      text: text.slice(0, range.start) + inserted + text.slice(range.end),
      caret,
      keepOpen: !commit,
      drill: commit ? null : path,
    };
  }

  const inserted = `@${path} `;
  return {
    text: text.slice(0, context.start) + inserted + text.slice(context.end),
    caret: context.start + inserted.length,
    keepOpen: false,
    drill: null,
  };
}

/**
 * Accept without committing: the pager's Right arrow.
 *
 * A file behaves exactly like Tab, because there is nothing under a file to
 * descend into. A directory is written *without* the trailing `/`, which drops
 * the query out of dir mode on purpose — the list then shows what is inside,
 * files included, and typing `/` filters back to directories.
 */
export function drillInto(
  text: string,
  context: AtContext,
  match: FuzzyMatch,
  root: string,
): Acceptance {
  if (match.kind !== "directory") return acceptInto(text, context, match, root);

  const path = relativeTo(root, match.path);
  const range = pathRange(context);
  return {
    text: text.slice(0, range.start) + path + text.slice(range.end),
    caret: range.start + path.length,
    keepOpen: true,
    drill: path,
  };
}

/** What the composer asks for: a query plus the two modes the query encodes. */
export interface FileSearchAsk {
  query: string;
  dirsOnly: boolean;
  hidden: boolean;
}

export interface FileSearchDeps {
  /** Send an `x.ai/*` request. Must reject when there is no socket. */
  ext: (method: string, params: unknown) => Promise<unknown>;
  /** The attached session, whose id routes the stream and whose cwd is the root. */
  session: () => { sessionId: string; cwd: string } | null;
  say: (text: string) => void;
}

export interface FileSearch {
  /** The root every row is drawn relative to; `""` when nothing is attached. */
  root: () => string;
  matches: () => FuzzyMatch[];
  /** Entries indexed so far, for the count hint. */
  total: () => number;
  /** Whether the walk behind the current query has finished. */
  done: () => boolean;
  /** Send a query, opening the session's search if this is the first one. */
  ask: (next: FileSearchAsk) => void;
  /** Feed one `x.ai/search/fuzzy/status` notification. */
  apply: (params: unknown) => void;
  /** Hand the matcher back to the agent. Safe to call with nothing open. */
  release: () => void;
  /** The socket is gone: drop the id without pretending it can be closed. */
  forget: () => void;
}

/**
 * One fuzzy search per attached session, and who owns its lifetime.
 *
 * **Not the dropdown.** `open` builds a `nucleo` matcher and starts an `ignore`
 * walk of the whole tree, so tying it to a modal that opens on `@` and closes on
 * Escape would rebuild the index every time somebody typed an at-sign. The
 * pager's own equivalent is not modal-scoped either: `FileSearchState` holds its
 * daemon for the life of the process and the dropdown is a view over it. The
 * matching action for "the dropdown opened" is a `change` with an empty query,
 * which is exactly what re-walks (`FuzzySearchManager::change` calls
 * `restart_walk` when the query is empty).
 *
 * So the search is opened lazily on the first `@`, kept for as long as this
 * client stays attached to that session, and closed when it detaches — the one
 * moment the agent can be told, because there is no disconnect hook to tell it
 * later.
 *
 * **If the socket drops with a search open** the id is worthless: the status
 * stream is addressed to a leader client that no longer exists, and a `close`
 * has nowhere to go. So it is forgotten rather than closed, and the orphaned
 * matcher on the agent falls to the only sweep there is — 300s idle, collected
 * by the next `open` (`FuzzySearchManager::open` is the sole caller of
 * `cleanup_stale`). The next `open` is this client's, on reconnect, so the leak
 * closes itself as soon as anyone searches again.
 *
 * Hidden mode is a property of the *search*, not of the query
 * (`FuzzySearchContext.hidden` is fixed at open), so typing `!` after `@` swaps
 * the search rather than adding a parameter. That costs one re-walk at the
 * moment the `!` is typed, and it is the only way the wire offers.
 */
export function createFileSearch(deps: FileSearchDeps): FileSearch {
  let open: { searchId: string; sessionId: string; hidden: boolean } | null = null;
  let opening: Promise<void> | null = null;
  let wanted: FileSearchAsk | null = null;
  let pumping = false;
  // Batches are monotonic in `generation` across queries, so a batch older than
  // the newest one seen is a superseded driver still draining and is dropped.
  let seen = -1;

  const [results, setResults] = createStore<{
    matches: FuzzyMatch[];
    total: number;
    done: boolean;
  }>({ matches: [], total: 0, done: false });

  const forget = (): void => {
    open = null;
    opening = null;
    wanted = null;
    seen = -1;
    setResults({ matches: [], total: 0, done: false });
  };

  /** Open a search for this session, or reuse the one already open for it. */
  const ensure = async (session: { sessionId: string; cwd: string }, hidden: boolean) => {
    if (open && open.sessionId === session.sessionId && open.hidden === hidden) return;
    if (open) await shut();
    if (!opening) {
      opening = (async () => {
        // `cwd` is sent explicitly even though `sessionId` would resolve it:
        // `resolve_cwd` prefers the explicit one, and it is the same string the
        // rows are drawn relative to, so root and display cannot disagree.
        // `sessionId` is still sent, because it is what the leader routes the
        // status stream by.
        const reply = (await deps.ext(FUZZY_OPEN, {
          sessionId: session.sessionId,
          cwd: session.cwd,
          hidden,
        })) as { searchId?: unknown } | null;
        const searchId = reply && typeof reply.searchId === "string" ? reply.searchId : null;
        if (!searchId) throw new Error("open returned no search id");
        open = { searchId, sessionId: session.sessionId, hidden };
        seen = -1;
      })().finally(() => {
        opening = null;
      });
    }
    await opening;
  };

  const shut = async (): Promise<void> => {
    const closing = open;
    open = null;
    seen = -1;
    if (!closing) return;
    try {
      await deps.ext(FUZZY_CLOSE, { searchId: closing.searchId });
    } catch {
      // The agent frees it on the idle sweep either way; a failed close is not
      // worth a line in the status bar the user did not ask for.
    }
  };

  const deliver = async (next: FileSearchAsk): Promise<void> => {
    const session = deps.session();
    if (!session) return;
    await ensure(session, next.hidden);
    if (!open) return;
    // Opening took a round trip, and something newer was typed during it. Send
    // the newer one instead: this query's results would be replaced before they
    // could be drawn, and the walk it would start is work the agent then throws
    // away.
    if (wanted !== null) return;
    await deps.ext(FUZZY_CHANGE, {
      searchId: open.searchId,
      query: next.query,
      dirsOnly: next.dirsOnly,
      limit: RESULT_LIMIT,
    });
  };

  const pump = async (): Promise<void> => {
    if (pumping) return;
    pumping = true;
    try {
      while (wanted) {
        const next = wanted;
        // Cleared before the await, so a keystroke that lands during the round
        // trip is picked up by the next turn of the loop and the one it
        // superseded is never sent. The pager has no debounce because its
        // matcher is in-process; this is the same absence of one, with the
        // stale query dropped instead of queued.
        wanted = null;
        await deliver(next);
      }
    } catch (e) {
      wanted = null;
      deps.say(`file search failed: ${String(e)}`);
    } finally {
      pumping = false;
    }
  };

  return {
    root: () => deps.session()?.cwd ?? "",
    matches: () => results.matches,
    total: () => results.total,
    done: () => results.done,

    ask: (next) => {
      wanted = next;
      void pump();
    },

    apply: (params) => {
      const status = parseFuzzyStatus(params);
      // Not ours: the leader routes this by the `sessionId` in the params, so
      // every client subscribed to the session sees it — a second tab's search
      // and a terminal's arrive here too, under their own ids.
      if (!status || !open || status.searchId !== open.searchId) return;
      if (status.generation < seen) return;
      seen = status.generation;
      setResults({ matches: status.matches, total: status.total, done: status.done });
    },

    release: () => {
      wanted = null;
      void shut();
    },

    forget,
  };
}
