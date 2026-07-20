// Newline-delimited JSON-RPC 2.0 endpoint over stdio.
//
// Wire shape mirrors the Rust-side transport (see `xai-acp-lib`: compact
// (single-line) JSON, `\n`-terminated, `{"jsonrpc":"2.0", ...}`). This module
// is the ONLY place that touches a runtime-specific stdin/stdout API — every
// other module talks to `JsonRpcEndpoint`, which works identically on Bun,
// Node 22+, and Deno 2.
//
// Runtime differences, handled here:
// - Bun & Node: `process.stdin`/`process.stdout` (from `node:process`) are
//   async-iterable / callback-based Node streams. Reading via the stdin
//   async iterator and writing via `stdout.write(chunk, cb)` works
//   identically on both.
// - Deno: Node-compat `process.stdin` async iteration is comparatively new
//   and has had EOF/backpressure gaps across Deno versions, so we don't rely
//   on it. When `globalThis.Deno` is present we instead drive the Web-standard
//   `Deno.stdin.readable` / `Deno.stdout.writable` streams directly — those
//   are Deno's native, well-exercised path.
//
// Everything downstream of the byte layer is plain Web-standard
// (TextEncoder/TextDecoder, Uint8Array) so the framing and RPC logic itself
// has zero runtime-specific code.

import process from "node:process";

/** One read of raw bytes from the transport. `null` signals EOF. */
export interface ByteReader {
  read(): Promise<Uint8Array | null>;
}

/** One write of raw bytes to the transport. */
export interface ByteWriter {
  write(chunk: Uint8Array): Promise<void>;
}

/** Shape of the bits of the `Deno` global this module depends on. */
interface DenoLike {
  stdin?: { readable: ReadableStream<Uint8Array> };
  stdout?: { writable: WritableStream<Uint8Array> };
}

function getDeno(): DenoLike | undefined {
  return (globalThis as { Deno?: DenoLike }).Deno;
}

/**
 * Default stdin reader for the current runtime. Deno: native
 * `Deno.stdin.readable`. Bun/Node: `process.stdin`'s async iterator (yields
 * `Buffer`, a `Uint8Array` subclass).
 */
export function defaultByteReader(): ByteReader {
  const deno = getDeno();
  if (deno?.stdin?.readable) {
    const reader = deno.stdin.readable.getReader();
    return {
      async read() {
        const { value, done } = await reader.read();
        if (done) return null;
        return value ?? null;
      },
    };
  }

  const stdin = process.stdin as unknown as AsyncIterable<Uint8Array>;
  const iterator = stdin[Symbol.asyncIterator]();
  return {
    async read() {
      const { value, done } = await iterator.next();
      if (done || value === undefined) return null;
      return value;
    },
  };
}

/**
 * Default stdout writer for the current runtime. Deno: native
 * `Deno.stdout.writable`. Bun/Node: `process.stdout.write`.
 */
export function defaultByteWriter(): ByteWriter {
  const deno = getDeno();
  if (deno?.stdout?.writable) {
    const writer = deno.stdout.writable.getWriter();
    return {
      write: (chunk) => writer.write(chunk),
    };
  }

  return {
    write: (chunk) =>
      new Promise<void>((resolve, reject) => {
        process.stdout.write(chunk, (err) => {
          if (err) reject(err);
          else resolve();
        });
      }),
  };
}

/** Hard cap on a single line's byte length, per the wire contract. */
export const MAX_LINE_BYTES = 64 * 1024 * 1024;

/**
 * Buffers bytes from a `ByteReader` and yields decoded, `\n`-delimited lines
 * (terminator stripped). Throws if a line would exceed `maxLineBytes` before
 * a terminator is seen.
 */
export class LineReader {
  private readonly reader: ByteReader;
  private readonly maxLineBytes: number;
  private buf = new Uint8Array(0);
  private readonly decoder = new TextDecoder();
  private eof = false;

  constructor(reader: ByteReader, maxLineBytes: number = MAX_LINE_BYTES) {
    this.reader = reader;
    this.maxLineBytes = maxLineBytes;
  }

  private append(chunk: Uint8Array): void {
    const merged = new Uint8Array(this.buf.length + chunk.length);
    merged.set(this.buf, 0);
    merged.set(chunk, this.buf.length);
    this.buf = merged;
  }

