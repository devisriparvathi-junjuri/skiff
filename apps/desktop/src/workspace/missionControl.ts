/**
 * Mission control (#88): the Host's whole agent hierarchy as a graph on a
 * canvas, over the whole window. Admiral at the top, the fleet and project
 * Skippers under it, each project a framed cluster of session cards, Other
 * last. The strings come from the ledger: Admiral to a Skipper (a hand-off),
 * a Skipper to a pane (a task), Admiral to a pane (its own task). A plain
 * session has no string. A string takes its child's state color, and a
 * working string's dashes march toward the child. missionLayout.ts places it all.
 *
 * Running / All at the right of the search (#133): Running dims every card,
 * cluster and string that is not working or waiting, and folds a project
 * with no running session to one card. Nothing moves. The choice is stored in
 * `[appearance] mission_filter`, so the board opens with the last one, also
 * after a restart. With none stored: Running.
 * Ctrl+Shift+P searches the cards and dims the rest. The search stays until
 * Esc clears it, so a close and an open keep it.
 *
 * Drag empty space or use the wheel to pan. Ctrl+wheel and pinch zoom at the
 * pointer. Home fits the graph. A thing moves only inside its parent. Drag a
 * card to reorder it in its cluster: the others make room, and it snaps into
 * a slot. Drag a cluster by its head, or by its Skipper's card: a project in
 * a fleet moves inside the frame, which grows to hold it; a fleet, a project
 * in no fleet, Other and Admiral move on the canvas. A drop outside the
 * parent springs back. A drop never changes a session's project or fleet.
 * The order of each cluster, the offsets, the folds and the panel's fold
 * persist on this computer, per Host.
 *
 * The panel under the canvas shows the lit card: its last 12 screen lines,
 * read-only text from skiffd, its task, or a Skipper's hand-offs. One read a
 * second, for the lit card only. Ctrl+Alt+Shift+L folds it to a rail.
 *
 * The keys: Up the parent, Down the first child, Left and Right the siblings,
 * Space folds or opens a cluster, Enter goes to the lit session. A click goes
 * too. Ctrl+Shift+A or Esc closes. This file is the board. missionMotion.ts
 * opens and closes it.
 */

import { invoke } from "@tauri-apps/api/core";

import type { Project, SessionInfo } from "../platform/types";
import { agentIcon } from "../appearance/agentIcon";
import { stateIcon } from "../appearance/stateIcon";
import { childrenNote, stateLabel, statusNote } from "./programStatus";
import { DEFAULT_THEME, isLight } from "../appearance/colors";
import { themeIdFor, xtermTheme } from "../appearance/themes";
import { type Action, actionFor, keyLabel } from "../app/keys";
import { hostCan, hostInvoke } from "../app/hostCaps";
import { hostName, shownHost } from "../app/hosts";
import { settings } from "../app/panels";
import { openSkiffAgent, skiffAgentName, skiffAgentSession } from "../app/skiffAgent";
import { S, sessions } from "../app/state";
import { accent, place } from "../app/stateQueries";
import { ctxMenu } from "../ui/contextMenu";
import { h } from "../ui/dom";
import { type Entry, ledgerRows, ledgerTitle, needWord, taskState } from "./ledger";
import { fuzzy, taskTitle } from "./model";
import { skipperFace } from "./skipperFace";
import { closeMission, goToSession } from "./missionMotion";
import * as G from "./missionLayout";
import { worktreeLines } from "./sidebarLines";
import { projectBadge, treeSections } from "./tree";
import { type PluralKey, t, tn, upper } from "../i18n";
import { matchText, noteSince, sinceText } from "./missionControl/model";

/** Rows of text in the panel's screen. */
const ROWS = 12;
/** How often the open board reads the graph and the lit screen again. */
const TICK = 1000;
/** The key of the Other cluster: the sessions in no project. */
const OTHER = "project:\u0000other";

const root = h("div", "mission");
root.id = "mission-control";
root.hidden = true;
root.tabIndex = -1;
root.setAttribute("role", "dialog");
root.setAttribute("aria-label", t("cmd.missionControl"));
document.body.appendChild(root);

const fault = h("div", "mission-fault");
fault.hidden = true;
const canvas = h("div", "mission-canvas");
const world = h("div", "mission-world");
const strings = document.createElementNS("http://www.w3.org/2000/svg", "svg");
strings.setAttribute("class", "mission-strings");
// Paths stay across draws, so a working string's dashes keep marching (#198). Labels draw over them.
const strandLayer = document.createElementNS("http://www.w3.org/2000/svg", "g");
const labelLayer = document.createElementNS("http://www.w3.org/2000/svg", "g");
strings.append(strandLayer, labelLayer);
/** One group per string, by `from>to`: its line and its tucks. */
const strands = new Map<string, { g: SVGGElement; line: SVGPathElement; tuck: SVGPathElement }>();
/** The labels as last drawn, so a draw that changes none leaves them be. */
let labelsDrawn = "";
/** One march of a working string, in ms: one dash period. All strings march in one phase. */
const MARCH = 1600;
const items = h("div", "mission-items");
world.append(strings, items);
const ticks = h("div", "mission-ticks");
const zoomBar = h("div", "mission-zoom");
const zoomLevel = h("span", "level");
const zoomBtn = (text: string, title: string, run: () => void) => {
  const b = h("button", "", text);
  b.type = "button";
  b.tabIndex = -1;
  b.title = title;
  b.addEventListener("pointerdown", (e) => e.stopPropagation());
  b.addEventListener("click", run);
  return b;
};
canvas.append(world, ticks, zoomBar);
const panel = h("div", "mission-panel");
root.append(fault, canvas, panel);

let lit: string | null = null;
let timer: number | undefined;
let reading = false;
// Set by board.stop(): no draw or read until board.start().
let paused = false;
let screenFor: string | null = null;
const screen = h("pre", "mission-screen");

/** The view: pan and zoom, with the app zoom in the scale. */
let view: G.View = { x: 0, y: 0, z: 1 };
const appZoom = () => parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--zoom")) || 1;

// ---- What persists on this computer, per Host ----

interface Kept {
  offs: G.Offsets;
  order: G.Order;
  folded: string[];
  opened: string[];
  panel: boolean;
}
let offs: G.Offsets = {};
let order: G.Order = {};
/** Folded by hand, and opened by hand against the Running rule. */
let folded = new Set<string>();
let opened = new Set<string>();
let panelOpen = true;
let keptFor: string | null | undefined;
const keptKey = () => `skiff.mission.${shownHost() ?? "local"}`;

function loadKept() {
  const host = shownHost();
  if (keptFor === host) return;
  keptFor = host;
  let k: Partial<Kept> = {};
  try {
    const v: unknown = JSON.parse(localStorage.getItem(keptKey()) ?? "{}");
    if (v && typeof v === "object") k = v as Partial<Kept>;
  } catch {
    /* start as laid out */
  }
  offs = k.offs && typeof k.offs === "object" ? k.offs : {};
  order = k.order && typeof k.order === "object" ? k.order : {};
  folded = new Set(Array.isArray(k.folded) ? k.folded : []);
  opened = new Set(Array.isArray(k.opened) ? k.opened : []);
  panelOpen = k.panel !== false;
}

