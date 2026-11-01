import { Channel } from "@tauri-apps/api/core";

import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";

import { handleOsc52 } from "./osc52";
import { park } from "./terminalHost";

import { toBytes } from "../platform/ipcBytes";

import { appKey } from "../app/keyboard";
import { traceEcho, traceGl, traceKey, traceOutput, traceRender, traceSend } from "../diagnostics/latency";
import { work } from "../diagnostics/stalls";

import { FONT_DEFAULT, S, sessions } from "../app/state";
import { paneFontSize } from "./terminalFont";
import { place, shownIds } from "../app/stateQueries";
import { bySessionPriority } from "../workspace/model";
import type { SessionInfo } from "../platform/types";
import { opening, type Pane, panes } from "./terminalState";
import { hostInvoke } from "../app/hostCaps";

export const TERM_THEME = {
  background: "#08001a",
  foreground: "#e0ccff",
  cursor: "#08001a",
  cursorAccent: "#ff00ff",
  selectionBackground: "#4400aa",
  selectionForeground: "#ffffff",
  black: "#08001a",
  red: "#ff1177",
  green: "#aaff00",
  yellow: "#ffdd00",
  blue: "#3366ff",
  magenta: "#cc00ff",
  cyan: "#00ffcc",
  white: "#d8c8ff",
  brightBlack: "#440066",
  brightRed: "#ff4499",
  brightGreen: "#ccff33",
  brightYellow: "#ffff00",
  brightBlue: "#33aaff",
  brightMagenta: "#ff00ff",
  brightCyan: "#00ffff",
  brightWhite: "#ffffff",
};

/**
 * Set when skiffd restarts. A restored session keeps its id, so a key or a
 * mouse report sent meanwhile would reach the new shell as text.
 */
let inputOff = false;

/** Drops all terminal input while `off`. A restart reloads the page after. */
export function muteInput(off: boolean) {
  inputOff = off;
}

export function openPane(id: string): Promise<Pane> {
  const existing = panes.get(id);
  if (existing) return Promise.resolve(existing);
  let pending = opening.get(id);
  if (!pending) {
    pending = createPane(id).finally(() => opening.delete(id));
    opening.set(id, pending);
  }
  return pending;
}

export const FONT = "JetBrains Mono, ui-monospace, monospace";

/**
 * xterm measures a cell once, when the terminal opens. A font that is not
 * loaded yet gives the fallback's width, and the fit leaves an empty margin
 * until a reload. So every terminal waits for the bundled font.
 */
const fontReady = Promise.all(
  ["400", "700"].map((w) => document.fonts.load(`${w} ${FONT_DEFAULT}px "JetBrains Mono"`)),
).catch(() => undefined);

async function createPane(id: string): Promise<Pane> {
  await fontReady;
  const el = document.createElement("div");
  el.className = "pane";
  park.appendChild(el);

  const info = sessions.get(id);
  const term = new Terminal({
    cols: info?.cols ?? 80,
    rows: info?.rows ?? 24,
    theme: TERM_THEME,
    fontFamily: FONT,
    fontSize: paneFontSize(id),
    lineHeight: 1.2,
    cursorBlink: true,
    // The daemon's snapshot holds 2000 lines. A pane shown again after it
    // was parked keeps that much, so every pane keeps that much.
    scrollback: 2000,
    allowProposedApi: true,
  });
  handleOsc52(term);
  const fit = new FitAddon();
  term.loadAddon(fit);
  const search = new SearchAddon();
  term.loadAddon(search);
  const box = document.createElement("div");
  box.className = "term-box";
  el.appendChild(box);
  term.open(box);

  // Returning false keeps the key out of the PTY. The window listener acts on it.
  term.attachCustomKeyEventHandler((e) => {
    traceKey(id, e);
    // Ctrl+Backspace deletes a word, as in VS Code: ^W works in bash, zsh,
    // fish, vim and the agents. xterm.js would send ^H, one character.
    if (e.key === "Backspace" && e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
      if (e.type === "keydown") term.input("\x17");
      e.preventDefault();
      return false;
    }
    return appKey(e) === null;
  });

  term.onData((data) => {
    if (inputOff) return;
    const sent = traceSend(id);
    hostInvoke("session_terminal", "pty_write", { session: id, data }).then(sent, console.error);
  });
  term.onRender(() => traceRender(id));

  const pane: Pane = { el, term, fit, search, parked: false, stream: 0, wrote: 0, unacked: 0, acking: false, sub: Promise.resolve() };
  const drop = () => {
    term.dispose();
    el.remove();
  };
  try {
    await streamOutput(id, pane);
  } catch (e) {
    drop();
    throw e;
  }
  // session_removed may have arrived during the await.
  if (!sessions.has(id)) {
    drop();
    throw new Error(`session ${id} is gone`);
  }

  panes.set(id, pane);
  return pane;
}

