// The slash-command catalog: what a command is, where it came from, and which
// of them a query matches.
//
// Every rule here is a port of one the pager already has, and the tests read
// the Rust so a change there fails here rather than drifting quietly:
//
//   - `provenanceOf`  ← `slash/acp_command.rs`, `SkillMeta::parse`
//   - `badgeFor`      ← `slash/command.rs`, `CommandProvenance::badge`
//   - `groupOf`       ← `slash/mod.rs`, `MenuGroup::of`
//   - `matchIndices`  ← nucleo's `CaseMatching::Smart`, via `slash/matcher.rs`
//   - `menuQuery`     ← `slash/mod.rs`, `analyze_input`
//
// Pure and DOM-free, the split `transcript.ts` and `directory.ts` make.
//
// ## What the browser deliberately does not copy
//
// The pager ranks the bare `/` menu by a **most-recently-used** file it keeps
// on disk (`slash/mru.rs`). That is pager-local state with no wire form, so
// there is nothing here to read it from; commands therefore keep the order the
// shell sent, which is the order `available_commands` builds them in — builtins
// first, then skills, then a plugin's own commands, then workflows. The
// bracketed `[tag]` map is pager-local in the same way and has no counterpart.
//
// Nucleo's *scores* are likewise not on the wire. What is reproducible exactly
// is the match **set** — a smart-case subsequence — and that is what
// `matchIndices` computes; the ordering within it is this client's own and is
// spelled out at `rankCommands`.
import type { AvailableCommand } from "./wire.ts";

/**
 * Where a skill was discovered, as the shell spells it on the wire.
 *
 * `SkillScope` is `#[serde(rename_all = "lowercase")]`
 * (`xai-grok-tools/src/implementations/skills/types.rs`), and the list matters
 * for more than validation: a `scope` string that is *not* one of these is what
 * the pager calls `Foreign` — an unknown kind it passes through rather than
 * rejecting — while a missing one is `Malformed`.
 */
export const SKILL_SCOPES = ["local", "repo", "user", "server", "bundled", "plugin"] as const;

export type SkillScope = (typeof SKILL_SCOPES)[number];

/** The scope whose skills sink to their own band in the menu; `MenuGroup::of`. */
export const BUNDLED_SCOPE: SkillScope = "bundled";

/**
 * What the agent said this command is — the pager's `SkillMeta`, one for one.
 *
 * The three failure shapes are kept rather than collapsed because they are not
 * the same thing: `foreign` is a scope this build has not heard of, `malformed`
 * is skill-shaped metadata that does not parse, and `absent` is a command that
 * simply is not a skill. All three route identically (the typed line passes
 * through to the shell); only the first two are worth being able to name.
 */
export type SkillMeta =
  | { kind: "absent" }
  | { kind: "skill"; path: string; scope: SkillScope; pluginName: string | null }
  | { kind: "plugin_command"; pluginName: string | null }
  | { kind: "foreign" }
  | { kind: "malformed" };

/**
 * Origin of a command, as the menu states it — the pager's `CommandProvenance`
 * minus `Builtin`.
 *
 * There is no `builtin` here because this client has none: the pager owns
 * `/model`, `/theme`, `/exit` and forty more locally, and every one of them is
 * a terminal gesture. Everything a browser can offer came off the wire, so
 * `shell` covers what the pager's `Builtin` and `Shell` both badge.
 */
export type CommandProvenance =
  | { kind: "shell" }
  | { kind: "skill"; source: string }
  | { kind: "plugin"; source: string | null };

/**
 * The pager's badge text, reproduced exactly.
 *
 * `test/commands.test.ts` reads `CommandProvenance::badge` and fails when these
 * strings stop agreeing with it, so the two clients cannot describe the same
 * command with different words.
 */
export function badgeFor(provenance: CommandProvenance): string {
  switch (provenance.kind) {
    case "shell":
      return "built-in";
    case "skill":
      return `skill · ${provenance.source}`;
    case "plugin":
      return provenance.source === null ? "plugin" : `plugin · ${provenance.source}`;
  }
}

