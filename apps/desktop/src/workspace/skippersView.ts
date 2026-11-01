/**
 * The Skippers view: the side panel's second view, beside the tree. One flat
 * list: the Skiff agent (Admiral) first, then each fleet Skipper and project
 * Skipper in tree order. A row is the Skipper's avatar, its project or fleet,
 * the time of its last output, and the latest line of its work. A click or
 * Enter opens its pane. The rows are session rows, so the list keys work as
 * in the tree.
 *
 * Each row starts with the Skipper's face, then its model's icon. The icon
 * carries the state, as on the tree's rows: teal or red for the watcher,
 * turning while the watcher follows open work, and the skiff under way while
 * the Skipper works.
 */
import { agentFailed, agentName, scopeName } from "./model";
import { faceSig, skipperFace } from "./skipperFace";
import { FLAG, stateIcon } from "../appearance/stateIcon";
import { icon } from "../ui/icons";
import { agentIcon, launchIcon } from "../appearance/agentIcon";
import { ledgerWorkTitle } from "./ledger";
import { rowMenu, skiffAgentMenu } from "./menus";
import { revealSession } from "./view";
import { clearSelection, keepRow } from "./selection";
import { fleetSkipperOf, openSkiffAgent, projectSkipperOf, projectWatch, SKIFF_AGENT_KEY, skiffAgentChoice, skiffAgentFolder, skiffAgentName, skiffAgentSession, skiffWatch, type WatchState } from "../app/skiffAgent";
import { treeSections } from "./tree";
import { accent } from "../app/stateQueries";
import { DEFAULT_ACCENT, S } from "../app/state";
import { $, h } from "../ui/dom";
import { tally } from "../diagnostics/stalls";
import { render } from "../app/renderRequest";
import type { Project, SessionInfo } from "../platform/types";
import { t } from "../i18n";

export type SidebarView = "tree" | "skippers";

const VIEW_KEY = "skiff.sidebar.view";

function loadView(): SidebarView {
  try {
    return localStorage.getItem(VIEW_KEY) === "skippers" ? "skippers" : "tree";
  } catch {
    return "tree";
  }
}

let view: SidebarView = loadView();

/** True while the side panel shows the Skippers list. */
export const skippersOn = () => view === "skippers";

/** Shows the tree or the Skippers list. The choice lasts across restarts. */
export function setSidebarView(v: SidebarView) {
  if (v === view) return;
  view = v;
  try {
    localStorage.setItem(VIEW_KEY, v);
  } catch {
    /* the choice lasts for this run */
  }
  render("sidebar-view");
}

/** One Skipper in the list. `s` is null only for the Skiff agent with no session. */
type Entry = { kind: "admiral" | "fleet" | "project"; s: SessionInfo | null; name: string; scope: string | null; project: Project | null };

/** The Skiff agent, then each section's fleet Skipper and project Skippers, in tree order. */
export function skipperEntries(): Entry[] {
  const out: Entry[] = [];
  if (skiffAgentFolder()) out.push({ kind: "admiral", s: skiffAgentSession(), name: skiffAgentName(), scope: null, project: null });
  for (const sec of treeSections()) {
    const fs = sec.fleet ? fleetSkipperOf(sec.fleet) : null;
    if (fs && sec.fleet) out.push({ kind: "fleet", s: fs, name: sec.fleet, scope: `fleet:${sec.fleet}`, project: null });
    for (const p of sec.projects) {
      const ps = projectSkipperOf(p.name);
      if (ps) out.push({ kind: "project", s: ps, name: p.name, scope: p.name, project: p });
    }
  }
  return out;
}

/** The Skippers that wait on the person: the count on the toggle. */
export const waitingSkippers = () => skipperEntries().filter((e) => e.s?.state === "waiting").length;

/** The latest line of a Skipper's work: its newest open hand-off or task, else the title its agent set. */
function latestLine(e: Entry): string {
  const s = e.s;
  if (!s) {
    const a = skiffAgentChoice();
    return a ? t("sidebar.agentStart", { agent: a.id }) : t("sidebar.noAgentOn");
  }
  const work = e.scope ? ledgerWorkTitle(e.scope) : null;
  return work ?? s.title?.replace(/^[^\p{L}\p{N}]+/u, "") ?? agentName(s);
}