  /** Returns the next line, or `null` at end of stream (no more data ever). */
  async nextLine(): Promise<string | null> {
    for (;;) {
      const idx = this.buf.indexOf(0x0a);
      if (idx !== -1) {
        const lineBytes = this.buf.subarray(0, idx);
        this.buf = this.buf.subarray(idx + 1);
        if (lineBytes.length > this.maxLineBytes) {
          throw new Error(
            `stdio line exceeds max length of ${this.maxLineBytes} bytes`,
          );
        }
        return this.decoder.decode(lineBytes);
      }

      if (this.buf.length > this.maxLineBytes) {
        throw new Error(
          `stdio line exceeds max length of ${this.maxLineBytes} bytes`,
        );
      }

      if (this.eof) {
        if (this.buf.length === 0) return null;
        const rest = this.decoder.decode(this.buf);
        this.buf = new Uint8Array(0);
        return rest;
      }

      const chunk = await this.reader.read();
      if (chunk === null) {
        this.eof = true;
        continue;
      }
      if (chunk.length > 0) this.append(chunk);
    }
  }
}

/** Encodes a value as compact JSON followed by `\n`. */
export function encodeLine(value: unknown): Uint8Array {
  return new TextEncoder().encode(`${JSON.stringify(value)}\n`);
}

/** Writes JSON values as `\n`-delimited compact JSON lines. */
export class LineWriter {
  private readonly writer: ByteWriter;

  constructor(writer: ByteWriter) {
    this.writer = writer;
  }

  async writeValue(value: unknown): Promise<void> {
    await this.writer.write(encodeLine(value));
  }
}

export type JsonRpcId = number | string;

export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: JsonRpcId;
  method: string;
  params?: unknown;
}

export interface JsonRpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

export interface JsonRpcErrorObject {
  code: number;
  message: string;
  data?: unknown;
}

export interface JsonRpcSuccess {
  jsonrpc: "2.0";
  id: JsonRpcId;
  result: unknown;
}

export interface JsonRpcErrorResponse {
  jsonrpc: "2.0";
  id: JsonRpcId | null;
  error: JsonRpcErrorObject;
}

/** Standard JSON-RPC 2.0 error codes used by this endpoint. */
export const JsonRpcErrorCode = {
  ParseError: -32700,
  InvalidRequest: -32600,
  MethodNotFound: -32601,
  InvalidParams: -32602,
  InternalError: -32603,
} as const;

/** Thrown when an outgoing request comes back with a JSON-RPC error object. */
export class JsonRpcRemoteError extends Error {
  readonly code: number;
  readonly data?: unknown;

  constructor(error: JsonRpcErrorObject) {
    super(error.message);
    this.name = "JsonRpcRemoteError";
    this.code = error.code;
    this.data = error.data;
  }
}

/** Thrown when an outgoing request doesn't get a reply within its timeout. */
export class RpcTimeoutError extends Error {
  constructor(method: string, timeoutMs: number) {
    super(`RPC call "${method}" timed out after ${timeoutMs}ms`);
    this.name = "RpcTimeoutError";
  }
}

export type RequestHandler = (
  params: unknown,
) => Promise<unknown> | unknown;
export type NotificationHandler = (
  params: unknown,
) => Promise<void> | void;

export interface JsonRpcEndpointOptions {
  reader?: ByteReader;
  writer?: ByteWriter;
  maxLineBytes?: number;
  /** Default timeout for outgoing `request()` calls without an explicit override. */
  defaultTimeoutMs?: number;
  /** Called for lines that can't be parsed/routed as JSON-RPC. Defaults to a no-op. */
  onProtocolError?: (err: unknown, line: string) => void;
}

const DEFAULT_TIMEOUT_MS = 30_000;

interface PendingCall {
  resolve: (value: unknown) => void;
  reject: (err: unknown) => void;
  timer: ReturnType<typeof setTimeout>;
}

/**
 * Bidirectional JSON-RPC 2.0 endpoint over a single newline-delimited
 * transport: serves incoming requests/notifications (host→plugin) and issues
 * outgoing requests/notifications (plugin→host) on the very same stream, per
 * the wire contract (`id` spaces are independent per direction, so incoming
 * ids are simply echoed back and outgoing ids come from a local counter).
 */