function trimmedField(meta: Record<string, unknown>, key: string): string | null {
  const value = meta[key];
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}

/**
 * Classify `_meta` — `SkillMeta::parse`, including the order of its tests.
 *
 * The order is load-bearing and the Rust says why: `pluginCommand` is read
 * *only* where the skill classification already yielded `absent`, because the
 * agent omits `path` and `scope` for a handler-backed command on purpose.
 * Adding either key to earn a badge would reclassify it as a markdown skill and
 * break its dispatch.
 *
 * `key in meta` rather than `meta[key] !== undefined`, because the Rust asks
 * `serde_json::Map::get` — an explicit `null` is a key that is *present*, and
 * it lands in `malformed` rather than in `absent`.
 */
export function skillMetaOf(meta: Record<string, unknown> | null | undefined): SkillMeta {
  if (!meta) return { kind: "absent" };
  const hasPath = "path" in meta;
  const hasScope = "scope" in meta;
  if (!hasPath && !hasScope) {
    if (meta["pluginCommand"] === true) {
      return { kind: "plugin_command", pluginName: trimmedField(meta, "pluginName") };
    }
    return { kind: "absent" };
  }
  const path = meta["path"];
  const rawScope = meta["scope"];
  const scope = SKILL_SCOPES.find((known) => known === rawScope);
  if (typeof path === "string" && scope) {
    return { kind: "skill", path, scope, pluginName: trimmedField(meta, "pluginName") };
  }
  if (!scope && typeof rawScope === "string") return { kind: "foreign" };
  return { kind: "malformed" };
}

/**
 * Provenance of one advertised command.
 *
 * A skill's source is its plugin's install name when it has one and its scope
 * otherwise — `SkillIdentity::source`. A plugin command's is the plugin name
 * the agent sent, and `null` when it sent none, which is a real case the wire
 * allows and the badge has its own wording for.
 */
export function provenanceOf(command: AvailableCommand): CommandProvenance {
  const meta = skillMetaOf(command._meta);
  switch (meta.kind) {
    case "skill":
      return { kind: "skill", source: meta.pluginName ?? meta.scope };
    case "plugin_command":
      return { kind: "plugin", source: meta.pluginName };
    default:
      return { kind: "shell" };
  }
}

/**
 * Which band of the bare `/` menu a row sits in — `MenuGroup::of`, declared in
 * the order they are drawn.
 *
 * Skills sink below the commands because there can be far more of them than
 * fit on screen, and a plugin's *manifest* command ranks with the commands
 * rather than with the skills its plugin may also ship: it runs code instead of
 * expanding a file.
 */
export const MENU_GROUPS = ["command", "bundled_skill", "other_skill"] as const;

export type MenuGroup = (typeof MENU_GROUPS)[number];

export function groupOf(provenance: CommandProvenance): MenuGroup {
  if (provenance.kind !== "skill") return "command";
  return provenance.source === BUNDLED_SCOPE ? "bundled_skill" : "other_skill";
}

// ---------------------------------------------------------------------------
// The seven names the pager hides, and this client's verdict on each
// ---------------------------------------------------------------------------

/**
 * Every name in the pager's `BLOCKED_ACP_NAMES`, with the browser's own
 * decision and the reason for it.
 *
 * The pager hides these **because it has its own screen for them** — a unified
 * `/hooks` and `/plugins` UI, and a `/help` of its own. That reason does not
 * transfer: a browser has no such screen, so hiding the same seven would hide
 * functionality nothing else here can reach.
 *
 * The question is therefore not "what does the pager do" but "can the browser
 * render the effect", and for these it can. Every one of them reports through
 * `send_host_turn_slash_command_output`, which is an ordinary
 * `agent_message_chunk` on `session/update`
 * (`session/acp_session.rs:1440`) — the same text this client already draws for
 * any assistant message. None of them needs a view the browser lacks.
 *
 * `test/commands.test.ts` reads `BLOCKED_ACP_NAMES` out of the pager and fails
 * when a name is added there without a verdict here, so the next one is a
 * decision someone makes rather than a default someone inherits.
 */
