// Every rule in `src/commands.ts` is a port of one the pager already has, so
// most of these tests read the Rust rather than restating it. A change to the
// terminal's badge wording, to the scopes it knows, to how many rows its
// dropdown shows, or to the seven names it hides fails here — which is the only
// way two clients stay one product without anyone remembering to check.
import { describe, expect, test } from "bun:test";

import {
  BUNDLED_SCOPE,
  MAX_VISIBLE_ROWS,
  PAGER_BLOCKED,
  SKILL_SCOPES,
  acceptRow,
  argumentHint,
  badgeFor,
  groupOf,
  matchIndices,
  menuQuery,
  offeredCommands,
  provenanceOf,
  rankCommands,
  skillMetaOf,
  triggersOf,
} from "../src/commands.ts";
import type { AvailableCommand } from "../src/wire.ts";

const CRATES = new URL("../../../crates/codegen/", import.meta.url).pathname;

async function read(relative: string): Promise<string> {
  return await Bun.file(`${CRATES}${relative}`).text();
}

function command(
  name: string,
  meta?: Record<string, unknown>,
  extra: Partial<AvailableCommand> = {},
): AvailableCommand {
  return { name, description: `${name} does a thing`, input: null, _meta: meta ?? null, ...extra };
}

/** A markdown skill, as the shell advertises one. */
function skill(name: string, scope: string, pluginName?: string): AvailableCommand {
  return command(name, {
    scope,
    path: `/somewhere/${name}/SKILL.md`,
    bareName: name,
    qualifiedName: `${scope}:${name}`,
    ...(pluginName ? { pluginName } : {}),
  });
}

/** A plugin's manifest command: no `path`, no `scope`, and `pluginCommand`. */
function pluginCommand(name: string, plugin: string): AvailableCommand {
  return command(name, {
    pluginCommand: true,
    pluginName: plugin,
    bareName: name,
    qualifiedName: `${plugin}:${name}`,
  });
}

describe("what the wire says a command is", () => {
  test("the scopes are the ones the shell serializes", async () => {
    // `SkillScope` is `rename_all = "lowercase"`, and a `scope` outside the set
    // is `Foreign` rather than an error — so the set has to be right.
    const source = await read("xai-grok-tools/src/implementations/skills/types.rs");
    const body = /pub enum SkillScope \{([\s\S]*?)\n\}/.exec(source)?.[1] ?? "";
    const variants = [...body.matchAll(/^\s{4}([A-Z][A-Za-z]*)\s*=/gm)].map((m) =>
      m[1]!.toLowerCase(),
    );
    expect(variants).not.toHaveLength(0);
    expect([...SKILL_SCOPES] as string[]).toEqual(variants);
  });

  test("a skill's source is its plugin when it has one, else its scope", () => {
    expect(provenanceOf(skill("commit", "user"))).toEqual({ kind: "skill", source: "user" });
    expect(provenanceOf(skill("deploy", "plugin", "acme"))).toEqual({
      kind: "skill",
      source: "acme",
    });
  });

  test("`pluginCommand` is read only where the skill keys are absent", () => {
    // The Rust says why in as many words: the agent omits `path`/`scope` for a
    // handler-backed command deliberately, and adding either to earn a badge
    // would reclassify it as a markdown skill and break its dispatch.
    expect(skillMetaOf({ pluginCommand: true, pluginName: "acme" })).toEqual({
      kind: "plugin_command",
      pluginName: "acme",
    });
    expect(
      skillMetaOf({ pluginCommand: true, pluginName: "acme", scope: "plugin", path: "/p" }).kind,
    ).toBe("skill");
  });

  test("a plugin command without a plugin name is a case, not a bug", () => {
    expect(provenanceOf(command("ship", { pluginCommand: true }))).toEqual({
      kind: "plugin",
      source: null,
    });
    expect(provenanceOf(command("ship", { pluginCommand: true, pluginName: "  " }))).toEqual({
      kind: "plugin",
      source: null,
    });
  });

  test("an unknown scope is foreign, a broken one is malformed, and both pass through", () => {
    expect(skillMetaOf({ scope: "workflow", path: "/w" }).kind).toBe("foreign");
    expect(skillMetaOf({ scope: 7, path: "/w" }).kind).toBe("malformed");
    expect(skillMetaOf({ path: null }).kind).toBe("malformed");
    expect(provenanceOf(command("x", { scope: "workflow", path: "/w" }))).toEqual({
      kind: "shell",
    });
  });

  test("a builtin, and a workflow, carry no provenance keys at all", () => {
    expect(provenanceOf(command("compact"))).toEqual({ kind: "shell" });
    expect(
      provenanceOf(command("ship-it", { workflowSource: "user", workflowPath: "/w.md" })),
    ).toEqual({ kind: "shell" });
  });
});