function saveKept() {
  // Only a fleet, a project and Admiral keep an offset; a cluster keeps an order. A card's old offset goes.
  const keep = new Set([...placed.clusters.keys(), ...(m.g.admiral ? [m.g.admiral] : [])]);
  for (const k of Object.keys(offs)) if (!keep.has(k)) delete offs[k];
  for (const k of Object.keys(order)) if (!keep.has(k)) delete order[k];
  try {
    localStorage.setItem(keptKey(), JSON.stringify({ offs, order, folded: [...folded], opened: [...opened], panel: panelOpen } satisfies Kept));
  } catch {
    /* the layout lasts for this run */
  }
}

// ---- The search and Running / All, in the app's top bar ----

/**
 * The search, in the app's top bar where the palette field sits; it shows
 * only while the board is open.
 */
const search = h("input", "mission-search");
search.type = "text";
search.autocomplete = "off";
search.spellcheck = false;
search.placeholder = t("mission.search");
search.setAttribute("aria-label", t("mission.search"));
const find = h("label", "tb-cell mission-find");
find.appendChild(search);
document.getElementById("open-switcher")?.after(find);

/** Running / All, at the right of the search. `running`: only sessions that work or wait stand out. */
let running = true;
const scope = h("div", "mission-scope");
scope.setAttribute("role", "group");
scope.setAttribute("aria-label", t("mission.scopeAria"));
const allCount = h("span", "mission-scope-n");
const runningCount = h("span", "mission-scope-n");
const scopeAll = h("button", "mission-scope-btn");
scopeAll.append(t("mission.all"), allCount);
const scopeRunning = h("button", "mission-scope-btn");
scopeRunning.append(h("span", "mission-scope-dot"), t("mission.running"), runningCount);
for (const [b, on] of [[scopeRunning, true], [scopeAll, false]] as const) {
  b.type = "button";
  b.tabIndex = -1;
  // The keys stay where they are: on the board or in the search.
  b.addEventListener("mousedown", (e) => e.preventDefault());
  b.addEventListener("click", (e) => {
    // Inside the search's label: the click must not go on to the search.
    e.preventDefault();
    setRunning(on, true);
  });
  scope.appendChild(b);
}
find.appendChild(scope);

/** A session that works or waits. An idle session, a shell at its prompt and an exited one are not. */
const isRunning = (s: SessionInfo) => s.state === "working" || s.state === "waiting";

/** Shows Running or All. `store`: a change by the user, kept in the config. */
function setRunning(on: boolean, store = false) {
  if (store && on !== running) invoke("set_mission_filter", { running: on }).catch(console.error);
  running = on;
  scopeAll.setAttribute("aria-pressed", String(!on));
  scopeRunning.setAttribute("aria-pressed", String(on));
  if (!root.hidden) draw();
}
setRunning(true);

/** The stored filter, from `[appearance] mission_filter` when the app starts. Only `all` shows all. */
export function restoreRunning(filter: string | null | undefined) {
  setRunning(filter !== "all");
}

/** The Running key here: only the running sessions stand out, or all. */
export function toggleRunning() {
  setRunning(!running, true);
}

// ---- The model: the graph from the sessions, the projects and the ledger ----

/** A project's sessions in tree order, less its Skipper, which has a card of its own. */
function projectSessions(p: Project | null): SessionInfo[] {
  const seen = new Set<string>();
  const out: SessionInfo[] = [];
  for (const w of p ? p.worktrees : [null]) {
    for (const l of worktreeLines(w)) {
      if (l.pointer || l.group) continue;
      const s = l.session;
      if (!seen.has(s.id) && !s.coordinator && !s.skipper) {
        seen.add(s.id);
        out.push(s);
      }
    }
  }
  return out;
}

interface Str {
  from: string;
  /** The session the string goes to; its card, or its folded cluster, is where it ends. */
  to: string;
  label: string;
}

interface Model {
  g: G.Graph;
  /** Every session on the board by id: the cards, the Skippers and Admiral. */
  sess: Map<string, SessionInfo>;
  /** Each cluster's project. Other has none. */
  projects: Map<string, Project | null>;
  /** Each project cluster's sessions, folded or not. */
  members: Map<string, SessionInfo[]>;
  /** A session's ledger task, for its meta line and its string. */
  rows: Map<string, Entry>;
  strs: Str[];
  /** The sessions the search leaves. `null`: no search. */
  found: Set<string> | null;
}

/** The Skipper whose rows these are: Admiral for `grand`, else the session with that scope. */
function ownerOf(owner: string, admiral: SessionInfo | null): SessionInfo | null {
  if (owner === "grand") return admiral;
  return [...sessions.values()].find((s) => s.skipper === owner && s.state !== "done") ?? null;
}

function model(): Model {
  const admiral = skiffAgentSession();
  const sess = new Map<string, SessionInfo>();
  const projects = new Map<string, Project | null>();
  const members = new Map<string, SessionInfo[]>();
  const rows = new Map<string, Entry>();
  const ledger = hostCan("ledger") ? ledgerRows().filter((r) => r.bucket !== "finished") : [];
  for (const r of ledger) if (r.kind === "task" && r.pane_id) rows.set(r.pane_id, r);
  const q = search.value.trim();
  const found = q ? new Set<string>() : null;
  if (admiral) sess.set(admiral.id, admiral);

  const cluster = (p: Project | null, skipper: SessionInfo | null): G.GProject => {
    const key = p ? `project:${p.name}` : OTHER;
    // The cards in the hand order, then the new ones in tree order.
    const byId = new Map(projectSessions(p).map((s) => [s.id, s]));
    const list = G.orderCards([...byId.keys()], order[key]).map((id) => byId.get(id)!);
    projects.set(key, p);
    members.set(key, list);
    for (const s of list) {
      sess.set(s.id, s);
      if (found && fuzzy(q, matchText(s, p, rows.get(s.id))) >= 0) found.add(s.id);
    }
    if (skipper) sess.set(skipper.id, skipper);
    // Running folds a cluster with nothing running. A fold or an open by hand wins.
    const fold = folded.has(key) || (!opened.has(key) && running && !list.some(isRunning));
    // A plan heads the cards of its tasks in this cluster.
    const plans: G.GPlan[] = [];
    for (const s of list) {
      const r = rows.get(s.id);
      const plan = r?.parent ? ledger.find((x) => x.uid === r.parent && x.kind === "plan") : null;
      if (!plan || plans.some((pl) => pl.uid === plan.uid)) continue;
      const cards = list.filter((t) => rows.get(t.id)?.parent === plan.uid);
      plans.push({ uid: plan.uid, title: ledgerTitle(plan), cards: cards.map((t) => t.id), done: cards.filter((t) => rows.get(t.id)!.state === "done").length });
    }
    return { key, name: p?.name ?? t("sidebar.other"), skipper: skipper?.id ?? null, cards: list.map((s) => s.id), plans, folded: fold };
  };

  const g: G.Graph = { admiral: admiral?.id ?? null, fleets: [], projects: [] };
  for (const sec of treeSections()) {
    if (sec.fleet) {
      const skipper = [...sessions.values()].find((s) => s.skipper === `fleet:${sec.fleet}` && s.state !== "done") ?? null;
      if (skipper) sess.set(skipper.id, skipper);
      g.fleets.push({ key: `fleet:${sec.fleet}`, name: sec.fleet, skipper: skipper?.id ?? null, projects: sec.projects.map((p) => cluster(p, [...sessions.values()].find((s) => s.skipper === p.name && s.state !== "done") ?? null)) });
    } else {
      for (const p of sec.projects) g.projects.push(cluster(p, [...sessions.values()].find((s) => s.skipper === p.name && s.state !== "done") ?? null));
    }
  }
  if (projectSessions(null).length) g.projects.push(cluster(null, null));
  if (found) for (const s of sess.values()) if (s.coordinator || !!s.skipper) if (fuzzy(q, `${cardName(s)} ${s.state}`) >= 0) found.add(s.id);

  // The strings. A hand-off ties Admiral to the Skipper it went to; a task ties its owner to its pane.
  const strs: Str[] = [];
  for (const r of ledger) {
    if (!r.pane_id || !sess.has(r.pane_id)) continue;
    const from = r.kind === "handoff" ? admiral : r.kind === "task" ? ownerOf(r.owner, admiral) : null;
    if (!from || !sess.has(from.id) || from.id === r.pane_id) continue;
    strs.push({ from: from.id, to: r.pane_id, label: r.kind === "handoff" ? ledgerTitle(r) : "" });
  }
  return { g, sess, projects, members, rows, strs, found };
}

