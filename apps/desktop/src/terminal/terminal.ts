import { openPane, parkPane, unparkPane, useWebgl } from "./terminalRuntime";
import { paneHead } from "./paneHeader";
import { applyZoom } from "./terminalFont";
/** Pane layout, terminal fitting, and terminal-area listeners. */

import { slotBody } from "../workspace/canvas";
import { isSlot } from "../workspace/layoutSlots";
import { sessionsOf } from "../workspace/layout";
import { createLayoutView } from "../workspace/layoutView";


import { modalOpen } from "../ui/dom";
import type { SessionInfo } from "../platform/types";
import { host, park } from "./terminalHost";

import { saveGroup } from "../app/groups";

import { paneMenu } from "../workspace/menus";

import { S, sessions } from "../app/state";
import { keysOffPanes } from "../app/seen";
import { activeGroupObj, viewLayout } from "../app/stateQueries";
import { type Pane, panes } from "./terminalState";
import { focusPane } from "./paneActions";
import { previewOnScreen } from "./peek";
import { hostInvoke } from "../app/hostCaps";
import { step } from "../diagnostics/stalls";

export const view = createLayoutView(host, {
  attach: (id, body) => {
    if (isSlot(id)) return slotBody(id, body);
    const pane = panes.get(id);
    if (pane) {
      unparkPane(id, pane);
      body.appendChild(pane.el);
      return;
    }
    openPane(id)
      .then((p) => {
        if (!body.isConnected) return;
        body.appendChild(p.el);
        useWebgl(id, p);
        fitShown();
        const grab = S.grab === id;
        if (grab) S.grab = null;
        if (id === S.focused && (canTakeFocus() || (grab && !modalOpen() && !previewOnScreen()))) p.term.focus();
      })
      .catch(console.error);
  },
  head: paneHead,
  focus: (id) => {
    // The focused pane of a picked group is not selected yet: a click selects it.
    if (!isSlot(id) && (id !== S.focused || keysOffPanes())) focusPane(id);
  },
  menu: (id, e) => paneMenu(id, e.clientX, e.clientY),
  resized: () => fitSoon(),
  committed: () => {
    // The drag settled: the next frame fits the final size, with no wait.
    fitNextFrame();
    const g = activeGroupObj();
    if (g) saveGroup(g);
  },
});

/** Draws the view and parks every terminal it does not show. */
export function renderLayout() {
  const layout = viewLayout();
  const changed = view.sync(layout);
  const shown = new Set(sessionsOf(layout));
  for (const [id, p] of panes) if (!shown.has(id) && (!p.parked || p.el.parentElement !== park)) parkPane(id, p);
  if (changed === "resized") fitShown();
  if (changed === "rebuilt") {
    for (const id of shown) {
      const p = panes.get(id);
      if (p) useWebgl(id, p);
    }
    requestAnimationFrame(() => {
      fitShown();
      for (const id of shown) {
        const p = panes.get(id);
        if (p) p.term.refresh(0, p.term.rows - 1);
      }
      if (S.focused && canTakeFocus()) panes.get(S.focused)?.term.focus();
    });
  }
}

/** True when no menu, dialog or input holds the keys, so a terminal may take them. */
function canTakeFocus(): boolean {
  const a = document.activeElement;
  // An empty pane of a split canvas keeps the keys while it is being filled.
  // A preview over the panes keeps the keys until the user leaves it.
  return !modalOpen() && !previewOnScreen() && (!a || a === document.body || (host.contains(a) && !a.closest(".slot")));
}

let resizeTimer: number | undefined;
/** Panes fitted since the last pty_resize. A later fit adds to them: it must not drop an earlier pane's new size. */
const resizePending = new Set<string>();
/** The size each session's pty last got from here. Before that, the daemon's size. */
const sentSize = new Map<string, [number, number]>();

/** True when the terminal has another size than the daemon's pty. */
function sizeChanged(id: string, p: Pane): boolean {
  const info = sessions.get(id);
  const [cols, rows] = sentSize.get(id) ?? [info?.cols, info?.rows];
  return cols !== p.term.cols || rows !== p.term.rows;
}

/**
 * The daemon told the session's size. Another client (`skiff attach`) can
 * resize the pty too; then the size sent from here no longer holds, and
 * the next fit sends again, as it did before the sizes were compared.
 */
export function ptySized(info: SessionInfo) {
  const sent = sentSize.get(info.id);
  if (sent && (sent[0] !== info.cols || sent[1] !== info.rows)) sentSize.delete(info.id);
}