describe("the badge, word for word", () => {
  test("says what `CommandProvenance::badge` says", async () => {
    // Read the Rust's own arms rather than restating them: the badge is the
    // only place a browser tells a plugin's command from a builtin, so the two
    // clients must not be able to word it differently.
    const source = await read("xai-grok-pager/src/slash/command.rs");
    const arms = /pub fn badge\(&self\)[\s\S]*?\n    \}/.exec(source)?.[0] ?? "";
    expect(arms).toContain('Cow::Borrowed("built-in")');
    expect(arms).toContain('format!("skill · {source}")');
    expect(arms).toContain('Cow::Borrowed("plugin")');
    expect(arms).toContain('format!("plugin · {name}")');

    expect(badgeFor({ kind: "shell" })).toBe("built-in");
    expect(badgeFor({ kind: "skill", source: "acme" })).toBe("skill · acme");
    expect(badgeFor({ kind: "plugin", source: null })).toBe("plugin");
    expect(badgeFor({ kind: "plugin", source: "acme" })).toBe("plugin · acme");
  });

  test("a plugin's command never reads as built-in", () => {
    // The whole point of `4df5f45e`. A browser that dropped this would show a
    // plugin's `/ship` exactly as it shows the shell's `/compact`.
    expect(badgeFor(provenanceOf(pluginCommand("ship", "deployer")))).toBe("plugin · deployer");
    expect(badgeFor(provenanceOf(command("compact")))).toBe("built-in");
  });
});

describe("the seven names the pager hides", () => {
  test("each one has a verdict here", async () => {
    // The pager hides these because it has its own `/hooks`, `/plugins` and
    // `/help` screens. A browser has none, so the decision is per command and
    // this is what forces the next one to be made rather than inherited.
    const source = await read("xai-grok-pager/src/slash/registry.rs");
    const block = /BLOCKED_ACP_NAMES: &\[&str\] = &\[([\s\S]*?)\];/.exec(source)?.[1] ?? "";
    const names = [...block.matchAll(/"([^"]+)"/g)].map((m) => m[1]!);
    expect(names).not.toHaveLength(0);
    expect(Object.keys(PAGER_BLOCKED).sort()).toEqual([...names].sort());
    for (const [name, verdict] of Object.entries(PAGER_BLOCKED)) {
      expect(verdict.because.length, `${name} needs a reason`).toBeGreaterThan(0);
    }
  });

  test("all of them reach the browser as ordinary text, so all of them are shown", async () => {
    // Not a preference: every one of these reports through
    // `send_host_turn_slash_command_output`, which is an `AgentMessageChunk` on
    // `session/update` — the same update this client already draws.
    const exec = await read("xai-grok-shell/src/session/acp_session_impl/slash_exec.rs");
    for (const action of [
      "HooksList",
      "HooksAdd",
      "HooksRemove",
      "HooksTrust",
      "HooksUntrust",
      "PluginsReload",
    ]) {
      const arm = new RegExp(`BuiltinAction::${action}[^\\n]*=> \\{([\\s\\S]*?)\\n            \\}`);
      expect(arm.exec(exec)?.[1] ?? "", action).toContain("send_host_turn_slash_command_output");
    }
    const chunk = await read("xai-grok-shell/src/session/acp_session.rs");
    expect(chunk).toContain("acp::SessionUpdate::AgentMessageChunk(");

    const shown = ["hooks-list", "hooks-add", "hooks-trust", "reload-plugins"].map((n) =>
      command(n),
    );
    expect(offeredCommands(shown).map((c) => c.name)).toEqual([
      "hooks-list",
      "hooks-add",
      "hooks-trust",
      "reload-plugins",
    ]);
  });

  test("`help` is a reservation, not a command the browser is dropping", async () => {
    // It is in the pager's list and in the shell's `PAGER_COMMAND_KEYS`, but it
    // is not in `BUILTIN_COMMANDS` — nothing advertises it, so there is nothing
    // for a browser to hide.
    const shell = await read("xai-grok-shell/src/session/slash_commands.rs");
    expect(shell).not.toContain('name: "help"');
    expect(shell).toContain('"help"');
  });
});

