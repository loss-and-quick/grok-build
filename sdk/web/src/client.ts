// JSON-RPC 2.0 over the gateway's WebSocket.
//
// The gateway moves ACP frames verbatim in both directions
// (`crates/codegen/xai-grok-shell/src/agent/web_gateway.rs`, `relay`), so this
// is an ordinary ACP peer that happens to have a socket instead of stdio, and
// on the leader's books it is an ordinary client — replay on attach, live
// fan-out, shared permission modals. None of that is re-implemented here.
import { EXT_PREFIX } from "./wire.ts";

export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: number | string;
  method: string;
  params?: unknown;
}

export interface JsonRpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

export interface JsonRpcError {
  code: number;
  message: string;
  data?: unknown;
}

export interface JsonRpcResponse {
  jsonrpc: "2.0";
  id: number | string;
  result?: unknown;
  error?: JsonRpcError;
}

export type JsonRpcFrame = JsonRpcRequest | JsonRpcNotification | JsonRpcResponse;

/** JSON-RPC's own code for "I do not implement that". */
export const METHOD_NOT_FOUND = -32601;

/**
 * JSON-RPC's code for a handler that ran and could not produce a result.
 *
 * Distinct from {@link METHOD_NOT_FOUND} on purpose. A refusal to answer
 * `x.ai/folder_trust/request` is how this client says "the user dismissed the
 * card", and reporting that as "method not found" would tell the next reader —
 * and any log — that the browser has no card at all, which stopped being true.
 */
export const INTERNAL_ERROR = -32603;

export type NotificationHandler = (method: string, params: unknown) => void;
export type RequestHandler = (method: string, params: unknown) => Promise<unknown> | undefined;

/**
 * Build the gateway URL.
 *
 * The secret goes in the query string, not a header: the browser `WebSocket`
 * constructor cannot set `Authorization`, and the gateway accepts either
 * (`validate_auth`, constant-time on both paths).
 */
export function gatewayUrl(base: string, secret: string): string {
  const url = new URL(base);
  url.searchParams.set("server-key", secret);
  return url.toString();
}

/** Strip the ACP extension prefix so callers dispatch on the real method name. */
export function unprefix(method: string): string {
  return method.startsWith(EXT_PREFIX) ? method.slice(EXT_PREFIX.length) : method;
}

/**
 * Unwrap an extension response.
 *
 * Extension handlers are not consistent about this and a client cannot tell
 * from the method name which it will get. `x.ai/sessions/list` and
 * `x.ai/plugins/panel_action` go through `to_ext_response`, which wraps the
 * payload in `ExtMethodResult` — so the roster really does arrive as
 * `result.result.sessions`. `x.ai/settings/list` goes through
 * `to_raw_response`, which does not wrap
 * (`crates/codegen/xai-grok-shell/src/extensions/mod.rs`).
 *
 * So this sniffs, exactly as the pager's own `parse_roster_list_response` does
 * (`crates/codegen/xai-grok-pager/src/app/roster.rs`). Sniffing is not a fix —
 * a payload whose own top-level key is `result` would be unwrapped wrongly —
 * and the inconsistency is reported rather than papered over.
 */
export function unwrapExt(payload: unknown): unknown {
  if (typeof payload !== "object" || payload === null) return payload;
  const envelope = payload as { result?: unknown; error?: unknown };
  if (!("result" in envelope)) return payload;
  if (envelope.error != null) {
    throw new Error(`extension call failed: ${JSON.stringify(envelope.error)}`);
  }
  return envelope.result;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
}

/** The socket surface this client needs; narrowed so tests can supply a fake. */
export interface SocketLike {
  send(data: string): void;
  close(): void;
  addEventListener(type: "message", listener: (event: { data: unknown }) => void): void;
  addEventListener(type: "open" | "close" | "error", listener: () => void): void;
}

export class GatewayClient {
  private socket: SocketLike | null = null;
  private nextId = 1;
  private readonly pending = new Map<number | string, Pending>();
  private readonly notificationHandlers: NotificationHandler[] = [];
  private requestHandler: RequestHandler | undefined;

  constructor(private readonly openSocket: () => SocketLike) {}

  onNotification(handler: NotificationHandler): void {
    this.notificationHandlers.push(handler);
  }

  /**
   * Answer agent→client requests.
   *
   * A handler returning `undefined` declines, and the frame is answered with
   * `method not found` rather than dropped. Silence would be worse than a
   * refusal: the agent waits on the response, and an unanswered
   * `session/request_permission` parks the turn until it times out.
   */
  onRequest(handler: RequestHandler): void {
    this.requestHandler = handler;
  }

  connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const socket = this.openSocket();
      this.socket = socket;
      socket.addEventListener("message", (event) => this.receive(String(event.data)));
      socket.addEventListener("open", () => resolve());
      socket.addEventListener("error", () => reject(new Error("gateway socket error")));
      socket.addEventListener("close", () => {
        this.failAllPending(new Error("gateway socket closed"));
        this.socket = null;
      });
    });
  }

  close(): void {
    this.socket?.close();
    this.socket = null;
  }

  /** Send a standard ACP request. */
  request(method: string, params: unknown): Promise<unknown> {
    const id = this.nextId++;
    const frame: JsonRpcRequest = { jsonrpc: "2.0", id, method, params };
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.send(frame);
    });
  }

  /**
   * Send an `x.ai/*` extension request; the underscore is added here, once, and
   * the reply is unwrapped by {@link unwrapExt}.
   */
  async ext(method: string, params: unknown): Promise<unknown> {
    return unwrapExt(await this.request(`${EXT_PREFIX}${method}`, params));
  }

  notify(method: string, params: unknown): void {
    this.send({ jsonrpc: "2.0", method, params });
  }

  private send(frame: JsonRpcFrame): void {
    if (!this.socket) throw new Error("gateway is not connected");
    this.socket.send(JSON.stringify(frame));
  }

  /** Exposed for tests: feed a raw frame as if it arrived on the socket. */
  receive(text: string): void {
    let frame: unknown;
    try {
      frame = JSON.parse(text);
    } catch {
      return;
    }
    if (typeof frame !== "object" || frame === null) return;
    const message = frame as Partial<JsonRpcRequest & JsonRpcResponse>;

    if (message.method !== undefined && message.id !== undefined) {
      void this.answer(message.id, message.method, message.params);
      return;
    }
    if (message.method !== undefined) {
      const method = unprefix(message.method);
      for (const handler of this.notificationHandlers) handler(method, message.params);
      return;
    }
    if (message.id === undefined) return;
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    if (message.error) {
      pending.reject(new Error(`${message.error.message} (${message.error.code})`));
    } else {
      pending.resolve(message.result);
    }
  }

  private async answer(id: number | string, method: string, params: unknown): Promise<void> {
    const answered = this.requestHandler?.(unprefix(method), params);
    if (answered === undefined) {
      this.send({
        jsonrpc: "2.0",
        id,
        error: { code: METHOD_NOT_FOUND, message: `unsupported: ${method}` },
      });
      return;
    }
    try {
      this.send({ jsonrpc: "2.0", id, result: await answered });
    } catch (e) {
      // A handler that took the request and then declined is not a handler that
      // does not exist. The agent treats every error the same — for folder
      // trust, "not a decision", which releases its dedup key — but the code is
      // what a person reads when they ask why a card went unanswered.
      this.send({
        jsonrpc: "2.0",
        id,
        error: { code: INTERNAL_ERROR, message: String(e) },
      });
    }
  }

  private failAllPending(reason: Error): void {
    for (const pending of this.pending.values()) pending.reject(reason);
    this.pending.clear();
  }
}
