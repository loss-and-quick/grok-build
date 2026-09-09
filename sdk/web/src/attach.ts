// Files attached to a prompt from the browser, and what each one becomes on the
// wire.
//
// Nothing here is new protocol. `prompt` has always been a `ContentBlock[]`
// (`wire.ts`, `PromptRequest`), and the terminal already puts more than text in
// it: a pasted screenshot reaches the same session actor as
// `ContentBlock::Image` (`acp_session_impl/interjection.rs:89`,
// `prompt_blocks.extend(images.into_iter().map(acp::ContentBlock::Image))`).
// This is the browser reaching the same place with a file picker instead of a
// clipboard.
//
// ## Which kinds, and why those
//
// Measured against a live agent rather than taken from its advertisement,
// because the advertisement is wrong in one direction and silent in another.
// `initialize` answers `promptCapabilities: {image: false, audio: false,
// embeddedContext: true}`, and of those three:
//
// - **image — advertised false, actually works.** A 48×48 PNG sent as an
//   `image` block came back described by the model ("Red"). The flag says the
//   opposite of what the agent does, and the terminal contradicts it too, so
//   this client follows the behaviour.
// - **audio — advertised false, and genuinely refused**: the agent answers
//   `-32602 Invalid params` and the whole turn dies. So audio is refused *here*,
//   before it can cost someone a turn. This is the one kind a picker must not
//   accept.
// - **resource — advertised true, and true.** Text contents reach the model
//   verbatim (a passphrase planted in one came back), under a `file://` uri and
//   an invented `attachment://` one alike. Blob contents reach it too.
//
// ## The ceiling is measured, not chosen
//
// The whole prompt — text and every attachment — is one WebSocket text frame.
// Probing the gateway with frames of growing size: 16,777,216 bytes (16 MiB
// exactly) is answered normally, and 16,777,217 **closes the socket** with code
// 1000 and no JSON-RPC error at all. There is no failure to catch, only a
// connection that goes away, so the client has to stay under it by measuring the
// frame it is about to send rather than by trusting a per-file rule of thumb.
import type { ContentBlock } from "./wire.ts";

/**
 * The largest frame this client will hand to the socket.
 *
 * 12 MiB against a measured 16 MiB cliff. The margin is not decoration: JSON
 * escaping, the envelope and the session id all ride the same frame, and going
 * over does not fail the request, it drops the connection.
 */
export const FRAME_BUDGET = 12 * 1024 * 1024;

/**
 * The largest single file, before base64.
 *
 * Base64 is 4/3 of the bytes, so 8 MiB of file is about 10.7 MiB of frame — one
 * of these fits inside {@link FRAME_BUDGET} with room for a prompt around it,
 * and two do not, which is what the whole-frame check is for.
 */
export const FILE_CEILING = 8 * 1024 * 1024;

/**
 * The agent's own floor for an image, in total pixels.
 *
 * Not invented: an image below it is dropped mid-turn and the agent says so on
 * `image_dropped` — "images must have at least 512 total pixels". Refusing it in
 * the picker turns a silent mid-turn drop into an answer at the moment of
 * attaching, which is the only moment the person can do anything about it.
 */
export const IMAGE_PIXEL_FLOOR = 512;

/** Image types the agent's normalizer handles. */
const IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

/** What a file becomes on the wire. */
export type AttachmentKind = "image" | "text" | "binary";

export interface Attachment {
  /** Stable within a composer, so a chip can be removed without an index. */
  id: string;
  name: string;
  mimeType: string;
  kind: AttachmentKind;
  /** Bytes before base64, which is what a person recognises as the file's size. */
  size: number;
  /** Base64 for image and binary; the decoded text for text. */
  payload: string;
}

/** Why a file was not taken. Shown as written; each sentence names its source. */
export type Refusal = { name: string; reason: string };

export type Taken = { attachments: Attachment[]; refused: Refusal[] };

/**
 * Is this a file the agent can be told about at all?
 *
 * Audio is the only outright no, and it is a no because the agent says so with
 * an error that kills the turn rather than by ignoring the block.
 */
export function kindOf(mimeType: string, name: string): AttachmentKind | null {
  if (mimeType.startsWith("audio/")) return null;
  if (IMAGE_TYPES.has(mimeType)) return "image";
  if (mimeType.startsWith("text/") || TEXT_SUFFIX.test(name)) return "text";
  if (mimeType === "application/json" || mimeType === "application/xml") return "text";
  return "binary";
}

/**
 * Suffixes a browser gives no type for but a person means as text.
 *
 * A browser reports `""` for `.ts`, `.rs`, `.toml` and most source files, so
 * without this every file anyone actually wants to attach to a coding agent
 * would go as an opaque blob.
 */
const TEXT_SUFFIX =
  /\.(txt|md|markdown|rs|ts|tsx|js|jsx|json|toml|yaml|yml|css|html|sh|bash|py|go|java|c|h|cc|cpp|hpp|sql|csv|tsv|ini|cfg|conf|lock|patch|diff|log|env|gitignore)$/i;

/** `data` for an `image`, `blob` for a binary `resource`. */
function base64(bytes: Uint8Array): string {
  let binary = "";
  // Chunked: `String.fromCharCode(...bytes)` on a multi-megabyte array blows the
  // argument limit, and it does it at the size where attaching starts to matter.
  for (let at = 0; at < bytes.length; at += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(at, at + 0x8000));
  }
  return btoa(binary);
}