/**
 * Subscribes the pane to its session: a snapshot, then live output.
 * `fromBottom`: the rows the view sat above the bottom. The snapshot resets
 * the view; once xterm has parsed what came, the view goes back there.
 */
function streamOutput(id: string, pane: Pane, fromBottom = 0): Promise<void> {
  const n = ++pane.stream;
  const channel = new Channel<unknown>();
  let parsing = 0;
  channel.onmessage = (m) => {
    // A chunk can still arrive after a park, or after session_removed
    // disposed the terminal.
    const s = sessions.get(id);
    if (!s || pane.stream !== n) return;
    // skiffd's times for the next chunk, sent while the trace runs.
    if (traceEcho(id, m)) return;
    const bytes = toBytes(m);
    const traced = traceOutput(id, bytes.length);
    parsing++;
    const queued = performance.now();
    pane.term.write(bytes, () => {
      traced?.();
      // xterm parses in slices between frames: a long wait here is output it fell behind on.
      work(`xterm parse ${bytes.length} B`, performance.now() - queued);
      if (pane.stream !== n) return;
      if (--parsing === 0 && fromBottom > 0) {
        const b = pane.term.buffer.active;
        pane.term.scrollToLine(Math.max(0, b.baseY - fromBottom));
        fromBottom = 0;
      }
      ackOutput(id, pane, n, bytes.length);
      pane.wrote = n;
    });
    s.last_output_at = Date.now();
  };
  pane.unacked = 0;
  return hostInvoke("session_terminal", "subscribe_output", { session: id, stream: n, onOutput: channel });
}

/**
 * Tells Rust that xterm parsed `bytes`, so it keeps sending. One call is in
 * flight at a time; acks that arrive meanwhile add up for the next one.
 */
function ackOutput(id: string, pane: Pane, stream: number, bytes: number) {
  pane.unacked += bytes;
  if (pane.acking) return;
  pane.acking = true;
  const sent = pane.unacked;
  pane.unacked = 0;
  hostInvoke("session_terminal", "ack_output", { session: id, stream, bytes: sent })
    .catch(console.error)
    .finally(() => {
      pane.acking = false;
      // What is left belongs to the current stream: a new one resets it.
      if (pane.unacked) ackOutput(id, pane, pane.stream, 0);
    });
}

/** A pane out of the layout stops its stream; showing it again redraws it from a snapshot. */
export function parkPane(id: string, pane: Pane) {
  park.appendChild(pane.el);
  if (pane.parked) return;
  pane.parked = true;
  pane.stream++;
  pane.sub = pane.sub.then(() => hostInvoke<void>("session_terminal", "unsubscribe_output", { session: id })).catch(console.error);
}

/**
 * After a reload: each shown pane streams again on the new connection, from
 * a snapshot, and keeps its scroll position. Parked panes stay parked.
 */
export function reattachPanes() {
  for (const [id, pane] of panes) {
    if (pane.parked) continue;
    const b = pane.term.buffer.active;
    const fromBottom = b.baseY - b.viewportY;
    pane.sub = pane.sub.then(() => streamOutput(id, pane, fromBottom)).catch(console.error);
  }
}

export function unparkPane(id: string, pane: Pane) {
  if (!pane.parked) return;
  pane.parked = false;
  // The app or pane font size may have changed while it was parked.
  const size = paneFontSize(id);
  if (pane.term.options.fontSize !== size) pane.term.options.fontSize = size;
  pane.sub = pane.sub.then(() => streamOutput(id, pane)).catch(console.error);
}

/**
 * WebGL contexts are few (WebKit drops the oldest past 16) and slow to make:
 * a new one blocks the page for 100 to 200 ms. So a shown pane asks for one,
 * and the queue attaches one per frame after the switch has painted, the
 * focused pane first. Until then the pane draws with the DOM renderer. The
 * panes shown most recently keep their context, so switching back to a group
 * is cheap; older parked panes lose theirs and draw nothing while hidden.
 * The limit sits just under WebKit's 16.
 */
const GL_KEEP = 14;
/** Pane ids with a WebGL renderer, least recently shown first. */
const glOrder: string[] = [];
/** Pane ids that wait for a context, in the order they asked. */
const glWanted: string[] = [];
let glFrame = 0;