export class JsonRpcEndpoint {
  private readonly lineReader: LineReader;
  private readonly lineWriter: LineWriter;
  private readonly requestHandlers = new Map<string, RequestHandler>();
  private readonly notificationHandlers = new Map<string, NotificationHandler>();
  private readonly pending = new Map<JsonRpcId, PendingCall>();
  private readonly defaultTimeoutMs: number;
  private readonly onProtocolError: (err: unknown, line: string) => void;

  private nextId = 1;
  private started = false;
  private loopPromise: Promise<void> = Promise.resolve();
  private closedResolve!: () => void;
  private stopSignal: Promise<void> = new Promise(() => {});
  private resolveStopSignal: () => void = () => {};
  /** Serializes all outgoing writes so concurrent handlers can't interleave lines. */
  private writeChain: Promise<void> = Promise.resolve();
  readonly whenClosed: Promise<void>;

  constructor(options: JsonRpcEndpointOptions = {}) {
    this.lineReader = new LineReader(
      options.reader ?? defaultByteReader(),
      options.maxLineBytes,
    );
    this.lineWriter = new LineWriter(options.writer ?? defaultByteWriter());
    this.defaultTimeoutMs = options.defaultTimeoutMs ?? DEFAULT_TIMEOUT_MS;
    this.onProtocolError = options.onProtocolError ?? (() => {});
    this.whenClosed = new Promise((resolve) => {
      this.closedResolve = resolve;
    });
  }

  /** Registers a handler for an incoming request method (host→plugin). */
  setRequestHandler(method: string, handler: RequestHandler): void {
    this.requestHandlers.set(method, handler);
  }

  /** Registers a handler for an incoming notification method (host→plugin). */
  setNotificationHandler(method: string, handler: NotificationHandler): void {
    this.notificationHandlers.set(method, handler);
  }

  /** Starts the read loop. Idempotent. */
  start(): void {
    if (this.started) return;
    this.started = true;
    this.stopSignal = new Promise((resolve) => {
      this.resolveStopSignal = resolve;
    });
    this.loopPromise = this.runLoop().finally(() => {
      this.rejectAllPending(new Error("JSON-RPC endpoint closed"));
      this.closedResolve();
    });
  }

  /**
   * Requests the read loop to stop and waits for it to fully exit. Safe to
   * call even when the underlying transport never reaches EOF (e.g. an
   * in-memory test double) — `start()` races the next `nextLine()` against
   * this stop signal so the loop always unwinds promptly.
   */
  async stop(): Promise<void> {
    if (this.started) {
      this.started = false;
      this.resolveStopSignal();
    }
    await this.loopPromise;
  }

  private async runLoop(): Promise<void> {
    for (;;) {
      type StepResult = { kind: "line"; line: string | null } | { kind: "stop" };
      let step: StepResult;
      try {
        step = await Promise.race<StepResult>([
          this.lineReader.nextLine().then((line) => ({ kind: "line", line })),
          this.stopSignal.then((): StepResult => ({ kind: "stop" })),
        ]);
      } catch (err) {
        this.onProtocolError(err, "");
        return;
      }
      if (step.kind === "stop") return;
      if (step.line === null) return;
      if (step.line.length === 0) continue;
      // Deliberately NOT awaited: a handler (a hook, `setup()`, ...) may
      // itself issue an outgoing request (e.g. `ctx.storage.get`) whose
      // response can only be delivered by this very loop. Awaiting dispatch
      // here would park the loop mid-handler and deadlock that call forever.
      // `nextLine()` above is still awaited each iteration, so the single
      // underlying reader is never consumed concurrently — only downstream
      // handler execution runs concurrently with reading the next line.
      const line = step.line;
      void this.handleLine(line).catch((err) => this.onProtocolError(err, line));
    }
  }