/**
 * How many pixels an image has, read out of its own header.
 *
 * PNG, GIF, JPEG and WebP, because those are the four the agent normalizes. A
 * header this cannot read returns `null` and the image is taken: guessing it
 * too small and refusing would be worse than letting the agent answer.
 */
export function pixelsOf(bytes: Uint8Array): number | null {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const u8 = (at: number): number => bytes[at] ?? 0;
  // PNG: IHDR width/height are big-endian at 16 and 20.
  if (bytes.length > 24 && u8(0) === 0x89 && u8(1) === 0x50 && u8(2) === 0x4e && u8(3) === 0x47) {
    return view.getUint32(16) * view.getUint32(20);
  }
  // GIF: little-endian logical screen size at 6 and 8.
  if (bytes.length > 10 && u8(0) === 0x47 && u8(1) === 0x49 && u8(2) === 0x46) {
    return view.getUint16(6, true) * view.getUint16(8, true);
  }
  // JPEG: walk the segments to the first SOFn, which carries the real size.
  if (bytes.length > 4 && u8(0) === 0xff && u8(1) === 0xd8) {
    let at = 2;
    while (at + 9 < bytes.length) {
      if (u8(at) !== 0xff) break;
      const marker = u8(at + 1);
      const length = view.getUint16(at + 2);
      // SOF0..SOF15, minus the four that are not frame headers.
      if (marker >= 0xc0 && marker <= 0xcf && ![0xc4, 0xc8, 0xcc, 0xd8].includes(marker)) {
        return view.getUint16(at + 7) * view.getUint16(at + 5);
      }
      if (length <= 0) break;
      at += 2 + length;
    }
    return null;
  }
  // WebP (VP8X and lossy VP8 only; the rest fall through to "cannot tell").
  if (bytes.length > 30 && u8(0) === 0x52 && u8(8) === 0x57 && u8(9) === 0x45) {
    if (u8(12) === 0x56 && u8(13) === 0x50 && u8(14) === 0x38 && u8(15) === 0x58) {
      const w = (u8(24) | (u8(25) << 8) | (u8(26) << 16)) + 1;
      const h = (u8(27) | (u8(28) << 8) | (u8(29) << 16)) + 1;
      return w * h;
    }
  }
  return null;
}

/** Take one file, or say why not. Bytes are already read; this is the pure half. */
export function take(
  name: string,
  mimeType: string,
  bytes: Uint8Array,
  id: string,
): Attachment | Refusal {
  const kind = kindOf(mimeType, name);
  if (kind === null) {
    return {
      name,
      reason: "The agent refuses audio in a prompt and fails the whole turn for it.",
    };
  }
  if (bytes.length > FILE_CEILING) {
    return {
      name,
      reason: `${human(bytes.length)} is over the ${human(FILE_CEILING)} a single file may be.`,
    };
  }
  if (bytes.length === 0) {
    return { name, reason: "The file is empty." };
  }
  if (kind === "image") {
    const pixels = pixelsOf(bytes);
    if (pixels !== null && pixels < IMAGE_PIXEL_FLOOR) {
      return {
        name,
        reason: `The agent drops images under ${IMAGE_PIXEL_FLOOR} pixels; this one has ${pixels}.`,
      };
    }
  }
  return {
    id,
    name,
    mimeType: mimeType || (kind === "text" ? "text/plain" : "application/octet-stream"),
    kind,
    size: bytes.length,
    payload: kind === "text" ? new TextDecoder().decode(bytes) : base64(bytes),
  };
}

/** Is this a file that was taken, or a refusal? */
export function isRefusal(result: Attachment | Refusal): result is Refusal {
  return "reason" in result;
}

/**
 * The blocks one attachment becomes.
 *
 * A text file goes as an embedded `resource` rather than folded into the prompt
 * text, because the agent renders it under a header naming the path — the same
 * shape its own file-read tool produces — instead of leaving the model to guess
 * where a pasted blob starts and ends. The uri is `attachment://<name>`, not a
 * `file://` path: the file is on *this* machine, and a `file://` uri would name
 * a path the agent could go and read something else at.
 */
export function blockFor(attachment: Attachment): ContentBlock {
  if (attachment.kind === "image") {
    return { type: "image", mimeType: attachment.mimeType, data: attachment.payload };
  }
  const uri = `attachment://${encodeURIComponent(attachment.name)}`;
  if (attachment.kind === "text") {
    return {
      type: "resource",
      resource: { uri, mimeType: attachment.mimeType, text: attachment.payload },
    };
  }
  return {
    type: "resource",
    resource: { uri, mimeType: attachment.mimeType, blob: attachment.payload },
  };
}

/**
 * Would this prompt fit in a frame?
 *
 * Measured on the assembled JSON, because that is the thing with the limit. A
 * per-file rule cannot answer it: three files each comfortably under the
 * per-file ceiling are not comfortably under the frame's.
 */
export function frameSize(text: string, attachments: readonly Attachment[]): number {
  return JSON.stringify({
    sessionId: "00000000-0000-0000-0000-000000000000",
    prompt: [{ type: "text", text }, ...attachments.map(blockFor)],
    _meta: { promptId: "00000000-0000-0000-0000-000000000000" },
  }).length;
}

export function overBudget(text: string, attachments: readonly Attachment[]): boolean {
  return frameSize(text, attachments) > FRAME_BUDGET;
}

/** Sizes as a person reads them, so a refusal names a number they recognise. */
export function human(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
