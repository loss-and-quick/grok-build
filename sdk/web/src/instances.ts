// Which agent this page is looking at, and which others it remembers.
//
// **An instance is a machine, not an address.** That is the whole of this
// module, and it is not a stance — it is what the wire already says.
// `initialize._meta` carries `hostname`, `agentId`, `agentInstanceId` and
// `agentVersion` (`acp_agent.rs`), of which this client used to read none:
// `agentId` is a UUID the agent persists at `$GROK_HOME/agent_id`, so it
// survives a port change, a restart and the difference between `127.0.0.1` and
// the address the same machine answers on over the network. Keying on the
// address instead is a known trap rather than a hypothetical: opencode did it,
// found their built-in server and `http://localhost:4096` had forked every
// project list in two, and had to ship a migration to canonicalise it away
// (`server.tsx:44-77`). An opaque id the agent already mints costs nothing and
// cannot fork.
//
// One edge is conceded rather than fought: `GROK_AGENT_ID` can be set in the
// environment (`xai-grok-telemetry/src/id.rs`), and two machines told to report
// one id will merge here. That is the choice of whoever set the variable, and
// second-guessing it would mean trusting an address over an identity, which is
// the thing this module exists not to do.
//
// ## Why the secret is not in the list
//
// A record here is describable — a label, a host, when it was last seen — and
// the credential for a whole machine's agent is not. Holding them apart makes
// "forget every secret, keep the list" and "export the list" one line each,
// where a single blob would make both impossible to write honestly. The secret
// still lives in `localStorage`, because a `/s/<id>` bookmark opened in a
// second tab has to reach a gateway before it can attach to anything and
// `sessionStorage` does not survive that — and it is still never rendered back
// into a field.
//
// ## Why switching is one socket at a time
//
// Not simplicity. `connect` runs `settleAuth`, which in the general case sends
// `authenticate` (`gateway.ts`), so every live socket is a *login*, not a
// subscription. A page that eagerly opened three remembered instances would
// drive three sign-ins on three machines nobody asked about, and would owe each
// of them an answer to `session/request_permission` from a machine the person
// is not looking at. opencode holds every server live and can afford to because
// theirs is basic auth in a config record; that difference is the argument
// against copying them, not for it.

/** The four keys `initialize._meta` carries about the agent behind a socket. */
export interface AgentIdentity {
  /**
   * The machine's own id: a UUID persisted at `$GROK_HOME/agent_id`.
   *
   * The key for everything here. It survives a leader restart and a change of
   * address, which is exactly what an instance has to survive to be a machine
   * rather than a URL.
   */
  agentId?: string;
  /**
   * The id of *this* leader process, new on every launch (`id.rs`).
   *
   * Not an identity — a generation. A change means the leader restarted, so
   * session ids remembered from before it may no longer exist, and a client
   * that re-opened one on faith would be asking for a session that is gone.
   */
  agentInstanceId?: string;
  hostname?: string;
  agentVersion?: string;
}

/** One remembered machine. */
export interface Instance {
  /**
   * This client's own id, minted when the record is created.
   *
   * Not `agentId`: a record exists from the moment someone types an address,
   * which is before any `initialize` has said what is behind it. The two are
   * reconciled by {@link rememberConnection} on the first successful connect.
   */
  id: string;
  /** What to call it. Defaults to the machine's hostname; editable. */
  label: string;
  /** Every address known to reach it, the most recently used first. */
  addresses: string[];
  agentId?: string;
  agentInstanceId?: string;
  hostname?: string;
  agentVersion?: string;
  lastSeenUnixMs?: number;
  /** The last session attached on it, so a switch back can offer it. */
  lastSessionId?: string;
  /** How long its roster was when it was last read. */
  sessionCount?: number;
}

