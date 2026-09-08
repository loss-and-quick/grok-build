// Escape sequences, and the one thing stripping them is not.
//
// The fixture is real: `cargo build` captured through a pty, escapes and all
// (`script -qec "cargo build --color=always"`). It is here because the failure
// mode this defends against only appears in a build — a hand-written sample
// with a few SGR codes in it passes any implementation, which is exactly why
// the dependency audit refused the SGR-only converters.
import { describe, expect, test } from "bun:test";

import { plainText } from "../src/ansi.ts";

const CARGO = await Bun.file(new URL("./fixtures/cargo-build.pty", import.meta.url)).text();

describe("a real build's output", () => {
  test("the fixture is the thing itself: colour, a hyperlink, and a progress bar", () => {
    // Asserted so a fixture that gets replaced by something tamer fails here
    // instead of quietly making every test below vacuous.
    expect(CARGO).toContain("\x1b[1m\x1b[92m");
    expect(CARGO).toContain("\x1b]8;;https://");
    expect(CARGO.match(/\x1b\[K/g)!.length).toBeGreaterThan(20);
    expect(CARGO.match(/\r/g)!.length).toBeGreaterThan(100);
  });

  test("nothing escaped survives to the page", () => {
    const clean = plainText(CARGO);
    expect(/[\x1b\x9b]/.test(clean)).toBe(false);
    expect(clean).not.toContain("[1m");
    expect(clean).not.toContain("8;;https://doc.rust-lang.org");
    // The link's *text* is not the link's target, and the text is what a
    // terminal shows.
    expect(clean).toContain("`dev` profile [unoptimized + debuginfo]");
  });

  test("the progress bar collapses to one line, the way a terminal collapses it", () => {
    // The defect in one number. An SGR-only strip leaves every intermediate
    // state of the bar in the buffer, separated by carriage returns — and a
    // `<pre>` breaks a line on `\r`, so the reader gets dozens of rows of
    // near-identical bar. Applying `\r` and `CSI K` is what makes it one.
    const sgrOnly = CARGO.replaceAll(/\x1b\[[0-9;]*m/g, "");
    expect(sgrOnly.match(/Building \[/g)!.length).toBeGreaterThan(50);

    const clean = plainText(CARGO);
    expect(clean).not.toContain("Building [");
    expect(clean).not.toContain("\r");
    expect(clean.split("\n").filter((line) => line.includes("Compiling syntect")).length).toBe(1);
    expect(clean.split("\n").at(-1)).toContain("Finished");
  });
});

describe("the sequences that decide what a line finally says", () => {
  test("a carriage return overwrites from column zero and leaves the rest", () => {
    // Exactly why `CSI K` exists: a shorter rewrite does not erase the tail.
    expect(plainText("aaaaa\rbb")).toBe("bbaaa");
    expect(plainText("aaaaa\rbb\x1b[K")).toBe("bb");
  });

  test("erase-in-line takes all three of its modes", () => {
    expect(plainText("abcdef\rxy\x1b[0K")).toBe("xy");
    // Mode 1 erases up to and *including* the active position, per ECMA-48.
    expect(plainText("abcdef\rxy\x1b[1K")).toBe("   def");
    expect(plainText("abcdef\rxy\x1b[2K")).toBe("");
  });

  test("cursor movement is not implemented, and the result says so", () => {
    // `CSI 3D` moves back three columns, so a terminal erases `def` here and
    // this does not. That is the boundary being drawn on purpose: past `\r` and
    // `CSI K` lies a screen, and the emulator for it already exists in Rust
    // (`pager-render/src/render/terminal_output.rs`). A second one written in
    // JavaScript would be a second opinion about the same bytes.
    expect(plainText("abcdef\x1b[3D\x1b[K")).toBe("abcdef");
  });

  test("backspace and tabs move the column rather than printing", () => {
    expect(plainText("ab\bc")).toBe("ac");
    expect(plainText("a\tb")).toBe("a       b");
  });

  test("an OSC string runs to BEL or to the string terminator, and neither leaks", () => {
    expect(plainText("\x1b]0;a window title\x07text")).toBe("text");
    expect(plainText("\x1b]8;;https://x\x1b\\link\x1b]8;;\x1b\\!")).toBe("link!");
  });

  test("a control this does not act on is dropped, never printed", () => {
    // Tool output is arbitrary program output. A control character rendered as
    // text is the defect, so the default is to drop rather than to pass on.
    expect(plainText("a\x00\x07b")).toBe("ab");
    expect(plainText("a\x1b[38;2;255;0;0mred\x1b[0m")).toBe("ared");
    expect(plainText("a\x9b31mred")).toBe("ared");
  });

  test("text with nothing in it to strip comes back unchanged", () => {
    // The stripper runs over the shell's already-stripped `output_for_prompt`
    // too, so being a no-op on clean text is a property, not a coincidence.
    const plain = "line one\nline two\n\nline four";
    expect(plainText(plain)).toBe(plain);
  });
});
