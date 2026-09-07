// A plugin that publishes one panel using all five `PanelBlock` kinds.
//
// It exists to prove the claim in `docs/WEB-UI.md`: a plugin needs no change to
// render in a browser. Nothing below mentions a client, a colour, or a
// rendering surface — it publishes a `PanelViewModel` and the host decides who
// draws it. The pager draws this panel too, from the same publish.
//
// Install it by copying this directory (dereferencing `_sdk`) into
// `$GROK_HOME/plugins/panel-probe` and adding `panel-probe` to `[plugins]
// enabled` in `config.toml`. See `sdk/web/README.md`.
import { definePlugin, observed } from "./_sdk/index.ts";
import type { PanelViewModel } from "./_sdk/generated/PanelViewModel.ts";

const PANEL_ID = "panel-probe";

function panel(lastAction: string): PanelViewModel {
  return {
    id: PANEL_ID,
    title: "Panel probe",
    blocks: [
      {
        kind: "status",
        items: [
          { label: "tone", value: "neutral", tone: "neutral" },
          { label: "tone", value: "success", tone: "success" },
          { label: "tone", value: "warning", tone: "warning" },
          { label: "tone", value: "error", tone: "error" },
          { label: "last action", value: lastAction, tone: "neutral" },
        ],
      },
      {
        kind: "markdown",
        text: [
          "## Markdown block",
          "",
          "Inline `code`, **bold**, and a [link](https://example.com).",
          "",
          "```",
          "a fenced block",
          "```",
        ].join("\n"),
      },
      {
        kind: "table",
        columns: ["block", "renders"],
        rows: [
          ["status", "chips coloured by tone"],
          ["markdown", "headings, code, links"],
          ["table", "this"],
          ["input", "a text field"],
          ["actions", "buttons"],
        ],
        selectable: true,
      },
      {
        kind: "input",
        id: "note",
        label: "Note",
        placeholder: "type something, then press Echo",
        value: null,
        secret: false,
      },
      {
        kind: "input",
        id: "token",
        label: "Secret",
        placeholder: "masked",
        value: null,
        secret: true,
      },
      {
        kind: "actions",
        buttons: [
          { id: "echo", label: "Echo", key: "e" },
          { id: "clear", label: "Clear", key: "c" },
        ],
      },
    ],
  };
}

definePlugin({
  name: "panel-probe",
  hooks: {
    session_start: async (_payload, ctx) => {
      await ctx.ui.publishPanel(panel("none yet"));
      return observed();
    },
  },
  async onPanelAction(panelId, buttonId, inputs, ctx) {
    if (panelId !== PANEL_ID) return;
    // Re-publishing the same id replaces the panel, so the round-trip is
    // visible: press a button, see what the host delivered back.
    const echo =
      buttonId === "clear"
        ? "cleared"
        : `${buttonId} note=${inputs["note"] ?? ""} token=${inputs["token"] ? "***" : ""}`;
    await ctx.ui.publishPanel(panel(echo));
  },
});