/**
 * What the instance line says.
 *
 * `live` and `unauthenticated` are already two different things in this client
 * — the sign-in card's condition is `phase() === "live" && auth.status !==
 * "settled"` — but the connection line has always written "connected" for both,
 * which is the one state in which the page can do nothing at all and says
 * nothing about it.
 *
 * `never` is a record nobody has reached yet: it has a label somebody typed and
 * no machine behind it.
 */
export type InstanceState =
  | "never"
  | "away"
  | "connecting"
  | "waiting"
  | "live"
  | "unauthenticated"
  | "lost";

/** Read the identity out of an `initialize` reply's `_meta`. */
export function readIdentity(meta: Record<string, unknown> | undefined): AgentIdentity {
  const text = (key: string): string | undefined => {
    const value = meta?.[key];
    return typeof value === "string" && value !== "" ? value : undefined;
  };
  return {
    agentId: text("agentId"),
    agentInstanceId: text("agentInstanceId"),
    hostname: text("hostname"),
    agentVersion: text("agentVersion"),
  };
}

/** A fresh record for an address nobody has connected to yet. */
export function newInstance(address: string, id: string = crypto.randomUUID()): Instance {
  return { id, label: hostOf(address), addresses: [address] };
}

/**
 * The route for one session, naming the instance it is on.
 *
 * Session ids are unique on a leader and not between leaders, so a bare
 * `/s/<id>` is half an address. Of the three ways to complete it — a path
 * segment, a query hint, or resolving by trying every remembered instance —
 * only this one keeps every bookmark already written working: a link with no
 * `i` reads as "whichever instance this tab is on", which is exactly what it
 * has always meant. Resolving by trying them all was never available: that
 * needs a socket per instance, and a socket here is a sign-in.
 *
 * The hint is a hint. Nothing anywhere connects because a URL said so.
 */
export function sessionHref(sessionId: string, instanceId: string | null): string {
  return instanceId
    ? `/s/${sessionId}?i=${encodeURIComponent(instanceId)}`
    : `/s/${sessionId}`;
}

/** The host and port, which is what identifies a gateway to a person. */
export function hostOf(address: string): string {
  try {
    return new URL(address).host;
  } catch {
    return address;
  }
}

export interface Connected {
  /** The record the connection was started from. */
  id: string;
  address: string;
  identity: AgentIdentity;
  /** `Date.now()` at the caller, so this function stays pure. */
  at: number;
  sessionCount?: number;
}

/**
 * Fold a successful connection into the list.
 *
 * The merge is the point. Two records whose `agentId` matches are one machine
 * reached two ways, so they collapse into a single record holding both
 * addresses — with the address just used at the front, because a retry should
 * repeat what worked. Whichever record was seen more recently keeps its label,
 * so renaming survives a merge from either side.
 *
 * Returns the id that survived: the caller has a secret filed under the id it
 * started from, and a merge that silently changed the key would lose it.
 */
export function rememberConnection(
  list: readonly Instance[],
  connected: Connected,
): { instances: Instance[]; id: string; merged: string[] } {
  const from = list.find((instance) => instance.id === connected.id);
  const base: Instance = from ?? newInstance(connected.address, connected.id);
  const sameMachine = connected.identity.agentId
    ? list.filter(
        (instance) =>
          instance.id !== base.id && instance.agentId === connected.identity.agentId,
      )
    : [];

  // The newest label wins, and an untouched record's label is its hostname, so
  // this only ever prefers a name somebody typed over one nobody did.
  const freshest = [base, ...sameMachine].reduce((newest, instance) =>
    (instance.lastSeenUnixMs ?? 0) >= (newest.lastSeenUnixMs ?? 0) ? instance : newest,
  );
  const addresses = [
    connected.address,
    ...[base, ...sameMachine].flatMap((instance) => instance.addresses),
  ].filter((address, at, all) => all.indexOf(address) === at);

  // A label nobody has edited is still the host it was minted from, so the
  // machine's own name is an improvement on it; one that has been edited is a
  // person's choice and outranks anything the wire says.
  const untouched = freshest.label === hostOf(freshest.addresses[0] ?? "");
  const merged: Instance = {
    id: base.id,
    label: untouched ? (connected.identity.hostname ?? freshest.label) : freshest.label,
    addresses,
    agentId: connected.identity.agentId ?? base.agentId,
    agentInstanceId: connected.identity.agentInstanceId,
    hostname: connected.identity.hostname ?? base.hostname,
    agentVersion: connected.identity.agentVersion ?? base.agentVersion,
    lastSeenUnixMs: connected.at,
    lastSessionId: freshest.lastSessionId,
    sessionCount: connected.sessionCount ?? base.sessionCount,
  };

  const absorbed = new Set(sameMachine.map((instance) => instance.id));
  const instances = list.filter(
    (instance) => instance.id !== base.id && !absorbed.has(instance.id),
  );
  const at = list.findIndex((instance) => instance.id === base.id);
  instances.splice(at < 0 ? instances.length : at, 0, merged);
  return { instances, id: merged.id, merged: [...absorbed] };
}