/** Asks for a WebGL renderer on a shown pane. It arrives a frame or more later. */
export function useWebgl(id: string, pane: Pane) {
  if (pane.webgl) {
    const i = glOrder.indexOf(id);
    if (i >= 0) glOrder.splice(i, 1);
    glOrder.push(id);
    return;
  }
  if (!glWanted.includes(id)) glWanted.push(id);
  // Two frames out: the switch paints with the DOM renderer first.
  if (!glFrame) glFrame = requestAnimationFrame(() => (glFrame = requestAnimationFrame(drainWebgl)));
}

/** Attaches one context per frame. The focused pane goes first: its echo is what the user waits for. */
function drainWebgl() {
  glFrame = 0;
  const shown = shownIds();
  let k = S.focused ? glWanted.indexOf(S.focused) : -1;
  if (k < 0) k = 0;
  while (glWanted.length) {
    const [id] = glWanted.splice(k, 1);
    k = 0;
    const pane = panes.get(id);
    // Parked or gone since it asked: it asks again when it shows.
    if (!pane || pane.parked || pane.webgl || !shown.includes(id)) continue;
    attachWebgl(id, pane);
    break;
  }
  if (glWanted.length) glFrame = requestAnimationFrame(drainWebgl);
}

/** Gives the pane a WebGL renderer now, and drops the oldest contexts past the limit. */
function attachWebgl(id: string, pane: Pane, warm = false) {
  const t0 = performance.now();
  try {
    // WebKitGTK without DMA-BUF shows the WebGL canvas one frame late unless the
    // drawing buffer is kept. Echo drops from ~600 ms to ~30 ms.
    const gl = new WebglAddon(true);
    gl.onContextLoss(() => {
      gl.dispose();
      if (pane.webgl !== gl) return;
      pane.webgl = undefined;
      // A shown pane gets a new context; a parked one when it shows again.
      if (panes.get(id) === pane && shownIds().includes(id)) setTimeout(() => useWebgl(id, pane), 100);
    });
    const t1 = performance.now();
    pane.term.loadAddon(gl);
    pane.webgl = gl;
    traceGl(id, t1 - t0, performance.now() - t1, warm);
  } catch (e) {
    console.warn("WebGL renderer unavailable, using DOM renderer", e);
    return;
  }
  const i = glOrder.indexOf(id);
  if (i >= 0) glOrder.splice(i, 1);
  glOrder.push(id);
  const shown = shownIds();
  for (let j = 0; glOrder.length > GL_KEEP && j < glOrder.length; ) {
    const old = glOrder[j];
    const p = panes.get(old);
    if (p && shown.includes(old)) {
      j++;
      continue;
    }
    glOrder.splice(j, 1);
    p?.webgl?.dispose();
    if (p) p.webgl = undefined;
  }
}

/** The last key or pointer press. Warming waits for a quiet moment, so it never lands under one. */
let lastInput = 0;
for (const type of ["keydown", "pointerdown", "wheel"] as const) {
  window.addEventListener(type, () => (lastInput = performance.now()), { capture: true, passive: true });
}

/** Sessions warming tried: opened and given a context, or found gone. */
const warmed = new Set<string>();

/**
 * Warms the panes the user is likely to open next, after the app starts: a
 * terminal and then a WebGL context each, one step every few idle frames, so
 * a first visit costs nothing. Projects in saved order from the selected one,
 * Other last. It stops at the context limit; the queue takes over from there.
 */
export function warmPanes() {
  let frames = 0;
  const step = () => {
    if (++frames < 3 || performance.now() - lastInput < 500) return void requestAnimationFrame(step);
    frames = 0;
    if (glOrder.length >= GL_KEEP) return;
    const id = nextToWarm();
    if (!id) return;
    const pane = panes.get(id);
    if (!pane) {
      openPane(id)
        .then((p) => {
          // Not shown: no stream. The snapshot comes again when it shows.
          if (!shownIds().includes(id)) parkPane(id, p);
        })
        .catch(() => warmed.add(id))
        .finally(() => requestAnimationFrame(step));
      return;
    }
    if (!pane.webgl && !glWanted.includes(id)) attachWebgl(id, pane, true);
    warmed.add(id);
    requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}

/** The next session with no warm pane, nearest project first. */
function nextToWarm(): string | null {
  const order = S.projects.map((p) => p.name);
  const at = Math.max(0, order.indexOf(S.selectedProject ?? ""));
  const rank = (s: SessionInfo) => {
    const i = order.indexOf(place(s)?.project.name ?? "");
    return i < 0 ? order.length : (i - at + order.length) % order.length;
  };
  const list = [...sessions.values()].sort((a, b) => rank(a) - rank(b) || bySessionPriority(a, b));
  for (const s of list) {
    if (warmed.has(s.id)) continue;
    const p = panes.get(s.id);
    if (!p || !p.webgl) return s.id;
  }
  return null;
}
