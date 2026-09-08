// Walking the leader's filesystem, as arithmetic on absolute paths.
//
// The whole picker rests on one fact, established by test rather than by
// reading: `x.ai/fs/list` resolves per call, not per process. An absolute path
// is walked as given (`crates/codegen/xai-grok-shell/src/extensions/fs.rs`,
// `resolve_path`), and the process-wide workspace root never enters the walk
// because confinement is off in local mode. So a browser can already enumerate
// any directory its user can read, and none of this needs a wire change.
//
// The security boundary is deliberately not narrowed here. A client that has
// authenticated to the gateway can already read and write files anywhere on the
// box; restricting only *listing* would be theatre. The boundary is
// authentication plus the loopback bind, and `trusted_folders.toml` is not a
// substitute — it governs whether a directory's config is *executed*, not
// whether its name may be *seen*.
//
// Nothing here is DOM-aware, so the path arithmetic is testable without a
// browser — the same split `transcript.ts` makes.
import type { FsNode } from "./wire.ts";

/** The single separator this client speaks. The leader is a POSIX host. */
export const SEPARATOR = "/";

/** Root, and the only path with no parent. */
export const ROOT = SEPARATOR;

/**
 * Params for a picker's `x.ai/fs/list`.
 *
 * Three of these are the agent's defaults and are still written out, because
 * two of them would be wrong for a picker if the default ever moved and the
 * third is the one that must be overridden:
 *
 * - `depth: 1` — a picker shows one directory at a time. Depth 2 flattens
 *   grandchildren into the same list with no parent on them, so two `src`
 *   entries become indistinguishable.
 * - `includeHidden: true` — `.config`, `.local/share`, a dotted worktree root
 *   are all places a session legitimately lives.
 * - `respectGitIgnore: false` — **this one is not the default and matters
 *   most.** With the agent's `true`, listing this very repository omits
 *   `target/`, `docs/`, `bindings/` and `Cargo.toml`, because a parent
 *   `.gitignore` names them. A directory you cannot see is a directory you
 *   cannot open a session in, while `session/new` would have accepted it
 *   perfectly well — so the picker would be hiding roots the product supports.
 *   Ignore rules are about what an agent should *read*, not about where a user
 *   is allowed to *work*.
 */
export const LIST_PARAMS = {
  depth: 1,
  includeHidden: true,
  respectGitIgnore: false,
} as const;

/** Absolute in the only sense the agent accepts: `Path::is_absolute`. */
export function isAbsolute(path: string): boolean {
  return path.startsWith(SEPARATOR);
}

/**
 * Normalize a typed path: collapse repeated separators, resolve `.` and `..`,
 * and drop the trailing separator.
 *
 * The agent does none of this — `fs/list` on `/a/b/..` answers with entries
 * whose `path` is literally `/a/b/../name`, and walking up from there would
 * grow a path that never shortens. So the client resolves before it sends, and
 * every path it holds is already in its shortest form.
 *
 * A relative path is returned untouched: it is not this function's business to
 * invent a base, and the agent rejects it with `sessionId is required for
 * relative paths`, which is the honest answer to show.
 */
export function normalizePath(path: string): string {
  const trimmed = path.trim();
  if (!isAbsolute(trimmed)) return trimmed;
  const out: string[] = [];
  for (const segment of trimmed.split(SEPARATOR)) {
    if (segment === "" || segment === ".") continue;
    if (segment === "..") {
      out.pop();
      continue;
    }
    out.push(segment);
  }
  return SEPARATOR + out.join(SEPARATOR);
}

/**
 * The containing directory, or `null` at the root.
 *
 * Computed here rather than by appending `..` and asking the agent, for the
 * reason in {@link normalizePath}: the agent would answer with paths that carry
 * the `..` forward.
 */
export function parentOf(path: string): string | null {
  const normalized = normalizePath(path);
  if (!isAbsolute(normalized) || normalized === ROOT) return null;
  const cut = normalized.lastIndexOf(SEPARATOR);
  return cut <= 0 ? ROOT : normalized.slice(0, cut);
}

/** Join a directory and one child name, with no separator doubled at the root. */
export function childOf(directory: string, name: string): string {
  const base = normalizePath(directory);
  return base === ROOT ? `${ROOT}${name}` : `${base}${SEPARATOR}${name}`;
}

export interface Crumb {
  label: string;
  path: string;
}

/**
 * Every ancestor of `path`, root first, so the header doubles as the way back
 * up. The root's label is the separator itself — it has no name of its own.
 */
export function breadcrumbs(path: string): Crumb[] {
  const normalized = normalizePath(path);
  if (!isAbsolute(normalized)) return [];
  const crumbs: Crumb[] = [{ label: ROOT, path: ROOT }];
  let walked = "";
  for (const segment of normalized.split(SEPARATOR)) {
    if (!segment) continue;
    walked = `${walked}${SEPARATOR}${segment}`;
    crumbs.push({ label: segment, path: walked });
  }
  return crumbs;
}

/**
 * Split a page into what can be entered and what is only context.
 *
 * The agent already sorts directories before files and each group by name, so
 * this preserves the order it sent rather than re-sorting: the ordering is the
 * wire's, and a client that re-derives it is a client that can disagree with
 * the other one.
 *
 * Files are kept, not discarded. A `Cargo.toml` or a `package.json` in the list
 * is how a person recognizes the project they meant, and that recognition is
 * the entire job of this screen.
 */
export function partition(nodes: readonly FsNode[]): {
  directories: FsNode[];
  files: FsNode[];
} {
  const directories: FsNode[] = [];
  const files: FsNode[] = [];
  for (const node of nodes) {
    if (node.type === "directory") directories.push(node);
    else files.push(node);
  }
  return { directories, files };
}

/**
 * What an empty page means, given whether the path exists.
 *
 * The wire cannot tell these apart on its own, and this is worth stating
 * plainly rather than papering over: `fs/list` answers `{nodes: [], truncated:
 * false}` for an empty directory, for a path that is a *file*, and for a
 * directory the leader's user may not read. Only `x.ai/fs/exists` separates the
 * missing case, and nothing separates "empty" from "unreadable" — so this
 * function names both rather than picking one and being wrong half the time.
 */
export function emptyReason(exists: boolean): string {
  return exists ? "Empty, or not readable by the agent." : "No such directory.";
}