/**
 * Frames left to wait for a terminal that cannot measure yet. A terminal
 * opens in the hidden park, where xterm measures no cell, and it measures
 * only after a frame on screen. A fit before that does nothing, and the
 * pane keeps the size skiffd saved.
 */
let fitTries = 0;
const FIT_TRIES = 10;

/** Fit every visible terminal now; tell the daemon the new sizes once the drag settles. */
export function fitShown() {
  step("fit", fitNow);
}

function fitNow() {
  cancelAnimationFrame(fitFrame);
  fitFrame = 0;
  clearTimeout(fitTimer);
  fitTimer = 0;
  lastFit = performance.now();
  const shown = [...panes].filter(([, p]) => p.el.parentElement !== park && p.el.isConnected);
  let unmeasured = false;
  for (const [id, p] of shown) {
    if (!p.fit.proposeDimensions()) {
      unmeasured = true;
      continue;
    }
    p.fit.fit();
    // A drag that moves less than a cell changes nothing: the pty keeps its size.
    if (sizeChanged(id, p)) resizePending.add(id);
  }
  if (unmeasured && fitTries < FIT_TRIES) {
    fitTries++;
    fitNextFrame();
  } else {
    fitTries = 0;
  }
  if (resizePending.size === 0) return;
  clearTimeout(resizeTimer);
  resizeTimer = window.setTimeout(() => {
    for (const id of resizePending) {
      const p = panes.get(id);
      if (!p) {
        sentSize.delete(id);
        continue;
      }
      // The drag may have come back to the sent size meanwhile.
      if (!sizeChanged(id, p)) continue;
      const { cols, rows } = p.term;
      sentSize.set(id, [cols, rows]);
      hostInvoke("session_terminal", "pty_resize", { session: id, cols, rows }).catch((e) => {
        console.error(e);
        // The pty kept its size (skiffd reloads, or the connection dropped):
        // the next fit sends again. A later size sent meanwhile stays.
        const sent = sentSize.get(id);
        if (sent && sent[0] === cols && sent[1] === rows) sentSize.delete(id);
      });
    }
    resizePending.clear();
  }, 50);
}

let fitFrame = 0;
let fitTimer = 0;
/** When fitShown last ran, in performance.now() ms. */
let lastFit = 0;
/** The least time between two fits while resizes keep coming. */
const FIT_MS = 100;

/** fitShown on the next frame, at most once per frame. */
function fitNextFrame() {
  clearTimeout(fitTimer);
  fitTimer = 0;
  if (fitFrame) return;
  fitFrame = requestAnimationFrame(() => {
    fitFrame = 0;
    fitShown();
  });
}

/**
 * A window or divider drag fires many resizes. Each fit reflows the
 * scrollback of every shown pane, so the first resize fits on the next
 * frame and the rest wait for FIT_MS since the last fit. The wait runs out
 * after the last resize too, so the panes end at the final size.
 */
function fitSoon() {
  if (fitsHeld || fitFrame || fitTimer) return;
  const wait = FIT_MS - (performance.now() - lastFit);
  if (wait <= 0) {
    fitNextFrame();
    return;
  }
  fitTimer = window.setTimeout(() => {
    fitTimer = 0;
    fitNextFrame();
  }, wait);
}

new ResizeObserver(() => fitSoon()).observe(host);

/**
 * A panel slide changes the terminal area's width on each frame. While the
 * fits are held, the terminals keep their size, clipped or with room at the
 * right, and do not reflow. The release fits them once, at the final size.
 */
let fitsHeld = false;

export function holdFits() {
  fitsHeld = true;
  cancelAnimationFrame(fitFrame);
  fitFrame = 0;
  clearTimeout(fitTimer);
  fitTimer = 0;
}

export function releaseFits() {
  fitsHeld = false;
  fitShown();
}

// The right button belongs to Skiff inside the terminal area. xterm.js and
// the program behind it (which may paste on right-click) never see it.
for (const type of ["mousedown", "mouseup", "auxclick"] as const) {
  host.addEventListener(type, (e) => {
    if (e.button === 2) e.stopPropagation();
  }, true);
}

host.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  e.stopPropagation();
  const id = (e.target as HTMLElement).closest<HTMLElement>(".cell")?.dataset.session;
  if (id) paneMenu(id, e.clientX, e.clientY);
}, true);

/** The middle of the focused pane, where key-opened menus appear. */
export function paneCenter(): [number, number] {
  const r = S.focused ? view.cellRects().get(S.focused) : undefined;
  return r ? [r.left + r.width / 2 - 140, r.top + 60] : [window.innerWidth / 2 - 140, 120];
}

applyZoom();