export const PAGER_BLOCKED: Record<string, { show: boolean; because: string }> = {
  help: {
    show: true,
    because:
      "Never advertised: `help` is not in the shell's BUILTIN_COMMANDS at all. " +
      "It is on the pager's list so a *skill* named `help` ships qualified " +
      "instead of bare, which is the same reason it is in PAGER_COMMAND_KEYS. " +
      "There is nothing here for the browser to hide.",
  },
  "hooks-list": {
    show: true,
    because:
      "Answers with the loaded hooks as plain text. The pager draws that in its " +
      "own `/hooks` view; the browser has none, so this listing is the only way " +
      "a browser can see whether a project's hooks loaded at all.",
  },
  "hooks-add": {
    show: true,
    because:
      "Takes a path, validates it is under ~/.grok/ and answers with the result " +
      "as text. Nothing about the round-trip is terminal-shaped.",
  },
  "hooks-remove": {
    show: true,
    because:
      "The inverse of hooks-add, and it distinguishes 'removed' from 'not a " +
      "user-registered path' in its own message. Hiding it would leave a browser " +
      "able to add a hook path and unable to take it back.",
  },
  "hooks-trust": {
    show: true,
    because:
      "Grants hook execution for this project and says which root it granted. " +
      "The browser already answers `x.ai/folder_trust/request`, so a trust " +
      "decision is a gesture it makes; withholding the command would be arbitrary.",
  },
  "hooks-untrust": {
    show: true,
    because:
      "Revokes that grant, and reports 'not currently trusted' when there was " +
      "none. A grant a client can give and not withdraw is worse than neither.",
  },
  "reload-plugins": {
    show: true,
    because:
      "Re-reads plugins from disk and reports what it found. The browser renders " +
      "plugin panels but has no `/plugins` screen, so without this a plugin " +
      "installed mid-session could not be picked up from a browser at all. " +
      "(`plugins` itself is not on the pager's list — it is dropped there by " +
      "colliding with a pager builtin — and the browser shows it for the same " +
      "reason it shows this one.)",
  },
};

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/**
 * Rows the dropdown will show at once before it scrolls —
 * `MAX_VISIBLE_SUGGESTIONS` in `slash/mod.rs`.
 */
export const MAX_VISIBLE_ROWS = 8;

/**
 * Smart-case subsequence match, returning the matched positions in `haystack`
 * or `null` when there is no match.
 *
 * This is nucleo's `CaseMatching::Smart` as the pager configures it: an
 * all-lowercase query matches case-insensitively, and a query carrying any
 * uppercase character is matched exactly. Leftmost-greedy, so the indices are
 * the earliest positions that spell the query — the same set nucleo highlights,
 * even where its *score* would have preferred a later, tighter run.
 */
export function matchIndices(haystack: string, query: string): number[] | null {
  if (query === "") return [];
  const sensitive = query !== query.toLowerCase();
  const hay = sensitive ? haystack : haystack.toLowerCase();
  const needle = sensitive ? query : query.toLowerCase();
  const indices: number[] = [];
  let at = 0;
  for (const character of needle) {
    const found = hay.indexOf(character, at);
    if (found < 0) return null;
    indices.push(found);
    at = found + character.length;
  }
  return indices;
}

/**
 * The trigger keys a command answers to: its advertised name, plus the bare
 * suffix of a qualified one.
 *
 * `CommandTrigger::bare_suffix_sibling` gives a qualified skill a second
 * trigger so typing `login` still offers `/acme:login` beside a builtin of the
 * same name. The pager grants that only to skills; here it is granted to a
 * plugin's manifest command too, because the shell qualifies *both* kinds by
 * exactly the same rule (`build_plugin_commands` inherits the skill catalog's
 * `taken` set), so a `/acme:deploy` that lost the bare name is unreachable by
 * its own name otherwise.
 */
export function triggersOf(name: string): string[] {
  const cut = name.lastIndexOf(":");
  const bare = cut >= 0 ? name.slice(cut + 1) : "";
  return bare === "" ? [name] : [name, bare];
}