let m: Model = { g: { admiral: null, fleets: [], projects: [] }, sess: new Map(), projects: new Map(), members: new Map(), rows: new Map(), strs: [], found: null };
let placed: G.Placed = { cards: new Map(), clusters: new Map(), plans: [], order: new Map(), offs: {} };

// ---- Cards and clusters ----

function chevron(): HTMLElement {
  const el = h("span", "mission-chev");
  el.innerHTML = '<svg viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.3"><path d="M1.5 3.5 5 7l3.5-3.5"/></svg>';
  return el;
}

/** A card's name: Admiral's, "Skipper · gesher" for a Skipper, else the session's title. */
const cardName = (s: SessionInfo) => (s.coordinator ? skiffAgentName() : taskTitle(s));
const dimmed = (s: SessionInfo) => (running && !isRunning(s)) || (!!m.found && !m.found.has(s.id));
const stateWord = (s: SessionInfo) => t(s.state === "working" ? "state.working" : s.state === "waiting" ? "state.waiting" : s.state === "done" ? "state.done" : "state.idle");

/** A Skipper's counts over the rows it owns: its panes that work, that wait, and the rest that are open. */
function skipperCounts(s: SessionInfo): { working: number; waiting: number; open: number; handoffs: number } {
  const own = ledgerRows().filter((r) => r.bucket !== "finished" && r.kind === "task" && r.owner === (s.coordinator ? "grand" : s.skipper));
  const panes = own.map((r) => (r.pane_id ? sessions.get(r.pane_id) : null));
  const working = panes.filter((p) => p?.state === "working").length;
  const waiting = panes.filter((p) => p?.state === "waiting").length;
  const handoffs = ledgerRows().filter((r) => r.bucket !== "finished" && r.kind === "handoff" && (s.coordinator ? r.state !== "done" : r.pane_id === s.id)).length;
  return { working, waiting, open: Math.max(0, own.filter((r) => r.state !== "done").length - working - waiting), handoffs };
}

/** The card's meta line: the task and its uid, a shell's program, or the state. */
function metaOf(s: SessionInfo): HTMLElement {
  const el = h("span", "m");
  if (s.coordinator || !!s.skipper) {
    if (!hostCan("ledger")) {
      el.textContent = t("mission.noLedgerMeta");
      return el;
    }
    const c = skipperCounts(s);
    const part = (n: number, key: PluralKey, cls: string) => h("span", n ? cls : "", tn(key, n));
    if (s.coordinator) {
      el.append(part(c.handoffs, "mission.handoffs", ""), " · ", part(c.working + c.waiting + c.open, "mission.tasks", ""));
      return el;
    }
    el.append(part(c.working, "mission.working", "g"), " · ", part(c.waiting, "mission.waiting", "w"), " · ", part(c.open, "mission.open", ""));
    if (c.handoffs) el.append(" · ", tn("mission.handoffs", c.handoffs));
    return el;
  }
  // What the program reports: what it waits for, how its turn ended, its subtasks.
  const note = [statusNote(s), childrenNote(s)].filter(Boolean).join(" · ");
  const r = m.rows.get(s.id);
  if (r) {
    el.append(ledgerTitle(r), " ", h("span", "mono", r.uid));
    if (note) el.append(" · ", note);
    return el;
  }
  if (note) {
    el.textContent = note;
    return el;
  }
  if (s.role === "shell" && s.foreground_program && s.state === "working") el.textContent = s.foreground_program;
  else if (s.state === "done") el.textContent = s.exit_code ? t("mission.exitedWith", { code: s.exit_code }) : t("mission.exited");
  else if (s.state === "idle" && s.unread) el.textContent = t("mission.turnEnded");
  else if (s.state === "idle" && s.role === "shell") el.textContent = t("mission.atPrompt");
  else el.textContent = stateWord(s);
  return el;
}

/** The card's inside: the pin, the head and the body. `label`: the head's name; none inside a cluster. `num`: a Skipper's number key. */
function cardParts(s: SessionInfo, label: string | null, now: number, num = 0): HTMLElement[] {
  const pin = h("span", `mission-pin ${s.coordinator || !!s.skipper ? "accent" : s.state === "working" || s.state === "waiting" || s.state === "done" ? s.state : ""}`);
  const head = h("span", "head");
  if (num) head.appendChild(h("span", "num", String(num)));
  // A Skipper's card: its face with its model beside it, as the Skippers list shows them.
  if (label !== null) head.append(skipperFace(s.coordinator ? null : (s.skipper ?? ""), 18), agentIcon(s), h("span", "name", label));
  head.appendChild(h("span", "at", sinceText(s, now)));
  const body = h("span", "body");
  const tx = h("span", "tx");
  tx.append(h("span", "t", cardName(s)), metaOf(s));
  body.append(...(label !== null ? [stateIcon(s)] : [stateIcon(s), agentIcon(s)]), tx);
  return [pin, head, body];
}

/** A folded cluster's card: the badge, the name and the counts. */
function folderParts(key: string, now: number): HTMLElement[] {
  const p = m.projects.get(key) ?? null;
  const list = m.members.get(key) ?? [];
  const waits = list.filter((s) => s.state === "waiting").length;
  const works = list.filter((s) => s.state === "working").length;
  const pin = h("span", `mission-pin ${waits ? "waiting" : works ? "working" : ""}`);
  const head = h("span", "head");
  head.append(p ? projectBadge(p) : h("span", "name", "…"), h("span", "name", p?.name ?? t("sidebar.other")), h("span", "at", String(list.length)));
  const body = h("span", "body");
  const tx = h("span", "tx");
  const meta = h("span", "m");
  if (waits) meta.append(h("span", "w", tn("mission.wait", waits)), " · ");
  if (works) meta.append(h("span", "g", tn("mission.working", works)), " · ");
  meta.append(tn("mission.idle", list.length - waits - works));
  tx.append(h("span", "t", p?.name ?? t("sidebar.other")), meta);
  body.append(chevron(), tx);
  void now;
  return [pin, head, body];
}

