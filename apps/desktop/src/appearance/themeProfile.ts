import type { TerminalTheme, ThemeColorFamily } from "../platform/types";
import { isLight } from "./colors";

export type ColorFamily = ThemeColorFamily | "Unclassified";
export const COLOR_FAMILIES: readonly ColorFamily[] = ["Neutral", "Beige", "Red", "Orange", "Yellow", "Green", "Cyan", "Blue", "Purple", "Pink", "Unclassified"];

/** A theme's declared main color. Shared ANSI colors do not set its category. */
export function themeProfile(t: TerminalTheme): { mode: "Light" | "Dark"; family: ColorFamily; color: string } {
  return {
    mode: isLight(t.background) ? "Light" : "Dark",
    family: t.color_family ?? "Unclassified",
    color: t.main_color ?? t.foreground,
  };
}