/**
 * Whether the leader behind an instance has restarted since it was last seen.
 *
 * `agentInstanceId` is new on every launch, so a mismatch means the sessions
 * remembered from the previous run may not exist any more. Not an error — a
 * reason not to trust `lastSessionId`.
 */
export function restarted(instance: Instance, identity: AgentIdentity): boolean {
  return (
    instance.agentInstanceId !== undefined &&
    identity.agentInstanceId !== undefined &&
    instance.agentInstanceId !== identity.agentInstanceId
  );
}

/**
 * The state of the instance the page is on.
 *
 * The link decides everything except the difference between `live` and
 * `unauthenticated`, which is the agent's answer rather than the socket's — and
 * the one this client already knew and did not say.
 */
export function instanceState(
  phase: "offline" | "connecting" | "live" | "waiting" | "lost",
  authSettled: boolean,
  reached: boolean,
): InstanceState {
  if (phase === "live") return authSettled ? "live" : "unauthenticated";
  if (phase === "connecting") return "connecting";
  if (phase === "waiting") return "waiting";
  if (phase === "lost") return "lost";
  // Not connected, and the two reasons differ in what a person can do about
  // them: a machine that has answered before has a label, a version and a time
  // to show, while one that never has holds only an address somebody typed.
  return reached ? "away" : "never";
}

/** The theme role the state dot takes. */
export const STATE_ROLE = {
  never: "gray_dim",
  away: "gray_bright",
  connecting: "warning",
  waiting: "warning",
  live: "accent_success",
  unauthenticated: "warning",
  lost: "accent_error",
} as const;

/**
 * "5 minutes ago", in the reader's own language.
 *
 * `Intl.RelativeTimeFormat` is in the platform, which `WEB-DEPS.md` already
 * ruled on before there was anything to date: "when it is needed —
 * `Intl.RelativeTimeFormat`, it is in the platform". This is when.
 */
export function describeSeen(then: number | undefined, now: number): string {
  if (then === undefined) return "never connected";
  const seconds = Math.round((then - now) / 1000);
  const format = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ["second", 60],
    ["minute", 60],
    ["hour", 24],
    ["day", 7],
    ["week", 5],
  ];
  let value = seconds;
  for (const [unit, span] of units) {
    if (Math.abs(value) < span) return format.format(value, unit);
    value = Math.round(value / span);
  }
  return format.format(value, "month");
}

// ---------------------------------------------------------------------------
// Storage
//
// The old shape was three flat keys — `url`, `secret`, `theme` — and the flat
// `url` is the whole reason this client could remember exactly one address. The
// new keys are scoped by instance, which is what opencode's `server-scope.ts`
// does, with their mistake corrected: the scope is an opaque id, not the URL.
// ---------------------------------------------------------------------------

const LIST_KEY = "grok.instances";
const CURRENT_KEY = "grok.instance.current";
const DEFAULT_KEY = "grok.instance.default";
const SECRET_PREFIX = "grok.secret.";
/** The single address and secret this client used to keep. */
const LEGACY_URL_KEY = "grok-gateway";
const LEGACY_SECRET_KEY = "grok-secret";