/** A cluster's head: the chevron, the badge, the name and the counts. */
function clusterHeadParts(key: string, fleet: boolean): HTMLElement[] {
  const list = fleet ? (m.g.fleets.find((f) => f.key === key)?.projects ?? []).flatMap((p) => m.members.get(p.key) ?? []) : (m.members.get(key) ?? []);
  const waits = list.filter((s) => s.state === "waiting").length;
  const works = list.filter((s) => s.state === "working").length;
  const roll = h("span", "mission-roll");
  if (waits) roll.appendChild(h("span", "w", tn("mission.wait", waits)));
  if (works) roll.appendChild(h("span", "g", tn("mission.working", works)));
  roll.appendChild(h("span", "", fleet ? tn("palette.sessions", list.length) : String(list.length)));
  if (fleet) return [h("span", "name", key.slice("fleet:".length)), roll];
  const p = m.projects.get(key) ?? null;
  return [chevron(), p ? projectBadge(p) : h("span", "name", "…"), h("span", "name", p?.name ?? t("sidebar.other")), roll];
}

/** Markup to compare, less the busy icon's phase, which is new on each call. */
const strip = (html: string) => html.replace(/--(water|skiff|blink|face-turn)-delay:[^;"]*;?\s*/g, "");

/** Puts `parts` in `el` when they differ from what is there. The element stays, so its light does not flicker. */
function refill(el: HTMLElement, parts: HTMLElement[]) {
  const next = parts.map((p) => strip(p.outerHTML)).join("");
  if (el.dataset.html === next) return;
  el.dataset.html = next;
  el.replaceChildren(...parts);
}

const cards = new Map<string, HTMLElement>();
/** The Skippers' cards in number-key order: Ctrl+Shift+1 opens the first. */
let numbered: string[] = [];
const clusters = new Map<string, HTMLElement>();
const plans = new Map<string, HTMLElement>();

function setClass(el: HTMLElement, cls: string, on: boolean) {
  el.classList.toggle(cls, on);
}

/** Draws the graph as the model has it now: cards, clusters, plans and strings, keyed so what stays stays. */
function draw() {
  const now = Date.now();
  m = model();
  for (const s of m.sess.values()) noteSince(s, now);
  placed = G.placeGraph(m.g, offs);
  // The light: a card that left the board hands it to its parent, else the first node.
  if (lit && !G.standIn(m.g, placed, lit) && !placed.clusters.has(lit)) lit = G.navParent(m.g, lit) ?? G.navFirst(m.g);
  if (!lit) lit = G.navFirst(m.g);

  numbered = G.skipperOrder(m.g).filter((id) => placed.cards.has(id)).slice(0, 9);
  const seen = new Set<string>();
  // Cards: sessions, Skippers and Admiral; and folded clusters as cards.
  const putCard = (id: string, r: G.Rect, parts: HTMLElement[], cls: string, pc: string, dim: boolean) => {
    seen.add(id);
    let el = cards.get(id);
    if (!el) {
      el = h("div", "mission-card");
      el.dataset.id = id;
      cards.set(id, el);
      items.appendChild(el);
    }
    el.className = `mission-card ${cls}`;
    setClass(el, "lit", id === lit);
    setClass(el, "dim", dim);
    setClass(el, "dragging", drag?.card?.id === id);
    el.style.setProperty("--pc", pc);
    // A card on the move stays under the pointer; its slot waits for it.
    if (drag?.card?.id !== id) {
      el.style.left = `${r.x}px`;
      el.style.top = `${r.y}px`;
    }
    refill(el, parts);
  };
  for (const [id, r] of placed.cards) {
    const s = m.sess.get(id);
    if (!s) continue;
    const inCluster = !s.coordinator && !s.skipper;
    const p = inCluster ? (place(s)?.project ?? null) : null;
    const cls = [s.coordinator ? "admiral" : !!s.skipper ? "skipper" : "", s.state === "waiting" ? "waiting" : ""].join(" ");
    const pc = inCluster ? accent(p) : "var(--accent)";
    const label = inCluster ? null : cardName(s);
    putCard(id, r, cardParts(s, label, now, numbered.indexOf(id) + 1), cls, pc, dimmed(s));
    const label2 = t("mission.tileLabel", { title: cardName(s), state: stateLabel(s) });
    if (cards.get(id)!.getAttribute("aria-label") !== label2) cards.get(id)!.setAttribute("aria-label", label2);
  }
  for (const [key, c] of placed.clusters) {
    if (!c.folded) continue;
    const list = m.members.get(key) ?? [];
    const run = list.some(isRunning);
    putCard(key, c, folderParts(key, now), `folder${list.some((s) => s.state === "waiting") ? " waiting" : ""}`, accent(m.projects.get(key) ?? null), (running && !run) || (!!m.found && !list.some((s) => m.found!.has(s.id))));
  }
  for (const [id, el] of cards) {
    if (seen.has(id)) continue;
    el.remove();
    cards.delete(id);
  }

  // Clusters: the frames.
  const seenClusters = new Set<string>();
  for (const [key, c] of placed.clusters) {
    if (c.folded) continue;
    seenClusters.add(key);
    let el = clusters.get(key);
    if (!el) {
      el = h("div", "mission-cluster");
      el.dataset.id = key;
      el.appendChild(h("div", "mission-cluster-head"));
      clusters.set(key, el);
      items.appendChild(el);
    }
    const list = c.fleet ? (m.g.fleets.find((f) => f.key === key)?.projects ?? []).flatMap((p) => m.members.get(p.key) ?? []) : (m.members.get(key) ?? []);
    const skipper = c.fleet ? m.g.fleets.find((f) => f.key === key)?.skipper : null;
    const run = list.some(isRunning) || (skipper ? isRunning(m.sess.get(skipper)!) : false);
    setClass(el, "fleet", c.fleet);
    setClass(el, "lit", key === lit);
    setClass(el, "dim", (running && !run) || (!!m.found && !list.some((s) => m.found!.has(s.id))));
    el.style.setProperty("--pc", c.fleet ? "var(--accent)" : accent(m.projects.get(key) ?? null));
    Object.assign(el.style, { left: `${c.x}px`, top: `${c.y}px`, width: `${c.w}px`, height: `${c.h}px` });
    refill(el.firstElementChild as HTMLElement, clusterHeadParts(key, c.fleet));
  }
  for (const [key, el] of clusters) {
    if (seenClusters.has(key)) continue;
    el.remove();
    clusters.delete(key);
  }

  // Plans: captions.
  const seenPlans = new Set<string>();
  for (const pl of placed.plans) {
    seenPlans.add(pl.key);
    let el = plans.get(pl.key);
    if (!el) {
      el = h("div", "mission-plan");
      plans.set(pl.key, el);
      items.appendChild(el);
    }
    const text = t("mission.plan", { title: pl.plan.title, done: pl.plan.done, total: pl.plan.cards.length });
    if (el.textContent !== text) el.textContent = text;
    setClass(el, "dim", running && !pl.plan.cards.some((id) => isRunning(m.sess.get(id)!)));
    Object.assign(el.style, { left: `${pl.x}px`, top: `${pl.y}px`, width: `${pl.w}px` });
  }
  for (const [key, el] of plans) {
    if (seenPlans.has(key)) continue;
    el.remove();
    plans.delete(key);
  }

  drawStrings();
  counts();
  find.style.setProperty("--pc", cards.get(lit ?? "")?.style.getPropertyValue("--pc") ?? clusters.get(lit ?? "")?.style.getPropertyValue("--pc") ?? "");
  fault.hidden = hostCan("ledger");
  if (!fault.hidden) fault.textContent = t("mission.noLedgerFault", { host: hostName() });
  drawTicks();
  drawPanel();
}

/** The strings: one path per link, from the parent's foot to the child's pin. A hand-off carries its label. */
function drawStrings() {
  const done = new Set<string>();
  let labels = "";
  for (const st of m.strs) {
    const to = G.standIn(m.g, placed, st.to);
    const from = placed.cards.get(st.from);
    if (!to || !from) continue;
    const key = `${st.from}>${to}`;
    if (done.has(key)) continue;
    done.add(key);
    const toRect = placed.cards.get(to) ?? placed.clusters.get(to)!;
    const child = m.sess.get(st.to)!;
    const foldedTo = !placed.cards.has(to);
    const members = foldedTo ? (m.members.get(to) ?? []) : [child];
    const state = members.some((s) => s.state === "waiting") ? "waiting" : members.some((s) => s.state === "working") ? "working" : stateWord(child);
    const dim = foldedTo ? !members.some(isRunning) && running : dimmed(child);
    const g = G.strand(G.foot(from), G.pin(toRect));
    let sd = strands.get(key);
    if (!sd) {
      const svg = "http://www.w3.org/2000/svg";
      sd = { g: document.createElementNS(svg, "g"), line: document.createElementNS(svg, "path"), tuck: document.createElementNS(svg, "path") };
      sd.line.setAttribute("class", "line");
      sd.g.append(sd.line, sd.tuck);
      strands.set(key, sd);
      strandLayer.appendChild(sd.g);
    }
    const cls = `${state}${dim ? " dim" : ""}`;
    if (sd.g.getAttribute("class") !== cls) {
      // A string that starts to work joins the others' phase.
      if (state === "working" && !sd.g.classList.contains("working")) sd.line.style.animationDelay = `${-(performance.now() % MARCH)}ms`;
      sd.g.setAttribute("class", cls);
    }
    if (sd.line.getAttribute("d") !== g.d) {
      sd.line.setAttribute("d", g.d);
      sd.tuck.setAttribute("d", g.tuck);
    }
    if (st.label) {
      const text = st.label.length > 40 ? `${st.label.slice(0, 39)}…` : st.label;
      const w = text.length * 6.2 + 12;
      labels += `<rect class="lblbg" x="${g.lx - w / 2}" y="${g.ly - 10}" width="${w}" height="20"/><text class="lbl${dim ? " dim" : ""}" x="${g.lx}" y="${g.ly + 4}" text-anchor="middle">${text.replace(/[<&]/g, (c) => (c === "<" ? "&lt;" : "&amp;"))}</text>`;
    }
  }
  for (const [key, sd] of strands) {
    if (done.has(key)) continue;
    sd.g.remove();
    strands.delete(key);
  }
  if (labels !== labelsDrawn) labelLayer.innerHTML = labelsDrawn = labels;
  // The box holds every string, so WebKit repaints them as its own content. The margin takes the sag and the labels.
  const b = G.bounds(placed);
  const w = String(Math.max(0, Math.ceil(b.x + b.w + 64)));
  const h = String(Math.max(0, Math.ceil(b.y + b.h + 64)));
  if (strings.getAttribute("width") !== w) strings.setAttribute("width", w);
  if (strings.getAttribute("height") !== h) strings.setAttribute("height", h);
}

/** A drag frame: the new places and the strings that moved. The drop draws the rest. */
function drawMoved() {
  placed = G.placeGraph(m.g, offs);
  for (const [id, r] of placed.cards) {
    const el = cards.get(id);
    if (!el || drag?.card?.id === id) continue;
    el.style.left = `${r.x}px`;
    el.style.top = `${r.y}px`;
  }
  for (const [key, c] of placed.clusters) {
    const el = c.folded ? cards.get(key) : clusters.get(key);
    if (!el) continue;
    el.style.left = `${c.x}px`;
    el.style.top = `${c.y}px`;
    if (!c.folded) Object.assign(el.style, { width: `${c.w}px`, height: `${c.h}px` });
  }
  for (const pl of placed.plans) {
    const el = plans.get(pl.key);
    if (el) Object.assign(el.style, { left: `${pl.x}px`, top: `${pl.y}px`, width: `${pl.w}px` });
  }
  drawStrings();
}

/** The top bar's counts: Running and All, of what the search leaves. */
function counts() {
  const all = [...m.sess.values()].filter((s) => !m.found || m.found.has(s.id));
  allCount.textContent = String(all.length);
  runningCount.textContent = String(all.filter(isRunning).length);
}

/** A waiting card out of view: a tick at the edge of the view, toward it. */
function drawTicks() {
  ticks.replaceChildren();
  const vw = canvas.clientWidth;
  const vh = canvas.clientHeight;
  const size = 10 * appZoom();
  for (const [id, el] of cards) {
    if (!el.classList.contains("waiting")) continue;
    const r = placed.cards.get(id) ?? placed.clusters.get(id);
    if (!r) continue;
    const at = G.edgeTick(view, r, vw, vh, size);
    if (!at) continue;
    const tick = h("button", "mission-tick");
    tick.type = "button";
    tick.tabIndex = -1;
    tick.dataset.id = id;
    tick.title = t("mission.waits", { name: el.querySelector(".t")?.textContent ?? "" });
    tick.style.left = `${at.x}px`;
    tick.style.top = `${at.y}px`;
    ticks.appendChild(tick);
  }
}

// ---- The view ----

function applyView() {
  world.style.transform = `translate(${view.x}px, ${view.y}px) scale(${view.z})`;
  const cell = 40 * view.z;
  canvas.style.backgroundSize = `${cell}px ${cell}px`;
  canvas.style.backgroundPosition = `${view.x}px ${view.y}px`;
  zoomLevel.textContent = `${Math.round((view.z / appZoom()) * 100)}%`;
  drawTicks();
}

function fit() {
  view = G.fitView(G.bounds(placed), canvas.clientWidth, canvas.clientHeight, 24 * appZoom());
  view.z = Math.min(view.z, appZoom());
  const b = G.bounds(placed);
  view.x = (canvas.clientWidth - b.w * view.z) / 2 - b.x * view.z;
  view.y = (canvas.clientHeight - b.h * view.z) / 2 - b.y * view.z;
  applyView();
}

/** The whole graph, then closer on the lit card when the fit is small. */
function openView() {
  fit();
  const z = appZoom();
  if (view.z >= 0.85 * z) return;
  const r = rectOf(lit);
  if (!r) return;
  view = G.centerOn({ ...view, z: 0.85 * z }, r, canvas.clientWidth, canvas.clientHeight);
  view.y = Math.min(24, view.y);
  applyView();
}

function zoomBy(f: number, px = canvas.clientWidth / 2, py = canvas.clientHeight / 2) {
  const z = appZoom();
  const next = G.zoomView({ ...view, z: view.z / z }, f, px, py);
  view = { ...next, z: next.z * z };
  applyView();
}

/** Where `id` is on the canvas: its card, or its folded cluster's card. */
function rectOf(id: string | null): G.Rect | null {
  if (!id) return null;
  const stand = G.standIn(m.g, placed, id) ?? (placed.clusters.has(id) ? id : null);
  return stand ? (placed.cards.get(stand) ?? placed.clusters.get(stand) ?? null) : null;
}

/** Brings `id` into view when it is out of it. */
function panTo(id: string | null) {
  const r = rectOf(id);
  if (!r || G.inView(view, r, canvas.clientWidth, canvas.clientHeight)) return;
  view = G.centerOn(view, r, canvas.clientWidth, canvas.clientHeight);
  applyView();
}

zoomBar.append(zoomBtn("−", t("mission.zoomOut", { key: "-" }), () => zoomBy(1 / 1.2)), zoomBtn("+", t("mission.zoomIn", { key: "=" }), () => zoomBy(1.2)), zoomBtn("⌂", t("mission.fitTip", { key: "Home" }), fit), zoomLevel);

// ---- The light, and the moves ----

/** `pan`: bring the card into view, for keys. The pointer is already on it. */
function light(id: string | null, pan = true) {
  if (!id) return;
  lit = id;
  for (const [k, el] of cards) el.classList.toggle("lit", k === id);
  for (const [k, el] of clusters) el.classList.toggle("lit", k === id);
  find.style.setProperty("--pc", cards.get(id)?.style.getPropertyValue("--pc") ?? clusters.get(id)?.style.getPropertyValue("--pc") ?? "");
  if (pan) panTo(id);
  drawPanel();
  void read();
}

const isCluster = (id: string) => id.startsWith("project:") || id.startsWith("fleet:");

/** Folds or opens a project cluster. A fleet folds every project in it. */
function toggleFold(id: string) {
  const keys = id.startsWith("fleet:") ? (m.g.fleets.find((f) => f.key === id)?.projects.map((p) => p.key) ?? []) : [id];
  if (!keys.length || !keys.every((k) => placed.clusters.has(k))) return;
  const anyOpen = keys.some((k) => !placed.clusters.get(k)!.folded);
  for (const k of keys) {
    if (anyOpen) {
      opened.delete(k);
      folded.add(k);
    } else {
      folded.delete(k);
      opened.add(k);
    }
  }
  saveKept();
  draw();
}

/** The next waiting card after the lit one, around the board. */
function nextWaitingCard(): string | null {
  const ids = [...cards].filter(([id, el]) => el.classList.contains("waiting") && !isCluster(id)).map(([id]) => id);
  if (!ids.length) return null;
  const i = ids.indexOf(lit ?? "");
  return ids[(i + 1) % ids.length];
}

/** The lit node's session: a card's own, a cluster's first. `null` with nothing to go to. */
function litSession(): string | null {
  if (!lit) return null;
  const id = isCluster(lit) ? (m.g.fleets.find((f) => f.key === lit)?.projects.flatMap((p) => p.cards)[0] ?? [...m.g.fleets.flatMap((f) => f.projects), ...m.g.projects].find((p) => p.key === lit)?.cards[0] ?? null) : lit;
  return id && m.sess.has(id) ? id : null;
}

/** To the lit node's session, maximized or not. */
function go(max = false) {
  const id = litSession();
  if (id) closeMission(id, max);
}

/** The board menu: the layout as a whole. */
function openMenu() {
  const at = cards.get(lit ?? "") ?? clusters.get(lit ?? "");
  const r = (at ?? canvas).getBoundingClientRect();
  const back = () => requestAnimationFrame(() => root.focus());
  ctxMenu.open(
    r.left,
    at ? r.bottom : r.top,
    upper(t("cmd.missionControl")),
    [
      { label: t("mission.arrange"), run: () => (offs = {}, order = {}, saveKept(), draw(), fit(), back()) },
      { label: t("mission.fit"), hint: "Home", run: () => (fit(), back()) },
      { sep: true },
      { label: t("mission.foldAll"), run: () => (foldAll(true), back()) },
      { label: t("mission.openAll"), run: () => (foldAll(false), back()) },
    ],
    back,
  );
}

function foldAll(fold: boolean) {
  for (const p of [...m.g.fleets.flatMap((f) => f.projects), ...m.g.projects]) {
    if (fold) {
      opened.delete(p.key);
      folded.add(p.key);
    } else {
      folded.delete(p.key);
      opened.add(p.key);
    }
  }
  saveKept();
  draw();
}

// ---- The panel under the canvas ----

const FOLD_SVG = '<svg viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.3"><path d="M1.5 3.5 5 7l3.5-3.5"/></svg>';

/** The session's terminal theme, as its pane shows it. */
function paint(pre: HTMLElement, s: SessionInfo) {
  const id = themeIdFor(s);
  const t = S.themes.find((x) => x.id === id) ?? S.themes.find((x) => x.id === DEFAULT_THEME);
  if (!t) return;
  pre.style.background = t.background;
  pre.style.color = xtermTheme(t).foreground;
  pre.style.colorScheme = isLight(t.background) ? "light" : "dark";
}

function togglePanel() {
  panelOpen = !panelOpen;
  saveKept();
  drawPanel();
  requestAnimationFrame(drawTicks);
}

function drawPanel() {
  panel.classList.toggle("folded", !panelOpen);
  const head = h("div", "mission-panel-head");
  const fold = h("button", "fold");
  fold.type = "button";
  fold.tabIndex = -1;
  fold.title = t("mission.foldPanel", { key: keyLabel("ledger") });
  fold.innerHTML = FOLD_SVG;
  fold.addEventListener("click", togglePanel);
  const body = h("div", "mission-panel-body");
  const side = h("div", "mission-side");
  const sideHead = (text: string, mono = "") => {
    const el = h("div", "mission-side-head", text);
    el.appendChild(h("span", "mono", mono));
    return el;
  };
  const kv = (pairs: [string, string, string?][]) => {
    const el = h("div", "mission-kv");
    for (const [k, v, cls] of pairs) el.append(h("div", "", k), h("div", cls ?? "", v));
    return el;
  };
  const s = lit ? m.sess.get(lit) : null;
  if (!s) {
    const list = lit ? (m.g.fleets.find((f) => f.key === lit)?.projects ?? []).flatMap((p) => m.members.get(p.key) ?? []).concat(m.members.get(lit) ?? []) : [];
    const name = lit?.startsWith("fleet:") ? lit.slice("fleet:".length) : (m.projects.get(lit ?? "")?.name ?? (lit === OTHER ? t("sidebar.other") : ""));
    head.append(h("span", "title", name), h("span", "state", `${tn("mission.waiting", list.filter((x) => x.state === "waiting").length)} · ${tn("mission.working", list.filter((x) => x.state === "working").length)} · ${tn("palette.sessions", list.length)}`), fold);
    screen.textContent = "";
    screen.classList.add("note");
    screen.removeAttribute("style");
    screenFor = null;
    side.appendChild(h("div", "mission-task none", t("mission.clusterNoPane")));
  } else {
    const p = place(s)?.project ?? null;
    const title = s.coordinator || !!s.skipper ? cardName(s) : p ? `${p.name} · ${cardName(s)}` : cardName(s);
    head.append(stateIcon(s), h("span", "title", title), h("span", `state${s.state === "waiting" ? " w" : ""}`, [stateWord(s), statusNote(s), childrenNote(s), sinceText(s, Date.now())].filter(Boolean).join(" · ")), fold);
    if (screenFor !== s.id) {
      screenFor = s.id;
      screen.textContent = "";
      screen.classList.remove("note");
    }
    paint(screen, s);
    const r = m.rows.get(s.id);
    if (s.coordinator || !!s.skipper) {
      const hos = ledgerRows().filter((x) => x.bucket !== "finished" && x.kind === "handoff" && (s.coordinator ? x.state !== "done" : x.pane_id === s.id));
      side.appendChild(sideHead(t("mission.handoffsHead"), String(hos.length)));
      if (!hostCan("ledger")) side.appendChild(h("div", "mission-task none", t("mission.noLedger")));
      else if (!hos.length) side.appendChild(h("div", "mission-task none", t("mission.noneDot")));
      else side.appendChild(kv(hos.map((x) => [x.uid, `${ledgerTitle(x)} · ${s.coordinator ? (x.pane_id && sessions.get(x.pane_id) ? cardName(sessions.get(x.pane_id)!) : x.project) : t("mission.from", { name: skiffAgentName() })}`])));
    } else {
      side.appendChild(sideHead(t("mission.taskHead"), r ? r.uid : ""));
      if (!hostCan("ledger")) side.appendChild(h("div", "mission-task none", t("mission.noLedger")));
      else if (!r) side.appendChild(h("div", "mission-task none", t("mission.noTask")));
      else {
        side.appendChild(h("div", "mission-task", r.task));
        const st = taskState(r);
        const owner = ownerOf(r.owner, skiffAgentSession());
        side.appendChild(kv([
          [t("mission.owner"), owner ? cardName(owner) : r.owner === "grand" ? skiffAgentName() : r.owner],
          [t("mission.state"), needWord(st) ?? st, needWord(st) ? "w" : ""],
          [t("mission.result"), t(r.state === "done" ? (r.result_exists ? "mission.resultReady" : r.flushed_at ? "mission.resultFlushed" : "mission.resultNone") : "mission.resultNotYet"), "mono"],
        ]));
      }
    }
  }
  body.append(screen, side);
  panel.replaceChildren(head, body);
}

/** Reads the lit card's screen. One read a second, for that card only. */
async function read() {
  const id = screenFor;
  if (!id || reading || !panelOpen || paused) return;
  reading = true;
  try {
    const lines = await hostInvoke<string[]>("session_screen", "session_screen", { session: id, rows: ROWS });
    if (paused || screenFor !== id) return;
    const text = lines.join("\n");
    if (screen.textContent !== text) screen.textContent = text;
    screen.classList.remove("note");
  } catch (e) {
    if (paused || screenFor !== id) return;
    // An older skiffd does not know the request.
    screen.textContent = /unknown variant|bad request/.test(String(e)) ? t("mission.updateDaemon") : String(e);
    screen.classList.add("note");
  } finally {
    reading = false;
  }
}

function tick() {
  if (paused) return;
  // A drag draws its own frames; the drop draws the whole board.
  if (!drag?.moved) draw();
  void read();
}

// ---- The pointer: pan, drag, click ----

interface Drag {
  x: number;
  y: number;
  /** The card or cluster under the pointer, for the click. */
  id: string | null;
  /** A fleet, a project or Admiral on the move, with its offset at the start. */
  move: { key: string; o: { dx: number; dy: number } } | null;
  /** A session card on the move in its cluster, from its place at the start. */
  card: { id: string; cluster: string; x: number; y: number } | null;
  chev: boolean;
  moved: boolean;
  vx: number;
  vy: number;
}
let drag: Drag | null = null;
let dragFrame = 0;
let settleTimer: number | undefined;

canvas.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  const t = e.target as Element;
  const tick = t.closest<HTMLElement>(".mission-tick");
  if (tick?.dataset.id) {
    light(tick.dataset.id);
    return;
  }
  const el = t.closest<HTMLElement>(".mission-card, .mission-cluster");
  const head = t.closest(".mission-cluster-head");
  // A cluster moves by its head only. Its inside is the canvas.
  const id = (el && (el.classList.contains("mission-card") || head) ? el.dataset.id : null) ?? null;
  // A Skipper's card, a folded cluster and a cluster head move their block; a session card reorders in its cluster.
  const key = id ? G.moveKey(m.g, id) : null;
  const cluster = id && !key ? G.clusterOf(m.g, id) : null;
  const at = cluster ? placed.cards.get(id!) : null;
  drag = {
    x: e.clientX,
    y: e.clientY,
    id,
    move: key ? { key, o: { ...(placed.offs[key] ?? { dx: 0, dy: 0 }) } } : null,
    card: cluster && at ? { id: id!, cluster, x: at.x, y: at.y } : null,
    chev: !!t.closest(".mission-chev"),
    moved: false,
    vx: view.x,
    vy: view.y,
  };
  canvas.setPointerCapture(e.pointerId);
  if (!drag.move && !drag.card) canvas.classList.add("panning");
  e.preventDefault();
});
canvas.addEventListener("pointermove", (e) => {
  if (!drag) return;
  const dx = e.clientX - drag.x;
  const dy = e.clientY - drag.y;
  if (!drag.moved && Math.hypot(dx, dy) < 4) return;
  if (!drag.moved) canvas.classList.add("moving");
  drag.moved = true;
  if (drag.move) {
    // The layout clamps it to its parent; the stored offset follows at the drop.
    offs[drag.move.key] = { dx: drag.move.o.dx + dx / view.z, dy: drag.move.o.dy + dy / view.z };
    // One draw per frame while the pointer moves.
    if (!dragFrame) dragFrame = requestAnimationFrame(() => (dragFrame = 0, drawMoved()));
  } else if (drag.card) {
    const c = drag.card;
    const el = cards.get(c.id);
    const x = c.x + dx / view.z;
    const y = c.y + dy / view.z;
    if (el) {
      window.clearTimeout(settleTimer);
      items.classList.add("reorder");
      el.classList.add("dragging");
      el.style.left = `${x}px`;
      el.style.top = `${y}px`;
    }
    // The slot under the card's middle: the others make room. Outside the frame, the order stays.
    const slot = G.slotAt(placed, c.cluster, { x: x + G.CARD_W / 2, y: y + G.CARD_H / 2 });
    const shown = placed.order.get(c.cluster) ?? [];
    if (slot !== null && shown[slot] !== c.id) {
      order[c.cluster] = G.reorder(shown, c.id, slot);
      if (!dragFrame) dragFrame = requestAnimationFrame(() => (dragFrame = 0, drawMoved()));
    }
  } else {
    view.x = drag.vx + dx;
    view.y = drag.vy + dy;
    applyView();
  }
});
/** The dropped card goes to its slot: the last valid place. */
function settle(id: string) {
  const el = cards.get(id);
  const r = placed.cards.get(id);
  el?.classList.remove("dragging");
  if (el && r) {
    el.style.left = `${r.x}px`;
    el.style.top = `${r.y}px`;
  }
  settleTimer = window.setTimeout(() => items.classList.remove("reorder"), 150);
}
const endDrag = (e: PointerEvent) => {
  if (!drag) return;
  canvas.classList.remove("panning", "moving");
  const d = drag;
  drag = null;
  if (d.moved) {
    cancelAnimationFrame(dragFrame);
    dragFrame = 0;
    draw();
    // The offset as the layout applied it: inside the parent.
    if (d.move && placed.offs[d.move.key]) offs[d.move.key] = placed.offs[d.move.key];
    if (d.card) settle(d.card.id);
    saveKept();
    return;
  }
  if (e.type === "pointercancel") return;
  root.focus();
  if (!d.id) return;
  if (d.chev && (isCluster(d.id) || cards.get(d.id)?.classList.contains("folder"))) {
    toggleFold(d.id);
    return;
  }
  light(d.id, false);
  go();
};
canvas.addEventListener("pointerup", endDrag);
canvas.addEventListener("pointercancel", endDrag);
canvas.addEventListener("dblclick", (e) => {
  const id = (e.target as Element).closest(".mission-cluster-head")?.closest<HTMLElement>(".mission-cluster")?.dataset.id;
  if (id && id.startsWith("project:")) toggleFold(id);
});
// The wheel pans. With Ctrl (or a pinch), it zooms at the pointer. Shift+wheel pans sideways.
canvas.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const r = canvas.getBoundingClientRect();
    if (e.ctrlKey || e.metaKey) zoomBy(Math.exp(-e.deltaY * 0.0015), e.clientX - r.left, e.clientY - r.top);
    else {
      view.x -= e.shiftKey ? e.deltaY : e.deltaX;
      view.y -= e.shiftKey ? 0 : e.deltaY;
      applyView();
    }
  },
  { passive: false },
);
// Only a pointer that moves lights a card. A key pans the canvas under a
// still pointer, and the card that lands under it must not take the light back.
let pointer = "";
canvas.addEventListener("mousemove", (e) => {
  if (drag) return;
  const at = `${e.clientX},${e.clientY}`;
  if (at === pointer) return;
  pointer = at;
  const el = (e.target as Element).closest<HTMLElement>(".mission-card");
  if (el?.dataset.id && el.dataset.id !== lit) light(el.dataset.id, false);
});

