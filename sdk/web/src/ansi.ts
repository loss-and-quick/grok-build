// Tool output is a program's own bytes, and the wire hands them over raw.
//
// ## Two channels, and why the raw one is the better source
//
// A shell tool puts its output on the wire twice. `content` is the untouched
// PTY stream (`shell/src/session/acp_conversion.rs`,
// `tools/notification_bridge.rs`), which is why `<pre class="tool-output">`
// used to show literal `[1m[36m`. `ToolOutput::Bash.output_for_prompt` is the
// same bytes with ANSI stripped for the model, built by
// `make_output_for_prompt` as `strip_str` plus a soft wrap
// (`xai-grok-tools/src/types/output.rs`), and `raw_output` is serialized
// through to ACP unrewritten — so a cleaned copy really is on the wire.
//
// **It is the wrong copy, and one `cargo build` shows why.** Read off the wire,
// the same build on both channels:
//
//     output_for_prompt  …Building [====>] 2/4: libc      Compiling ansidemo…
//     content            …Building [====>] 2/4: libc  \r\x1b[K…Compiling ansidemo…
//
// `strip_str` removes the `CSI K` and keeps the `\r`, so the erase that made
// the progress bar one line is gone while the carriage return that needs it
// stays. No client can put that back. The raw channel still has both, so the
// raw channel is what this client reads, and {@link plainText} is what makes it
// legible.
//
// ## What that is, and what it is not
//
// **This is a stripper, not a terminal, and the difference is the point.** The
// pager runs a real `vte 0.15`-driven emulator over tool output
// (`pager-render/src/render/terminal_output.rs`), which is how it keeps colour
// and how it keeps a cargo progress bar to one line. Two escapes decide that
// second part and both are handled here — a carriage return moves back to
// column zero and later bytes overwrite, and `CSI K` erases the rest of the
// line — because without them a build's progress bar arrives as dozens of
// near-identical rows, which was the specific reason the dependency audit
// refused `anser` and `ansi-to-html`. Everything past that is a screen: cursor
// addressing, scroll regions, the alternate buffer, and the colour itself.
//
// Those are not implemented, and should not be implemented here. The pager
// already owns a terminal emulator; a second one written in JavaScript would be
// a second opinion about the same bytes, which is exactly the drift this client
// exists to argue against. The place to end it is the wire: a channel carrying
// what the emulator decided, computed once by the agent the way
// `output_for_prompt` already is — and computed without dropping half of what
// it needs. Until then, colour is lost rather than guessed.

/** Where a tab stop lands, counting from column zero. */
const TAB_WIDTH = 8;

/** Final bytes of a CSI sequence, `@` through `~`. */
const CSI_FINAL = /[@-~]/;

/**
 * Escape sequences removed, `\r`, `\b`, `\t` and `CSI K` applied.
 *
 * Line-scoped by construction: the only cursor this keeps is a column within
 * the line being built, so nothing here can reach a line it has already given
 * up. A sequence this does not act on is dropped rather than printed, because
 * a control sequence rendered as text is the defect being fixed.
 */
export function plainText(raw: string): string {
  const lines: string[] = [];
  let line: string[] = [];
  let column = 0;

  const put = (text: string): void => {
    for (const character of text) {
      while (line.length < column) line.push(" ");
      line[column] = character;
      column += 1;
    }
  };
  const endLine = (): void => {
    lines.push(line.join(""));
    line = [];
    column = 0;
  };

  let at = 0;
  while (at < raw.length) {
    const code = raw.charCodeAt(at);

    // ESC, or its single-byte C1 equivalents.
    if (code === 0x1b || (code >= 0x80 && code <= 0x9f)) {
      const introducer = code === 0x1b ? raw[at + 1] : String.fromCharCode(code - 0x40);
      const body = at + (code === 0x1b ? 2 : 1);
      if (introducer === "[") {
        // CSI: parameters, intermediates, then one final byte.
        let end = body;
        while (end < raw.length && !CSI_FINAL.test(raw[end]!)) end += 1;
        const final = raw[end];
        if (final === "K") {
          // Erase in line. `0` (the default) from the cursor rightwards, `1` to
          // the cursor, `2` the whole line — the three a progress bar uses.
          const mode = raw.slice(body, end).replace(/[^0-9]/g, "") || "0";
          if (mode === "0") line = line.slice(0, column);
          else if (mode === "1") for (let i = 0; i <= column && i < line.length; i += 1) line[i] = " ";
          else if (mode === "2") line = [];
        }
        at = end + 1;
        continue;
      }
      if (introducer === "]" || introducer === "P" || introducer === "X" || introducer === "^" || introducer === "_") {
        // A string sequence: runs to BEL or to the string terminator.
        let end = body;
        while (end < raw.length) {
          const here = raw.charCodeAt(end);
          if (here === 0x07 || here === 0x9c) break;
          if (here === 0x1b && raw[end + 1] === "\\") {
            end += 1;
            break;
          }
          end += 1;
        }
        at = end + 1;
        continue;
      }
      // Anything else: a two-byte escape, or a stray ESC at the very end.
      at = body;
      continue;
    }

    const character = raw[at]!;
    at += 1;
    if (character === "\n") {
      endLine();
    } else if (character === "\r") {
      column = 0;
    } else if (character === "\b") {
      column = Math.max(0, column - 1);
    } else if (character === "\t") {
      put(" ".repeat(TAB_WIDTH - (column % TAB_WIDTH)));
    } else if (code < 0x20 || code === 0x7f) {
      // Every other C0 byte is a control this does not act on, and printing it
      // would put a control character on the page.
      continue;
    } else {
      put(character);
    }
  }
  if (line.length > 0 || column > 0) endLine();
  return lines.join("\n");
}