describe("filtering", () => {
  test("an empty query is the whole catalog, commands above skills", () => {
    const catalog = [
      command("compact"),
      skill("zeta", "user"),
      skill("alpha", "bundled"),
      pluginCommand("ship", "deployer"),
      skill("beta", "user"),
    ];
    expect(rankCommands(catalog, "").map((r) => r.display)).toEqual([
      // Commands keep the shell's order; a plugin's manifest command ranks with
      // them because it runs code rather than expanding a file.
      "/compact",
      "/ship",
      // Bundled skills next, then everything else, alphabetical inside each.
      "/alpha",
      "/beta",
      "/zeta",
    ]);
  });

  test("groups are the pager's, including where a plugin command sits", () => {
    expect(groupOf({ kind: "shell" })).toBe("command");
    expect(groupOf({ kind: "plugin", source: "acme" })).toBe("command");
    expect(groupOf({ kind: "skill", source: BUNDLED_SCOPE })).toBe("bundled_skill");
    expect(groupOf({ kind: "skill", source: "acme" })).toBe("other_skill");
  });

  test("smart case: a lowercase query ignores case, an uppercase one does not", () => {
    expect(matchIndices("Compact", "comp")).toEqual([0, 1, 2, 3]);
    expect(matchIndices("compact", "Comp")).toBeNull();
    expect(matchIndices("Compact", "Comp")).toEqual([0, 1, 2, 3]);
  });

  test("matching is a subsequence, and the highlight lands on the name", () => {
    expect(matchIndices("ssh-wrap", "sw")).toEqual([0, 4]);
    const rows = rankCommands([command("ssh-wrap")], "sw");
    // Indices are computed against `/ssh-wrap`, the way the pager computes them
    // against `row.display`, so the leading slash shifts them by one.
    expect(rows[0]?.indices).toEqual([1, 5]);
  });

  test("a qualified name is reachable by its bare suffix", () => {
    // `CommandTrigger::bare_suffix_sibling`: the shell qualifies a colliding
    // skill or plugin command, and without this the qualified spelling would be
    // the only way to find it.
    expect(triggersOf("acme:login")).toEqual(["acme:login", "login"]);
    expect(triggersOf("login")).toEqual(["login"]);
    const rows = rankCommands([skill("acme:login", "plugin", "acme")], "login");
    expect(rows.map((r) => r.display)).toEqual(["/acme:login"]);
  });

  test("a fully typed name outranks everything that merely matches it", () => {
    // `trigger_owns_typed_name`, which exists so nothing can hijack a name the
    // user has finished typing.
    const rows = rankCommands([command("compact-mode"), command("compact")], "compact");
    expect(rows.map((r) => r.display)).toEqual(["/compact", "/compact-mode"]);
  });

  test("a query with a slash in it matches nothing", () => {
    expect(rankCommands([command("compact")], "com/pact")).toEqual([]);
  });

  test("a repeated name is claimed once", () => {
    const rows = rankCommands([command("ship"), pluginCommand("Ship", "acme")], "");
    expect(rows.map((r) => r.display)).toEqual(["/ship"]);
  });

  test("the dropdown shows as many rows as the pager's", async () => {
    const source = await read("xai-grok-pager/src/slash/mod.rs");
    const declared = /MAX_VISIBLE_SUGGESTIONS: usize = (\d+)/.exec(source)?.[1];
    expect(declared).toBeDefined();
    expect(MAX_VISIBLE_ROWS).toBe(Number(declared));
  });
});

describe("reading and writing the composer", () => {
  test("the menu opens on a bare slash and stays open through the token", () => {
    expect(menuQuery("/", 1)).toBe("");
    expect(menuQuery("/com", 4)).toBe("com");
    // Caret-clamped: `/` typed before existing text lists everything.
    expect(menuQuery("/compact", 1)).toBe("");
    expect(menuQuery("hello", 5)).toBeNull();
  });

  test("it closes once the caret is past the command token", () => {
    // `cursor_in_command = cursor <= command_end`: the last position inside the
    // token is the one just before the separating space, and the space itself
    // is already the argument phase.
    expect(menuQuery("/model grok", 6)).toBe("model");
    expect(menuQuery("/model grok", 7)).toBeNull();
  });

  test("accepting replaces the token and keeps whatever followed it", () => {
    const row = rankCommands([command("model")], "mod")[0]!;
    expect(row.insertText).toBe("/model ");
    expect(acceptRow("/mod", row)).toEqual({ text: "/model ", caret: 7 });
    expect(acceptRow("/mod grok-4", row)).toEqual({ text: "/model grok-4", caret: 7 });
  });

  test("every command takes arguments, so every insert ends in a space", async () => {
    // `AcpSlashCommand::from` sets `has_args: true` unconditionally; `input`
    // only ever supplies a hint.
    const source = await read("xai-grok-pager/src/slash/acp_command.rs");
    expect(source).toContain("has_args: true");
    expect(rankCommands([command("flush")], "")[0]?.insertText).toBe("/flush ");
  });

  test("the argument hint stands in for the placeholder until arguments are typed", () => {
    const catalog = [command("model", undefined, { input: { hint: "<model id>" } })];
    expect(argumentHint(catalog, "/model ")).toBe("<model id>");
    expect(argumentHint(catalog, "/model grok")).toBeNull();
    expect(argumentHint(catalog, "/model")).toBeNull();
    expect(argumentHint([command("flush")], "/flush ")).toBeNull();
  });
});