// ---- The keys ----

export function missionOpen(): boolean {
  return !root.hidden;
}

/** A session's card, for the fly of the focus frame. A folded cluster's card does not stand for its sessions here. */
function tileOf(id: string | null): HTMLElement | null {
  return id && placed.cards.has(id) ? (cards.get(id) ?? null) : null;
}

/** Runs a board command. `false`: the board has no such command. */
function run(cmd: G.BoardCommand | null): boolean {
  if (!cmd) return false;
  switch (cmd) {
    case "parent": light(lit ? G.navParent(m.g, lit) : null); break;
    case "child": light(lit ? (G.navChildren(m.g, lit)[0] ?? null) : null); break;
    case "prev": light(lit ? G.navSibling(m.g, lit, -1) : null); break;
    case "next": light(lit ? G.navSibling(m.g, lit, 1) : null); break;
    case "fold": if (lit) toggleFold(isCluster(lit) ? lit : `project:${place(m.sess.get(lit)!)?.project?.name ?? ""}`); break;
    case "go": go(); break;
    case "go-max": go(true); break;
    case "fit": fit(); break;
    case "zoom-in": zoomBy(1.2); break;
    case "zoom-out": zoomBy(1 / 1.2); break;
    case "close": closeMission(); break;
    case "clear-search":
      search.value = "";
      draw();
      break;
    case "search": toggleSearch(); break;
    case "running": toggleRunning(); break;
    case "next-waiting": light(nextWaitingCard()); break;
    case "panel": togglePanel(); break;
    case "menu": openMenu(); break;
    case "settings":
      closeMission();
      void settings.open();
      break;
    case "skipper": {
      const s = skiffAgentSession();
      if (s) goToSession(s.id);
      else {
        closeMission();
        openSkiffAgent();
      }
      break;
    }
  }
  return true;
}

