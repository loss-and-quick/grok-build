import { describe, expect, test } from "bun:test";

import {
  FILE_CEILING,
  FRAME_BUDGET,
  IMAGE_PIXEL_FLOOR,
  blockFor,
  frameSize,
  human,
  isRefusal,
  kindOf,
  overBudget,
  pixelsOf,
  take,
  type Attachment,
} from "../src/attach.ts";

const bytes = (text: string): Uint8Array => new TextEncoder().encode(text);

/** A real PNG header, so `pixelsOf` is reading a header and not a fixture. */
function png(width: number, height: number): Uint8Array {
  const out = new Uint8Array(40);
  out.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a], 0);
  const view = new DataView(out.buffer);
  view.setUint32(16, width);
  view.setUint32(20, height);
  return out;
}

function attached(name: string, mime: string, body: Uint8Array): Attachment {
  const result = take(name, mime, body, "id-1");
  if (isRefusal(result)) throw new Error(`refused: ${result.reason}`);
  return result;
}

describe("what a file is allowed to become", () => {
  test("audio is refused outright, because the agent fails the turn over it", () => {
    // Measured, not assumed: an `audio` block comes back `-32602 Invalid params`
    // and the whole prompt dies. A picker that accepts one is a picker that
    // costs someone a turn.
    expect(kindOf("audio/wav", "note.wav")).toBeNull();
    const result = take("note.wav", "audio/wav", bytes("RIFF"), "id");
    expect(isRefusal(result)).toBe(true);
  });

  test("a source file with no browser type is text, not an opaque blob", () => {
    // Browsers report `""` for `.rs`, `.ts` and `.toml`, which is most of what
    // anyone attaches to a coding agent.
    expect(kindOf("", "main.rs")).toBe("text");
    expect(kindOf("", "Cargo.toml")).toBe("text");
    expect(kindOf("", "photo.heic")).toBe("binary");
  });

  test("an image the agent would drop mid-turn is refused at the moment of attaching", () => {
    const tiny = take("tiny.png", "image/png", png(16, 16), "id");
    expect(isRefusal(tiny)).toBe(true);
    expect((tiny as { reason: string }).reason).toContain(String(IMAGE_PIXEL_FLOOR));
    expect(isRefusal(take("ok.png", "image/png", png(48, 48), "id"))).toBe(false);
  });

  test("a header this cannot read is taken rather than guessed at", () => {
    expect(pixelsOf(bytes("not an image header at all"))).toBeNull();
    expect(isRefusal(take("odd.png", "image/png", bytes("....some bytes...."), "id"))).toBe(false);
  });

  test("a file over the per-file ceiling is refused with the number in it", () => {
    const huge = take("big.bin", "application/octet-stream", new Uint8Array(FILE_CEILING + 1), "id");
    expect(isRefusal(huge)).toBe(true);
    expect((huge as { reason: string }).reason).toContain(human(FILE_CEILING));
  });
});

describe("what each kind becomes on the wire", () => {
  test("an image goes as an image block, which the agent forwards to the model", () => {
    const block = blockFor(attached("shot.png", "image/png", png(48, 48)));
    expect(block).toMatchObject({ type: "image", mimeType: "image/png" });
  });

  test("text goes as an embedded resource, not folded into the prompt", () => {
    const block = blockFor(attached("notes.txt", "text/plain", bytes("hello\n")));
    expect(block).toMatchObject({
      type: "resource",
      resource: { uri: "attachment://notes.txt", mimeType: "text/plain", text: "hello\n" },
    });
  });

  test("the uri never claims to be a path on the agent's machine", () => {
    // The file is on *this* machine. A `file://` uri would name a path the agent
    // could go and read something else at.
    const block = blockFor(attached("main.rs", "", bytes("fn main() {}"))) as {
      resource: { uri: string };
    };
    expect(block.resource.uri.startsWith("attachment://")).toBe(true);
  });

  test("a binary goes as a resource with blob contents", () => {
    const block = blockFor(
      attached("thing.bin", "application/octet-stream", new Uint8Array([0, 1, 2, 250])),
    ) as { resource: { blob?: string; text?: string } };
    expect(typeof block.resource.blob).toBe("string");
    expect(block.resource.text).toBeUndefined();
  });
});

describe("the frame is what has the limit, so the frame is what is measured", () => {
  test("one file under the per-file ceiling still fits", () => {
    const one = attached("a.bin", "application/octet-stream", new Uint8Array(4 * 1024 * 1024));
    expect(overBudget("hello", [one])).toBe(false);
  });

  test("two files each under the ceiling together are not, which a per-file rule misses", () => {
    // 8 MiB each is legal on its own; base64 makes the pair about 21 MiB of
    // frame, and the gateway answers an oversized frame by closing the socket
    // rather than by refusing the request.
    const a = attached("a.bin", "application/octet-stream", new Uint8Array(FILE_CEILING));
    const b = { ...a, id: "id-2", name: "b.bin" };
    expect(a.size).toBeLessThanOrEqual(FILE_CEILING);
    expect(overBudget("hello", [a, b])).toBe(true);
    expect(frameSize("hello", [a, b])).toBeGreaterThan(FRAME_BUDGET);
  });

  test("base64 is counted, not the file size", () => {
    const one = attached("a.bin", "application/octet-stream", new Uint8Array(3 * 1024 * 1024));
    // Four characters on the wire for every three bytes of file.
    expect(frameSize("", [one])).toBeGreaterThan(4 * 1024 * 1024);
  });
});