/** `HH:MM` today, else the day: "Oct 29". */
function when(ms: number): string {
  if (!ms) return "";
  const d = new Date(ms);
  return d.toDateString() === new Date().toDateString()
    ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })
    : d.toLocaleDateString([], { month: "short", day: "numeric" });
}

/** The watcher, as the tree's Skipper rows read it. */
function watchOf(e: Entry): { state: WatchState; text: string } | null {
  if (!e.s) return null;
  return e.kind === "admiral" ? skiffWatch(e.s) : projectWatch(e.s);
}

/** The frame's state: the session state, with done split into exited and failed, and idle into unread. */
/** frameState in words, for a screen reader. */
function stateLabel(s: SessionInfo): string {
  const st = frameState(s);
  return st === "failed" ? t("skippers.failed") : st === "unread" ? t("skippers.unread") : t(`state.${s.state}`);
}

function frameState(s: SessionInfo | null): string {
  if (!s) return "off";
  if (agentFailed(s) || (s.state === "done" && s.exit_code && s.exit_code !== 0)) return "failed";
  if (s.state === "idle" && s.unread) return "unread";
  return s.state;
}

/** What a row shows, so an unchanged row is kept and its loops keep their phase. */
function rowSig(e: Entry, watch: { state: WatchState; text: string } | null, num: number): string {
  const s = e.s;
  return JSON.stringify([
    e.kind, e.name, s?.id ?? null, s?.state ?? null, s?.unread ?? false, s?.exit_code ?? null, s?.agent_exit ?? null,
    latestLine(e), when(s?.last_output_at ?? 0), watch, s?.watch?.follows?.length ?? 0, s ? S.selection.includes(s.id) : false,
    faceSig(e.kind === "admiral" ? null : e.scope), num,
  ]);
}

/** Opens a Skipper's pane, as a click on its row does. */
function openEntry(e: Entry) {
  const s = e.s;
  clearSelection();
  if (e.kind === "admiral") {
    if (!s) keepRow(SKIFF_AGENT_KEY);
    return openSkiffAgent();
  }
  revealSession(s!.id);
  requestAnimationFrame(() => keepRow(s!.id));
}

/** The fleet and project Skippers, less the Skiff agent, in list order: Ctrl+Shift+1 opens the first. */
const numbered = () => skipperEntries().filter((e) => e.kind !== "admiral").slice(0, 9);

/** Ctrl+Shift+1..9 in the Skippers list: the Skipper with that number. */
export function pickSkipperRow(n: number) {
  const e = numbered()[n - 1];
  if (!e) return;
  openEntry(e);
  lightRow(e.s!.id);
}

/** Lights a Skipper's row after a key opened it. A key can pick a row below the fold; a click cannot. */
function lightRow(id: string) {
  requestAnimationFrame(() => {
    keepRow(id);
    document.querySelector(`.sk-row[data-session="${id}"]`)?.scrollIntoView({ block: "nearest" });
  });
}

