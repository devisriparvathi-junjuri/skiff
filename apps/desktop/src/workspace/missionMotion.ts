/**
 * How mission control (#88) comes and goes: open, close, go to a session,
 * maximize one, and come back from that maximize. missionControl.ts is the board.
 *
 * The board only fades. The one thing that moves is the focus frame: it flies
 * from the focused pane to its card on open, and back on close. All layout
 * work happens first, under a board at opacity 0 or 1; then both rects are
 * measured once, and the motion plays.
 */

import { S } from "../app/state";
import { maxReturn, stillNext, toggleMaximize } from "../app/modes";
import { board, missionOpen } from "./missionControl";
import { refocusTerminal, revealSession } from "./view";

const MC_MS = 160;
const EASE = "cubic-bezier(0.2, 0, 0, 1)";
const still = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

let returnTo: HTMLElement | null = null;
/** Each transition has a number. An async step that finds another number stops. */
let gen = 0;
/** An open waits for the board to read its screens. A second open waits with it. */
let preparing = false;
/** The end of the running transition, and its animations. */
let done: (() => void) | null = null;
let running: Animation[] = [];
let frame: HTMLElement | null = null;

/** Ends the running transition at once, in its end state. */
function settle() {
  gen++;
  const end = done;
  done = null;
  for (const a of running.splice(0)) a.cancel();
  frame?.remove();
  frame = null;
  board.root.classList.remove("flying", "fading");
  board.root.style.removeProperty("opacity");
  board.root.style.removeProperty("pointer-events");
  end?.();
}

/** Starts a transition: the running one ends first. Returns its number. */
function begin(end: () => void): number {
  settle();
  done = end;
  return gen;
}

/** Plays the animations; the transition ends when they do. */
function play(my: number, list: Animation[]) {
  running = list;
  Promise.all(list.map((a) => a.finished)).then(
    () => my === gen && settle(),
    () => {},
  );
}

/** A session's pane, when it is on screen. */
function paneRect(id: string | null): DOMRect | null {
  if (!id) return null;
  const r = document.querySelector(`#terminal-host .cell[data-session="${CSS.escape(id)}"]`)?.getBoundingClientRect();
  return r && r.width > 0 && r.height > 0 ? r : null;
}

/** A session's card, when all of it shows in the canvas. */
function tileRect(id: string | null): DOMRect | null {
  const tile = board.tile(id);
  const view = tile?.closest<HTMLElement>(".mission-canvas");
  if (!tile || !view) return null;
  const r = tile.getBoundingClientRect();
  const v = view.getBoundingClientRect();
  const fits = r.top >= v.top - 0.5 && r.bottom <= v.bottom + 0.5 && r.left >= v.left - 0.5 && r.right <= v.right + 0.5;
  return fits ? r : null;
}

/** The 2px project-color frame, flown from the pane to the card, or back, over the board. */
function fly(id: string, from: DOMRect, to: DOMRect): Animation {
  frame = document.createElement("div");
  frame.className = "mission-frame";
  frame.style.setProperty("--pc", board.tile(id)?.style.getPropertyValue("--pc") ?? "");
  document.body.appendChild(frame);
  board.root.classList.add("flying");
  const box = (r: DOMRect) => ({ left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px`, height: `${r.height}px` });
  return frame.animate([box(from), box(to)], { duration: MC_MS, easing: EASE, fill: "forwards" });
}

function fade(from: number, to: number): Animation {
  board.root.classList.add("fading");
  return board.root.animate([{ opacity: from }, { opacity: to }], { duration: MC_MS, easing: EASE, fill: "forwards" });
}

/** `after`: runs when the board covers the window, at the end of the open. */
export async function openMission(after?: () => void) {
  if (preparing) return;
  settle();
  if (missionOpen()) return after?.();
  const my = gen;
  preparing = true;
  returnTo = document.activeElement as HTMLElement | null;
  const id = S.focused;
  const pane = paneRect(id);
  await board.prepare(id);
  preparing = false;
  if (my !== gen) {
    // A close came first: nothing showed. A restore that waited on this open stays to be pressed again.
    board.clear();
    if (after) maxReturn.to = (restore) => void openMission(restore);
    return;
  }
  const now = begin(() => after?.());
  const root = board.root;
  const motion = !still();
  if (motion) root.style.opacity = "0";
  root.hidden = false;
  root.focus();
  board.start();
  if (!motion) return settle();
  const lit = board.lit();
  const tile = pane ? tileRect(lit) : null;
  play(now, pane && tile && lit ? [fade(0, 1), fly(lit, pane, tile)] : [fade(0, 1)]);
}

/** `go`: the session to show. `max`: maximize its pane. Without `go`, back to the panes as they were. */
export function closeMission(go?: string, max = false) {
  if (preparing) {
    gen++;
    return;
  }
  settle();
  if (!missionOpen()) return;
  board.stop();
  const root = board.root;
  const motion = !still();
  if (!go) {
    if (returnTo?.isConnected) returnTo.focus();
    else refocusTerminal();
    const id = S.focused;
    const pane = motion ? paneRect(id) : null;
    const tile = pane ? tileRect(id) : null;
    const my = begin(() => board.clear());
    if (!motion) return settle();
    root.style.pointerEvents = "none";
    play(my, pane && tile && id ? [fade(1, 0), fly(id, tile, pane)] : [fade(1, 0)]);
    return;
  }
  // Under the opaque board: show the pane, maximize it, and let the layout settle. Nothing slides.
  const my = begin(() => {
    board.clear();
    if (document.activeElement === root || document.activeElement === document.body) refocusTerminal();
  });
  stillNext();
  revealSession(go);
  if (max && !S.maximized) {
    stillNext();
    toggleMaximize();
    maxReturn.to = (restore) => void openMission(restore);
  }
  refocusTerminal();
  if (!motion) return settle();
  requestAnimationFrame(() =>
    requestAnimationFrame(() => {
      if (my !== gen) return;
      const pane = paneRect(go);
      const tile = pane ? tileRect(go) : null;
      root.style.pointerEvents = "none";
      play(my, pane && tile ? [fade(1, 0), fly(go, tile, pane)] : [fade(1, 0)]);
    }),
  );
}

/** Ctrl+Shift+Enter here: the lit session, maximized. */
export function maximizeLit() {
  const lit = board.lit();
  if (lit) closeMission(lit, true);
}

/** To a session: from the board, it closes on that session; else the session shows. */
export function goToSession(id: string | undefined) {
  if (!id) return;
  if (missionOpen()) closeMission(id);
  else revealSession(id);
}

export function toggleMission() {
  if (missionOpen() || preparing) closeMission();
  else void openMission();
}