/**
 * Whether the last write was refused for want of room.
 *
 * `remember` has always swallowed storage errors, with the argument that "a
 * page with site data blocked still works; it just forgets" — true of a theme,
 * and false of this. A silently unsaved instance list is a machine the person
 * added and will not find again, so the refusal is reported instead. opencode
 * hit this ceiling for real and evicts its own keys on it (`persist.ts`).
 */
export type StorageFault = "quota" | "blocked" | null;

let fault: StorageFault = null;

export function storageFault(): StorageFault {
  return fault;
}

function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
    fault = null;
  } catch (e) {
    // By name rather than by class: what a store throws when it is full is a
    // `DOMException` in a browser and whatever the host says elsewhere, and the
    // name is the part every one of them agrees on.
    const name = e instanceof Error ? e.name : "";
    fault = name === "QuotaExceededError" ? "quota" : "blocked";
  }
}

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function drop(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    /* nothing to do about a store that will not have it removed */
  }
}

/**
 * Every remembered instance.
 *
 * Migrates the single remembered address on first read, and *moves* rather than
 * copies: leaving the old secret behind would mean "Forget secret" left a
 * working credential in storage under a key nothing looks at any more.
 */
export function loadInstances(): Instance[] {
  const stored = read(LIST_KEY);
  if (stored) {
    try {
      const parsed = JSON.parse(stored) as unknown;
      if (Array.isArray(parsed)) return parsed.filter(isInstance);
    } catch {
      // A list this build cannot read is not a list to guess at.
    }
    return [];
  }
  const url = read(LEGACY_URL_KEY);
  if (!url) return [];
  const instance = newInstance(url);
  const secret = read(LEGACY_SECRET_KEY);
  saveInstances([instance]);
  if (secret) rememberSecret(instance.id, secret);
  drop(LEGACY_URL_KEY);
  drop(LEGACY_SECRET_KEY);
  return [instance];
}

function isInstance(value: unknown): value is Instance {
  const record = value as Partial<Instance> | null;
  return (
    typeof record === "object" &&
    record !== null &&
    typeof record.id === "string" &&
    typeof record.label === "string" &&
    Array.isArray(record.addresses)
  );
}

export function saveInstances(instances: readonly Instance[]): void {
  write(LIST_KEY, JSON.stringify(instances));
}

export function currentInstanceId(): string | null {
  return read(CURRENT_KEY);
}

export function setCurrentInstanceId(id: string | null): void {
  if (id === null) drop(CURRENT_KEY);
  else write(CURRENT_KEY, id);
}

/**
 * The instance a fresh tab opens on, which is deliberately not the active one.
 *
 * Taken from opencode, where "default server" and "current server" are separate
 * (`entry.tsx:118-124`). Without it, looking at a colleague's machine once
 * quietly decides where every tab opened afterwards connects — and that
 * decision is a login.
 */
export function defaultInstanceId(): string | null {
  return read(DEFAULT_KEY);
}

export function setDefaultInstanceId(id: string | null): void {
  if (id === null) drop(DEFAULT_KEY);
  else write(DEFAULT_KEY, id);
}

export function secretFor(id: string): string {
  return read(SECRET_PREFIX + id) ?? "";
}

export function rememberSecret(id: string, secret: string): void {
  write(SECRET_PREFIX + id, secret);
}

export function forgetSecret(id: string): void {
  drop(SECRET_PREFIX + id);
}

/** Remove a record, and the credential that belongs to it. */
export function forgetInstance(instances: readonly Instance[], id: string): Instance[] {
  forgetSecret(id);
  const left = instances.filter((instance) => instance.id !== id);
  saveInstances(left);
  if (currentInstanceId() === id) setCurrentInstanceId(null);
  if (defaultInstanceId() === id) setDefaultInstanceId(null);
  return left;
}