/** The action keys with a meaning on the open board. The menu bar sends its accelerators here through `runAction()`. */
const BOARD_ACTIONS = new Set<Action>(["session-next", "session-prev", "list-project", "list-back", "next-waiting", "ledger", "project-menu"]);

/** An action key on the open board: the moves, the next waiting card, the panel and the menu. */
export function missionKey(a: Action | null): boolean {
  if (!a || !missionOpen() || !BOARD_ACTIONS.has(a)) return false;
  return run(G.boardCommand("", a, false, false, { ctrl: false, shift: false, alt: false, meta: false }));
}

/** Ctrl+Shift+1..9 here: the Skipper card with that number, as a click on it. */
export function pickSkipper(n: number) {
  const id = numbered[n - 1];
  if (!id) return;
  light(id, false);
  go();
}

/** Ctrl+Shift+P here: into the search, or back out to the board. The search stays. */
export function toggleSearch() {
  if (document.activeElement === search) root.focus();
  else {
    search.focus();
    search.select();
  }
}

search.addEventListener("input", () => draw());
/** The board's keys, on the board and in its search. */
function onKey(e: KeyboardEvent) {
  const a = actionFor(e);
  if (a === "quit") return;
  const cmd = G.boardCommand(e.key, a, e.target === search, !!search.value, { ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey, meta: e.metaKey });
  if (!run(cmd)) return;
  e.preventDefault();
  e.stopPropagation();
}
root.addEventListener("keydown", onKey);
find.addEventListener("keydown", onKey);

