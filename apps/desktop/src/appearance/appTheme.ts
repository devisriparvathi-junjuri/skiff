import { invoke } from "@tauri-apps/api/core";
import { contrast, isLight, mix, readable } from "./colors";
import type { TerminalTheme } from "../platform/types";

/** The surfaces text and marks sit on: bg, panel, raised. Stylesheet defaults until a theme applies. */
let surfaces = ["#0f1216", "#13171c", "#1b2129"];
let lightScheme = false;

/**
 * `c` pushed away from the surfaces (toward black on a light theme, white on
 * a dark one) until it reaches `ratio` on every one of them. A color that
 * already passes comes back unchanged.
 */
function legible(c: string, ratio: number, on = surfaces): string {
  if (!/^#[0-9a-f]{6}/i.test(c)) return c;
  return readable(c, on, ratio);
}

/**
 * A mark color (icon, accent bar, project name) made readable on the current
 * app theme: at least 3:1 on its surfaces. Colors picked for dark themes turn
 * darker on a light one.
 */
export const ink = (c: string): string => {
  let v = inked.get(c);
  if (v === undefined) inked.set(c, (v = legible(c, 3)));
  return v;
};

/** `ink` per color, for the surfaces of the moment. Rows ask for the same few colors on every render. */
const inked = new Map<string, string>();

/** Minimum contrast per text tier and per mark, on every surface. */
const TEXT_MIN: Record<string, number> = {
  "--text": 7, "--text-2": 5.5, "--text-3": 4.5, "--text-dim": 3.2,
  "--state-waiting-text": 4.5, "--state-done": 4.5, "--state-idle": 3.2, "--error": 4.5,
  "--state-working": 3, "--state-waiting": 3, "--project-default": 3, "--accent": 3,
};

/** The CSS variables the app theme sets. The stylesheet provides the loading fallback. */
const VARS = [
  "--bg", "--bg-deep", "--bg-panel", "--bg-raised", "--rule", "--rule-strong",
  "--text", "--text-2", "--text-3", "--text-dim",
  "--state-working", "--state-waiting", "--state-waiting-text", "--state-waiting-bg", "--state-waiting-rule",
  "--state-done", "--state-idle", "--error", "--project-default", "--accent",
  "--hover", "--press",
  "--error-bg", "--error-bg-hover", "--error-rule", "--error-text",
  "--danger", "--danger-hover", "--danger-text", "--ok", "--scrim", "--on-working", "--watch", "--rank",
];

/**
 * App colors from a terminal theme: surfaces step from the background toward
 * the foreground, text steps back, and the states take the theme's blue,
 * yellow, and red. `null` returns to the stylesheet defaults.
 */
export function applyAppTheme(t: TerminalTheme | null) {
  const root = document.documentElement.style;
  if (!t) {
    for (const v of VARS) root.removeProperty(v);
    root.removeProperty("color-scheme");
    surfaces = ["#0f1216", "#13171c", "#1b2129"];
    lightScheme = false;
    inked.clear();
    void invoke("set_window_dark", { dark: true }).catch(() => {});
    return;
  }
  const set = themeVars(t);
  inked.clear();
  for (const [k, v] of Object.entries(set)) root.setProperty(k, v);
  root.setProperty("color-scheme", lightScheme ? "light" : "dark");
  // The GNOME header bar and its menu follow the app theme.
  void invoke("set_window_dark", { dark: !lightScheme }).catch(() => {});
}

/**
 * The app colors a theme gives, without applying them: for a part of the
 * window in another project's theme, as mission control's lanes are.
 * `color-scheme` is in the set too.
 */
export function appThemeVars(t: TerminalTheme): Record<string, string> {
  const keep = [surfaces, lightScheme] as const;
  const set = themeVars(t);
  set["color-scheme"] = lightScheme ? "light" : "dark";
  [surfaces, lightScheme] = keep;
  inked.clear();
  return set;
}

/** The theme's app colors. Sets the surfaces the contrast checks use. */
function themeVars(t: TerminalTheme): Record<string, string> {
  const bg = t.background;
  const fg = t.foreground;
  const light = isLight(bg);
  const [, red, , yellow, blue, magenta] = t.palette;
  const set: Record<string, string> = t.ui ? fromUi(t.ui, t) : {
    "--bg": bg,
    "--bg-deep": light ? mix(bg, fg, 0.04) : mix(bg, "#000000", 0.25),
    "--bg-panel": mix(bg, fg, 0.05),
    "--bg-raised": mix(bg, fg, 0.1),
    "--rule": mix(bg, fg, 0.14),
    "--rule-strong": mix(bg, fg, 0.22),
    "--text": fg,
    "--text-2": mix(fg, bg, 0.15),
    "--text-3": mix(fg, bg, 0.35),
    "--text-dim": mix(fg, bg, 0.5),
    "--state-working": blue,
    "--state-waiting": yellow,
    "--state-waiting-text": mix(yellow, fg, 0.3),
    "--state-waiting-bg": mix(bg, yellow, 0.12),
    "--state-waiting-rule": mix(bg, yellow, 0.4),
    "--state-done": mix(fg, bg, 0.35),
    "--state-idle": mix(fg, bg, 0.5),
    "--error": red,
    "--project-default": magenta,
  };
  set["--accent"] = set["--project-default"];
  // Enforce contrast last, so theme files and derived mixes both pass.
  surfaces = [set["--bg"], set["--bg-panel"], set["--bg-raised"]];
  lightScheme = isLight(set["--bg"]);
  for (const [k, min] of Object.entries(TEXT_MIN)) set[k] = legible(set[k], min);
  Object.assign(set, alerts(set, t.palette[2]));
  // The watcher's teal takes the theme's cyan.
  set["--watch"] = legible(t.palette[6], 3);
  // The Skiff agent's rank stripes: the theme's yellow, toward the text, off the waiting amber.
  set["--rank"] = legible(mix(t.palette[3], set["--text"], 0.35), 3);
  set["--hover"] = lightScheme ? "rgba(0, 0, 0, 0.05)" : "rgba(255, 255, 255, 0.05)";
  set["--press"] = lightScheme ? "rgba(0, 0, 0, 0.09)" : "rgba(255, 255, 255, 0.09)";
  return set;
}

/** Error banners, the destructive button, the ok dot, and the modal scrim, from the theme's red and green. */
function alerts(v: Record<string, string>, green: string): Record<string, string> {
  const bg = v["--bg"];
  const red = v["--error"];
  const errorBg = mix(bg, red, lightScheme ? 0.1 : 0.14);
  const danger = lightScheme ? mix(red, "#000000", 0.15) : mix(bg, red, 0.45);
  const dangerText = lightScheme ? "#ffffff" : mix("#ffffff", red, 0.12);
  const working = v["--state-working"];
  const onWorking = [v["--bg-deep"], "#ffffff", "#000000"].reduce((a, b) => (contrast(b, working) > contrast(a, working) ? b : a));
  return {
    "--error-bg": errorBg,
    "--error-bg-hover": mix(bg, red, lightScheme ? 0.16 : 0.2),
    "--error-rule": mix(bg, red, 0.4),
    "--error-text": legible(mix(red, v["--text"], 0.2), 4.5, [errorBg, ...surfaces]),
    "--danger": danger,
    "--danger-hover": mix(danger, lightScheme ? "#000000" : "#ffffff", 0.1),
    "--danger-text": dangerText,
    "--ok": legible(green, 3),
    "--scrim": lightScheme ? "rgba(0, 0, 0, 0.32)" : "rgba(5, 7, 10, 0.72)",
    "--on-working": onWorking,
  };
}

/**
 * A theme file's own app colors (Superset's ui block, shadcn names). Any
 * missing key falls back to a mix of background and foreground.
 */
function fromUi(u: Record<string, string>, t: TerminalTheme): Record<string, string> {
  const bg = u.background ?? t.background;
  const fg = u.foreground ?? t.foreground;
  const muted = u.mutedForeground ?? mix(fg, bg, 0.45);
  const warning = u.warning ?? t.palette[3];
  const border = u.border ?? mix(bg, fg, 0.14);
  return {
    "--bg": bg,
    "--bg-deep": u.tertiary ?? u.sidebar ?? (isLight(bg) ? mix(bg, fg, 0.04) : mix(bg, "#000000", 0.25)),
    "--bg-panel": u.sidebar ?? u.card ?? mix(bg, fg, 0.05),
    "--bg-raised": u.secondary ?? u.muted ?? mix(bg, fg, 0.1),
    "--rule": border,
    "--rule-strong": mix(border, fg, 0.12),
    "--text": fg,
    "--text-2": u.sidebarForeground ?? mix(fg, bg, 0.15),
    "--text-3": mix(fg, muted, 0.5),
    "--text-dim": muted,
    "--state-working": u.primary ?? t.palette[4],
    "--state-waiting": warning,
    "--state-waiting-text": mix(warning, fg, 0.3),
    "--state-waiting-bg": mix(bg, warning, 0.12),
    "--state-waiting-rule": mix(bg, warning, 0.4),
    "--state-done": mix(fg, muted, 0.5),
    "--state-idle": muted,
    "--error": u.destructive ?? t.palette[1],
    "--project-default": u.primary ?? t.palette[5],
  };
}