/** `num`: the row's number key, 0 for none. */
function skipperRow(e: Entry, watch: { state: WatchState; text: string } | null, num: number): HTMLElement {
  const s = e.s;
  const color = e.project ? accent(e.project) : DEFAULT_ACCENT;
  const row = h("button", "session-row sk-row" + (s ? ` st-${s.state}` : "") + (s && S.selection.includes(s.id) ? " selected" : ""));
  row.type = "button";
  row.style.setProperty("--pc", color);
  if (s) row.dataset.session = s.id;
  else row.dataset.key = SKIFF_AGENT_KEY;
  const turning = watch?.state === "runs" && s?.state === "idle" && (s.watch?.follows?.length ?? 0) > 0;
  const name = h("span", "sk-name", e.kind === "fleet" ? scopeName(e.scope!) : e.name);
  const top = h("span", "sk-top");
  if (num) top.appendChild(h("span", "sk-num", String(num)));
  // The model beside the face, as the tree shows it: the running agent, or the one a start runs.
  // The model beside the face carries the state, as on the tree's rows: the
  // watcher's color and turn on the icon, the skiff under way while it works.
  const model = s ? agentIcon(s) : launchIcon(skiffAgentChoice());
  top.append(model, ...(s?.state === "working" ? [stateIcon(s)] : []), name);
  if (e.kind === "fleet") top.appendChild(h("span", "sk-tag", "fleet"));
  top.appendChild(h("span", "sk-time", when(s?.last_output_at ?? 0)));
  const line = h("span", "sk-line");
  line.appendChild(h("span", "title", latestLine(e)));
  if (s?.unread) {
    // The tree's unread flag, at the end of the line.
    const mark = h("span", "unread");
    mark.appendChild(icon(FLAG));
    line.appendChild(mark);
    row.classList.add("unread-row");
  }
  const text = h("span", "sk-text");
  text.append(top, line);
  if (watch?.state === "runs") row.classList.add("watch-on");
  if (watch?.state === "failed" || watch?.state === "missing") row.classList.add("watch-error");
  if (turning) {
    row.classList.add("watch-turn");
    row.style.setProperty("--turn-delay", `${-(performance.now() % 8000)}ms`);
  }
  const face = skipperFace(e.kind === "admiral" ? null : e.scope, 40);
  face.classList.add("sk-face");
  row.append(face, text);
  const what = e.kind === "admiral" ? t("skippers.admiral", { name: e.name }) : t(e.kind === "fleet" ? "skipper.avatarFleet" : "skipper.avatarProject", { scope: scopeName(e.scope!), skipper: "Skipper" });
  const vars = { what, line: latestLine(e), state: s ? stateLabel(s) : t("skippers.notRunning"), watch: watch?.text ?? "" };
  row.setAttribute("aria-label", watch?.text ? t("skippers.rowLabelWatch", vars) : t("skippers.rowLabel", vars));
  if (watch?.text) row.title = watch.text;
  row.addEventListener("mousedown", (ev) => {
    if (ev.button === 0) row.focus();
  });
  row.addEventListener("click", () => openEntry(e));
  row.addEventListener("contextmenu", (ev) => {
    ev.preventDefault();
    if (s) rowMenu(s, ev.clientX, ev.clientY);
    else skiffAgentMenu(null, ev.clientX, ev.clientY);
  });
  return row;
}

let kept = new Map<string, [sig: string, row: HTMLElement]>();

/** Draws the Skippers list into the side panel. Unchanged rows stay. */
export function renderSkippers(side: HTMLElement) {
  const next = new Map<string, [string, HTMLElement]>();
  const rows: HTMLElement[] = [];
  const nums = numbered();
  for (const e of skipperEntries()) {
    const key = e.s?.id ?? SKIFF_AGENT_KEY;
    const watch = watchOf(e);
    const num = nums.indexOf(e) + 1;
    const sig = rowSig(e, watch, num);
    const old = kept.get(key);
    const row = old && old[0] === sig ? old[1] : (tally("sidebar.row"), skipperRow(e, watch, num));
    next.set(key, [sig, row]);
    rows.push(row);
  }
  kept = next;
  if (!rows.length) rows.push(h("div", "note-row", t("skippers.none")));
  if (side.children.length === rows.length && rows.every((r, i) => side.children[i] === r)) return;
  side.replaceChildren(...rows);
}

/** The TREE | SKIPPERS toggle at the top of the side panel, with the count of Skippers that wait. */
export function renderViewToggle() {
  const bar = $<HTMLElement>("sidebar-view");
  if (!bar.childElementCount) {
    for (const [v, label] of [["tree", t("skippers.tabTree")], ["skippers", t("skippers.tabSkippers")]] as const) {
      const b = h("button", "sv-tab", label);
      b.type = "button";
      b.tabIndex = -1;
      b.dataset.view = v;
      b.addEventListener("click", () => setSidebarView(v));
      bar.appendChild(b);
    }
    bar.lastElementChild!.appendChild(h("span", "sv-count"));
  }
  for (const b of bar.querySelectorAll<HTMLElement>(".sv-tab")) {
    const on = b.dataset.view === view;
    b.classList.toggle("on", on);
    b.setAttribute("aria-pressed", String(on));
  }
  const n = waitingSkippers();
  const count = bar.querySelector<HTMLElement>(".sv-count")!;
  if (count.textContent !== (n ? String(n) : "")) count.textContent = n ? String(n) : "";
}