const topbar = () => document.getElementById("topbar");

/**
 * What the motion code (missionMotion.ts) drives. The board owns what shows;
 * the motion owns when and how it comes and goes.
 */
export const board = {
  root,
  lit: litSession,
  tile: tileOf,
  /** Builds the board lit on `focus` and reads its screen once. Shows nothing. */
  async prepare(focus: string | null) {
    paused = false;
    loadKept();
    search.title = t("mission.searchTip", { key: keyLabel("palette") });
    scope.title = t("mission.scopeTip", { key: keyLabel("focus-mode") });
    lit = focus;
    draw();
    await read();
  },
  /** With the root shown: fit the view, bring the lit card in, and keep the graph fresh. */
  start() {
    paused = false;
    // Focus mode and maximize leave the top bar inert. The board shows it, and its search is in it.
    topbar()?.toggleAttribute("inert", false);
    openView();
    light(lit, false);
    window.clearInterval(timer);
    timer = window.setInterval(tick, TICK);
  },
  /** No draw or read from here on: an animation is about to measure. */
  stop() {
    paused = true;
    window.clearInterval(timer);
  },
  /** Empty and hidden. */
  clear() {
    window.clearInterval(timer);
    topbar()?.toggleAttribute("inert", document.body.classList.contains("focus-mode"));
    root.hidden = true;
    drag = null;
    canvas.classList.remove("panning", "moving");
    items.classList.remove("reorder");
    screenFor = null;
    for (const el of [...cards.values(), ...clusters.values(), ...plans.values()]) el.remove();
    cards.clear();
    clusters.clear();
    plans.clear();
    strandLayer.replaceChildren();
    labelLayer.replaceChildren();
    labelsDrawn = "";
    strands.clear();
    ticks.replaceChildren();
  },
};