/** One row of the menu. */
export interface CommandRow {
  command: AvailableCommand;
  /** `/name`, the way the pager builds `CommandTrigger::display`. */
  display: string;
  /** Positions in {@link display} that spelled the query. */
  indices: number[];
  provenance: CommandProvenance;
  group: MenuGroup;
  /**
   * What acceptance puts in the composer.
   *
   * Always a trailing space. Every ACP command takes free-form arguments —
   * `AcpSlashCommand::from` sets `has_args: true` unconditionally, and `input`
   * only supplies a hint — so `SuggestionRow::from_command` always pushes one.
   */
  insertText: string;
  /** The argument hint, when the agent sent one; drawn as a placeholder. */
  hint: string | null;
}

function rowOf(command: AvailableCommand, indices: number[]): CommandRow {
  const provenance = provenanceOf(command);
  return {
    command,
    display: `/${command.name}`,
    indices,
    provenance,
    group: groupOf(provenance),
    insertText: `/${command.name} `,
    hint: command.input?.hint ?? null,
  };
}

/**
 * Drop what the browser will not offer: a name with no verdict is offered, a
 * name whose verdict is `show: false` is not, and a repeated name loses.
 *
 * The dedup is `apply_acp_commands`' `claimed` set — first spelling of a
 * lowercased name wins — kept because a client that renders a catalog twice is
 * a client that inserts the wrong one half the time.
 */
export function offeredCommands(commands: readonly AvailableCommand[]): AvailableCommand[] {
  const claimed = new Set<string>();
  const offered: AvailableCommand[] = [];
  for (const command of commands) {
    const key = command.name.toLowerCase();
    if (PAGER_BLOCKED[key]?.show === false) continue;
    if (claimed.has(key)) continue;
    claimed.add(key);
    offered.push(command);
  }
  return offered;
}

/**
 * The menu for a query.
 *
 * An empty query is the whole catalog in menu order: commands, then bundled
 * skills, then everything else, with skills alphabetical inside their bands
 * because nothing on the wire ranks them and alphabetical is the only order a
 * person can predict. Commands keep the shell's order — see the note at the top
 * of this file about the MRU that ranks them in the terminal and does not exist
 * here.
 *
 * A non-empty query keeps every command one of whose triggers the query is a
 * smart-case subsequence of, and orders them: an exactly-typed name first (the
 * pager's `trigger_owns_typed_name`, which is there so nothing outranks a name
 * the user finished typing), then alphabetically by display, which is the
 * pager's own last tiebreak.
 *
 * Between those two sits the one criterion that is not the pager's, and it is
 * standing in for the one thing the wire cannot carry: nucleo's *score*. A
 * prefix match ranks above a scattered one and an earlier first match above a
 * later one, which is the shape of what nucleo rewards without being its
 * arithmetic. The pager's two remaining tiebreaks are both no-ops here — its
 * MRU file is pager-local, and its builtin-before-ACP rule sorts on a
 * distinction a browser does not have, since every row it can draw is ACP.
 *
 * A query containing `/` matches nothing, exactly as the pager refuses one:
 * `if trimmed.contains('/') { return Vec::new() }`.
 */
