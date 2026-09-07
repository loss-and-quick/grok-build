// The non-visual half of a plugin panel: what a tone means, and what a button
// press carries back.
//
// The point of this module and its component is what is *not* in them: no
// plugin was changed to make its panel appear in a browser, and no panel type
// is restated here. The types are imported from `sdk/plugin/src/generated/`,
// where ts-rs put them, so a new `PanelBlock` variant is a compile error in
// `Panel.tsx` rather than a block that silently renders as nothing.
//
// `PanelTone` is the reason this works at all: a plugin says `"error"`, and the
// renderer picks the colour. Had the tone been a hex the plugin would have been
// painting for a terminal, and a second client would be repainting its output.
import type { PanelBlock } from "@grok-build/plugin/generated/PanelBlock.ts";
import type { PanelTone } from "@grok-build/plugin/generated/PanelTone.ts";

import { cssVarName, type ThemeRole } from "./theme.ts";

/** Colour role each tone paints in. The pager's own vocabulary, not a new one. */
export const TONE_ROLE: Record<PanelTone, ThemeRole> = {
  neutral: "text_secondary",
  success: "accent_success",
  warning: "warning",
  error: "accent_error",
};

export function toneColor(tone: PanelTone): string {
  return `var(${cssVarName(TONE_ROLE[tone])})`;
}

/**
 * Every `PanelBlock` variant this client draws.
 *
 * `Switch`/`Match` narrows each arm at the type level but cannot prove the set
 * is complete, so the completeness lives here: `satisfies` fails to compile the
 * day `sdk/plugin/src/generated/PanelBlock.ts` grows a sixth kind. The test
 * walks these keys and asserts each one produces DOM, so the guard and the
 * renderer cannot drift apart either.
 */
export const PANEL_BLOCK_KINDS = {
  status: true,
  markdown: true,
  table: true,
  input: true,
  actions: true,
} satisfies Record<PanelBlock["kind"], true>;

/** What a button press sends back: the button, plus every input in the panel. */
export interface PanelAction {
  panelId: string;
  buttonId: string;
  inputs: Record<string, string>;
}
