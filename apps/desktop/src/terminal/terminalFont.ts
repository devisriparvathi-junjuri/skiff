import { invoke } from "@tauri-apps/api/core";

import { h } from "../ui/dom";
import { host } from "./terminalHost";

import { FONT_DEFAULT, S } from "../app/state";

import { panes } from "./terminalState";

import { fitShown } from "./terminal";
import { t } from "../i18n";

let saveFontSize: ReturnType<typeof setTimeout> | undefined;

/**
 * The daemon's `[appearance]` keeps the size across machines and cleared
 * caches. localStorage is the fallback for a daemon older than the setting.
 * `save: false` applies a size the daemon already has.
 */
export function setFontSize(n: number, save = true) {
  S.fontSize = clamp(n);
  try {
    localStorage.setItem("skiff.fontSize", String(S.fontSize));
  } catch {
    /* a private window keeps the size for this run only */
  }
  // A parked pane takes the size when it is unparked. A held key steps the
  // size many times, and each step would rebuild its glyph atlas.
  for (const [id, p] of panes) if (!p.parked) p.term.options.fontSize = paneFontSize(id);
  applyZoom();
  fitShown();
  if (!save) return;
  showZoom(host, t("font.app", { percent: percent(S.fontSize) }));
  // A held key steps the size many times; the config file gets the last one.
  clearTimeout(saveFontSize);
  saveFontSize = setTimeout(() => invoke("set_font_size", { size: S.fontSize }).catch(console.error), 300);
}

/** Each session's steps away from the app size, so an app zoom scales it too. */
const PANE_KEY = "skiff.paneFont";
const offsets: Record<string, number> = loadOffsets();

function loadOffsets(): Record<string, number> {
  try {
    return JSON.parse(localStorage.getItem(PANE_KEY) ?? "{}") as Record<string, number>;
  } catch {
    return {};
  }
}

/** The text size of one session's terminal. */
export function paneFontSize(id: string): number {
  return clamp(S.fontSize + (offsets[id] ?? 0));
}

/** Changes one terminal only. `step` 0 gives it the app size again. */
export function stepPaneFont(id: string, step: -1 | 0 | 1) {
  const size = step ? clamp(paneFontSize(id) + step) : S.fontSize;
  if (size === S.fontSize) delete offsets[id];
  else offsets[id] = size - S.fontSize;
  try {
    localStorage.setItem(PANE_KEY, JSON.stringify(offsets));
  } catch {
    /* a private window keeps the size for this run only */
  }
  const p = panes.get(id);
  if (p && !p.parked) p.term.options.fontSize = size;
  fitShown();
  showZoom(p?.el.isConnected ? p.el : host, percent(size));
}

/**
 * The session the font keys act on: a highlighted session row, else the
 * focused pane while the keys are in the terminal area. None on
 * a group row or any other row: those zoom the app.
 */
export function fontTarget(): string | undefined {
  const a = document.activeElement as HTMLElement | null;
  const row = a?.closest<HTMLElement>("#sidebar-scroll [data-session]");
  if (row) return row.dataset.session;
  if (S.groupPicked || !S.focused) return undefined;
  return !a || a === document.body || (host.contains(a) && !a.closest(".slot")) ? S.focused : undefined;
}

const clamp = (n: number) => Math.min(28, Math.max(8, n));
const percent = (n: number) => `${Math.round((n / FONT_DEFAULT) * 100)}%`;

let zoomTimer: ReturnType<typeof setTimeout> | undefined;

/** The new size as a percent of the default, briefly, over what it changed. */
function showZoom(over: HTMLElement, text: string) {
  host.querySelectorAll(".zoom-badge").forEach((b) => b.remove());
  const badge = over.appendChild(h("div", "zoom-badge"));
  badge.setAttribute("role", "status");
  badge.textContent = text;
  clearTimeout(zoomTimer);
  zoomTimer = setTimeout(() => badge.remove(), 1200);
}

/** The app text follows the terminal size: 13 is 1x. */
export function applyZoom() {
  document.documentElement.style.setProperty("--zoom", String(S.fontSize / FONT_DEFAULT));
}