export function rankCommands(commands: readonly AvailableCommand[], query: string): CommandRow[] {
  const offered = offeredCommands(commands);
  const trimmed = query.trim();

  if (trimmed === "") {
    const rows = offered.map((command) => rowOf(command, []));
    return rows
      .map((row, order) => ({ row, order }))
      .sort((a, b) => {
        const band = MENU_GROUPS.indexOf(a.row.group) - MENU_GROUPS.indexOf(b.row.group);
        if (band !== 0) return band;
        if (a.row.group !== "command") {
          const name = a.row.display.toLowerCase().localeCompare(b.row.display.toLowerCase());
          if (name !== 0) return name;
        }
        return a.order - b.order;
      })
      .map(({ row }) => row);
  }

  if (trimmed.includes("/")) return [];

  const scored: { row: CommandRow; exact: boolean; prefix: boolean }[] = [];
  for (const command of offered) {
    const triggers = triggersOf(command.name);
    if (!triggers.some((trigger) => matchIndices(trigger, trimmed) !== null)) continue;
    // Highlight against the display text, the way `row.indices =
    // matcher.indices(row.display)` does: the query never contains `/`, so the
    // positions land on the name regardless of which trigger matched.
    const indices = matchIndices(`/${command.name}`, trimmed) ?? [];
    scored.push({
      row: rowOf(command, indices),
      exact: triggers.includes(trimmed),
      prefix: triggers.some((trigger) => matchIndices(trigger, trimmed)?.[0] === 0),
    });
  }

  return scored
    .sort((a, b) => {
      if (a.exact !== b.exact) return a.exact ? -1 : 1;
      if (a.prefix !== b.prefix) return a.prefix ? -1 : 1;
      const start = (a.row.indices[0] ?? 0) - (b.row.indices[0] ?? 0);
      if (start !== 0) return start;
      return a.row.display.localeCompare(b.row.display);
    })
    .map(({ row }) => row);
}

// ---------------------------------------------------------------------------
// Reading the composer
// ---------------------------------------------------------------------------

/**
 * The query the composer is asking, or `null` when the menu is closed.
 *
 * `analyze_input`, narrowed to the leading-`/` case: a line that starts with
 * `/`, with the caret still inside the command token. The query is
 * caret-clamped, so `/` typed before existing text lists everything rather than
 * filtering by whatever follows the caret.
 *
 * The pager also completes a `/token` in the *middle* of a line
 * (`mid_text_slash_context`), which this does not. That path exists there for
 * skill references inside prose; a command's own dispatch reads the leading
 * token only (`parse_invocation` on the whole line), so the discovery this menu
 * is for is complete without it. Half-implementing the other path would be
 * inventing rather than porting.
 */
export function menuQuery(text: string, caret: number): string | null {
  if (!text.startsWith("/")) return null;
  const at = Math.max(0, Math.min(caret, text.length));
  const rest = text.slice(1);
  // A bare `/`, or `/` and nothing but whitespace, is the whole-catalog case
  // and stays open wherever the caret is — `analyze_input`'s first branch.
  if (/^\s*$/.test(rest)) return "";
  const whitespace = /\s/.exec(rest);
  const commandEnd = whitespace ? whitespace.index + 1 : text.length;
  if (at > commandEnd) return null;
  const queryEnd = Math.min(Math.max(at, 1), commandEnd);
  return queryEnd <= 1 ? "" : text.slice(1, queryEnd);
}

/**
 * The argument hint to show once a command is typed and its arguments are still
 * empty — the pager's `args_placeholder`, which it draws in the composer for
 * exactly that state (`SlashSnapshot.args_placeholder`, set when
 * `args_text_empty`).
 *
 * `null` when the line is not a recognized command, when the hint is absent, or
 * when arguments have already been typed. Resolution is by advertised name,
 * case-folded the way `apply_acp_commands` folds it.
 */
export function argumentHint(
  commands: readonly AvailableCommand[],
  text: string,
): string | null {
  if (!text.startsWith("/")) return null;
  const rest = text.slice(1);
  const whitespace = /\s/.exec(rest);
  if (!whitespace) return null;
  const name = rest.slice(0, whitespace.index).toLowerCase();
  if (rest.slice(whitespace.index).trim() !== "") return null;
  const found = offeredCommands(commands).find((c) => c.name.toLowerCase() === name);
  return found?.input?.hint ?? null;
}

/**
 * Put an accepted row into the composer: the command token is replaced, and
 * whatever the line already carried after it is kept.
 *
 * Returns the new text and where the caret belongs — after the trailing space,
 * so the next keystroke is the first argument.
 */
export function acceptRow(text: string, row: CommandRow): { text: string; caret: number } {
  const rest = text.slice(1);
  const whitespace = /\s/.exec(rest);
  const commandEnd = whitespace ? whitespace.index + 1 : text.length;
  const tail = text.slice(commandEnd).replace(/^[ \t]+/, "");
  return { text: `${row.insertText}${tail}`, caret: row.insertText.length };
}