  private async handleLine(line: string): Promise<void> {
    let message: unknown;
    try {
      message = JSON.parse(line);
    } catch (err) {
      this.onProtocolError(err, line);
      return;
    }

    if (typeof message !== "object" || message === null || Array.isArray(message)) {
      this.onProtocolError(new Error("expected a JSON object"), line);
      return;
    }

    const msg = message as Record<string, unknown>;

    if (typeof msg.method === "string") {
      if ("id" in msg && msg.id !== undefined && msg.id !== null) {
        await this.handleIncomingRequest(msg.id as JsonRpcId, msg.method, msg.params);
      } else {
        await this.handleIncomingNotification(msg.method, msg.params);
      }
      return;
    }

    if ("id" in msg && msg.id !== undefined && msg.id !== null) {
      this.handleIncomingResponse(msg.id as JsonRpcId, msg);
      return;
    }

    this.onProtocolError(new Error("message has neither method nor id"), line);
  }

  private async handleIncomingRequest(
    id: JsonRpcId,
    method: string,
    params: unknown,
  ): Promise<void> {
    const handler = this.requestHandlers.get(method);
    if (!handler) {
      await this.sendError(id, {
        code: JsonRpcErrorCode.MethodNotFound,
        message: `method not found: ${method}`,
      });
      return;
    }
    try {
      const result = await handler(params);
      await this.sendResult(id, result);
    } catch (err) {
      await this.sendError(id, {
        code: JsonRpcErrorCode.InternalError,
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  private async handleIncomingNotification(
    method: string,
    params: unknown,
  ): Promise<void> {
    const handler = this.notificationHandlers.get(method);
    if (!handler) return;
    try {
      await handler(params);
    } catch (err) {
      this.onProtocolError(err, method);
    }
  }

  private handleIncomingResponse(id: JsonRpcId, msg: Record<string, unknown>): void {
    const pending = this.pending.get(id);
    if (!pending) return;
    this.pending.delete(id);
    clearTimeout(pending.timer);
    if ("error" in msg && msg.error) {
      pending.reject(new JsonRpcRemoteError(msg.error as JsonRpcErrorObject));
    } else {
      pending.resolve(msg.result);
    }
  }

  private rejectAllPending(err: unknown): void {
    for (const [id, pending] of this.pending) {
      clearTimeout(pending.timer);
      pending.reject(err);
      this.pending.delete(id);
    }
  }

  /**
   * Writes one JSON-RPC line, queued behind any writes already in flight.
   * Handler dispatch runs concurrently (see `runLoop`), so without this
   * queue two replies could otherwise interleave their bytes on the wire.
   */
  private write(value: unknown): Promise<void> {
    const next = this.writeChain.then(
      () => this.lineWriter.writeValue(value),
      () => this.lineWriter.writeValue(value),
    );
    // Swallow here so one failed write doesn't poison the chain for later
    // writes; the caller of `write()` still sees (and can react to) the error.
    this.writeChain = next.catch(() => {});
    return next;
  }

  private async sendResult(id: JsonRpcId, result: unknown): Promise<void> {
    const response: JsonRpcSuccess = { jsonrpc: "2.0", id, result: result ?? null };
    await this.write(response);
  }

  private async sendError(id: JsonRpcId, error: JsonRpcErrorObject): Promise<void> {
    const response: JsonRpcErrorResponse = { jsonrpc: "2.0", id, error };
    await this.write(response);
  }

  /** Sends an outgoing notification (plugin→host); no reply is expected. */
  async notify(method: string, params?: unknown): Promise<void> {
    const message: JsonRpcNotification = { jsonrpc: "2.0", method, params };
    await this.write(message);
  }

  /**
   * Sends an outgoing request (plugin→host) and resolves with its `result`
   * once the matching response line arrives, or rejects on a JSON-RPC error
   * reply, a transport close, or the per-call timeout.
   */
  async request<TResult = unknown>(
    method: string,
    params?: unknown,
    opts?: { timeoutMs?: number },
  ): Promise<TResult> {
    const id = this.nextId++;
    const timeoutMs = opts?.timeoutMs ?? this.defaultTimeoutMs;

    const resultPromise = new Promise<unknown>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new RpcTimeoutError(method, timeoutMs));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
    });

    const message: JsonRpcRequest = { jsonrpc: "2.0", id, method, params };
    try {
      await this.write(message);
    } catch (err) {
      const pending = this.pending.get(id);
      if (pending) {
        clearTimeout(pending.timer);
        this.pending.delete(id);
      }
      throw err;
    }

    return resultPromise as Promise<TResult>;
  }
}
