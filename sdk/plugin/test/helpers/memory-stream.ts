// In-memory ByteReader/ByteWriter test doubles that drive a `JsonRpcEndpoint`
// without spawning a process or touching real stdio. A `MemoryByteReader`
// simulates the peer's incoming bytes (host→plugin requests, and responses
// to plugin-initiated requests — both travel over the same stream in real
// stdio); a `MemoryByteWriter` records everything the endpoint writes back.

import type { ByteReader, ByteWriter } from "../../src/stdio.ts";

export class MemoryByteReader implements ByteReader {
  private readonly queue: Uint8Array[] = [];
  private readonly waiters: Array<(chunk: Uint8Array | null) => void> = [];
  private closed = false;

  /** Enqueues raw bytes to be read. */
  push(chunk: Uint8Array): void {
    if (this.closed) throw new Error("MemoryByteReader: push() after close()");
    const waiter = this.waiters.shift();
    if (waiter) {
      waiter(chunk);
    } else {
      this.queue.push(chunk);
    }
  }

  /** Enqueues `value` as a single compact-JSON line (JSON-RPC message). */
  pushLine(value: unknown): void {
    this.push(new TextEncoder().encode(`${JSON.stringify(value)}\n`));
  }

  /** Enqueues raw text verbatim (no trailing newline added). */
  pushRaw(text: string): void {
    this.push(new TextEncoder().encode(text));
  }

  /** Simulates EOF: any pending/future `read()` resolves to `null`. */
  close(): void {
    this.closed = true;
    for (const waiter of this.waiters.splice(0)) waiter(null);
  }

  read(): Promise<Uint8Array | null> {
    const chunk = this.queue.shift();
    if (chunk) return Promise.resolve(chunk);
    if (this.closed) return Promise.resolve(null);
    return new Promise((resolve) => this.waiters.push(resolve));
  }
}

export class MemoryByteWriter implements ByteWriter {
  /** Every line written so far, already `JSON.parse`d. */
  readonly messages: unknown[] = [];

  private buffer = "";
  private readonly decoder = new TextDecoder();
  private pendingWaits: Array<{ count: number; resolve: () => void }> = [];

  async write(chunk: Uint8Array): Promise<void> {
    this.buffer += this.decoder.decode(chunk, { stream: true });
    let idx = this.buffer.indexOf("\n");
    while (idx !== -1) {
      const line = this.buffer.slice(0, idx);
      this.buffer = this.buffer.slice(idx + 1);
      if (line.length > 0) this.messages.push(JSON.parse(line));
      idx = this.buffer.indexOf("\n");
    }
    this.pendingWaits = this.pendingWaits.filter(({ count, resolve }) => {
      if (this.messages.length >= count) {
        resolve();
        return false;
      }
      return true;
    });
  }

  /** Resolves once at least `count` messages have been written. */
  waitForCount(count: number, timeoutMs = 2_000): Promise<void> {
    if (this.messages.length >= count) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        reject(
          new Error(
            `waitForCount(${count}) timed out; have ${this.messages.length}`,
          ),
        );
      }, timeoutMs);
      this.pendingWaits.push({
        count,
        resolve: () => {
          clearTimeout(timer);
          resolve();
        },
      });
    });
  }
}
