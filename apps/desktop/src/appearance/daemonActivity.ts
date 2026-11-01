import { hostCan, hostInvoke } from "../app/hostCaps";
import { hostName } from "../app/hosts";
import { isMac } from "../app/keys";
import { confirmAction } from "../ui/confirm";
import { h } from "../ui/dom";
import { S } from "../app/state";
import { place } from "../app/stateQueries";
import { projectSkipperOf, skiffAgentSession } from "../app/skiffAgent";
import { branchName, locate, scopeFleet, scopeName, taskTitle } from "../workspace/model";
import type { DaemonMetrics, DaemonStatus, SessionInfo } from "../platform/types";
import { abandonShip, emptyPick, keepLive, moveTo, type Pick, pickAll, picked, pickedText, pickRow, sinkLabel } from "./sessionPick";
import { type MessageKey, t, tn, upper } from "../i18n";

export interface ActivityState { list: SessionInfo[]; metrics: DaemonMetrics | null; version: string | null; error: string | null; timer: number | null; pick: Pick; }
export const activityState = (): ActivityState => ({ list: [], metrics: null, version: null, error: null, timer: null, pick: emptyPick() });
const bytes = (n: number) => n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`;
const folder = (path: string) => {
  const parts = path.split("/").filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : path;
};
/** "now", "5m", "3h", "2d". `zero`: under a minute reads "0m", as an uptime does. */
const age = (at: number, zero = false) => {
  if (!at) return "—";
  const seconds = Math.max(0, Math.floor((Date.now() - at) / 1000));
  if (seconds < 60) return zero ? t("activity.minutes", { n: 0 }) : t("activity.now");
  if (seconds < 3600) return t("activity.minutes", { n: Math.floor(seconds / 60) });
  if (seconds < 86400) return t("activity.hours", { n: Math.floor(seconds / 3600) });
  return t("activity.days", { n: Math.floor(seconds / 86400) });
};

export async function refreshActivity(s: ActivityState, redraw: () => void) {
  try {
    const [list, metrics, status] = await Promise.all([
      hostInvoke<SessionInfo[]>("session_inventory", "list_sessions"),
      // An older skiffd has no metrics request: the table still lists its sessions.
      hostCan("daemon_metrics") ? hostInvoke<DaemonMetrics>("daemon_metrics", "daemon_metrics").catch(() => null) : Promise.resolve(null),
      hostInvoke<DaemonStatus>("session_inventory", "daemon_status").catch(() => null),
    ]);
    s.list = list.filter((x) => !x.peek); s.metrics = metrics; s.version = status?.version ?? null; s.error = null;
    keepLive(s.pick, s.list.map((x) => x.id));
  } catch (e) { s.error = String(e); }
  redraw();
}
export function stopActivity(s: ActivityState) { if (s.timer !== null) window.clearInterval(s.timer); s.timer = null; }
export function startActivity(s: ActivityState, redraw: () => void) {
  stopActivity(s); void refreshActivity(s, redraw);
  s.timer = window.setInterval(() => void refreshActivity(s, redraw), 5000);
}

function workspace(s: SessionInfo) {
  const open = place(s);
  if (open) return { text: `${open.project.name} / ${branchName(open.worktree)}`, out: false };
  const closed = locate(S.closedProjects, s.cwd);
  // Use the sidebar's ownership lookups. A row shown there is in the workspace.
  if (s.coordinator && skiffAgentSession()?.id === s.id) return { text: t("activity.skiffAgent"), out: false };
  const fleet = scopeFleet(s.skipper);
  const shown = fleet ? S.projects.some((p) => p.fleet === fleet) : S.projects.some((p) => p.name === s.skipper);
  if (s.skipper && shown && projectSkipperOf(s.skipper)?.id === s.id) return { text: scopeName(s.skipper), out: false };
  if (s.skipper && !s.coordinator) return { text: t("activity.ownerMissing"), out: true };
  return closed ? { text: t("activity.closed", { name: closed.project.name }), out: true } : { text: t("activity.noProject"), out: true };
}
function stateRank(s: SessionInfo) { return { waiting: 0, working: 1, idle: 2, done: 3 }[s.state]; }
const program = (s: SessionInfo) => s.foreground_program || [s.command, ...s.args].join(" ");

/** The column heads, in table order. `key` is null for a head that does not sort. */
type SortKey = "state" | "session" | "program" | "workspace" | "folder" | "out" | "age";
const COLS: { label: MessageKey; key: SortKey | null; cls: string }[] = [
  { label: "activity.colState", key: "state", cls: "" }, { label: "activity.colSession", key: "session", cls: "" }, { label: "activity.colProgram", key: "program", cls: "" },
  { label: "activity.colWorkspace", key: "workspace", cls: "" }, { label: "activity.colFolder", key: "folder", cls: "" },
  { label: "activity.colOut", key: "out", cls: "c-num" }, { label: "activity.colAge", key: "age", cls: "c-num" }, { label: "activity.colActions", key: null, cls: "" },
];
/** The chosen sort. It holds while the app runs and comes back to the default on restart. */
let sort: { key: SortKey; dir: 1 | -1 } | null = null;
/** A session's value under a head. null is a missing value (shown as a dash); it sorts last both ways. */
function sortValue(s: SessionInfo, key: SortKey): string | number | null {
  switch (key) {
    case "state": return stateRank(s);
    case "session": return taskTitle(s);
    case "program": return program(s) || null;
    case "workspace": return workspace(s).text;
    case "folder": return s.cwd || null;
    case "out": return s.output_bytes || null;
    case "age": return s.started_at ? Date.now() - s.started_at : null;
  }
}
/** Default order: out of the workspace first, then state, then start time. */
const byDefault = (a: SessionInfo, b: SessionInfo) => Number(workspace(b).out) - Number(workspace(a).out) || stateRank(a) - stateRank(b) || a.started_at - b.started_at || a.id.localeCompare(b.id);
function compare(a: SessionInfo, b: SessionInfo): number {
  if (!sort) return byDefault(a, b);
  const x = sortValue(a, sort.key), y = sortValue(b, sort.key);
  if (x === null || y === null) return x === y ? byDefault(a, b) : x === null ? 1 : -1;
  const c = typeof x === "number" && typeof y === "number" ? x - y : String(x).localeCompare(String(y), undefined, { sensitivity: "base", numeric: true });
  return c * sort.dir || byDefault(a, b);
}

export function daemonActivitySection(s: ActivityState, redraw: () => void, reveal: (id: string, closed: boolean) => void): HTMLElement[] {
  const head = h("div", "set-head"), title = h("div", "set-h", t("settings.tab.activity")); title.id = "set-h-activity";
  // The page shows the selected Host: "This computer (name)" or the remote Host's name.
  head.append(title, h("div", "set-sub mono", hostName()));
  const band = h("div", "set-band set-daemon-band");
  const m = s.metrics;
  const version = m?.version ?? s.version;
  // A value the daemon did not report shows a dash, never a zero.
  const up = m?.started_at ? age(m.started_at, true) : "—";
  // The selection count is a live status: a screen reader reads each change.
  const count = h("span", "set-daemon-picked"); count.setAttribute("role", "status");
  // Sink kills the selected sessions. It asks first.
  const sink = h("button", "set-rm set-sink"); sink.type = "button"; sink.dataset.key = "sink";
  sink.addEventListener("click", () => void sinkPicked());
  // A Host without daemon metrics, such as a remote Host, has no Up and no Memory.
  const metrics = hostCan("daemon_metrics");
  band.append(h("span", "", version ? `skiffd ${version}` : "skiffd"));
  if (metrics) band.append(h("span", "", t("activity.up", { time: up })));
  band.append(h("span", "", tn("palette.sessions", s.list.length)));
  if (metrics) band.append(h("span", "", t("activity.memory", { size: m?.memory_bytes ? bytes(m.memory_bytes) : "—" })));
  band.append(count, sink);
  const table = h("div", "set-daemon");
  // A grid of rows to select: click, Shift+click, Ctrl+click (⌘ on macOS), and the keys in onKey below.
  table.setAttribute("role", "grid");
  table.setAttribute("aria-multiselectable", "true");
  table.setAttribute("aria-labelledby", title.id);
  const cols = h("div", "set-row set-cols");
  cols.setAttribute("role", "row");
  for (const c of COLS) {
    // A head that sorts is a button: a click, Enter or Space sorts by it, and again reverses.
    const el = h(c.key ? "button" : "span", c.key ? `set-sort ${c.cls}` : c.cls, upper(t(c.label)));
    el.setAttribute("role", "columnheader");
    if (c.key) {
      const key = c.key; (el as HTMLButtonElement).type = "button"; el.dataset.key = key;
      const on = sort?.key === key;
      el.setAttribute("aria-sort", on ? (sort!.dir > 0 ? "ascending" : "descending") : "none");
      if (on) el.append(h("span", "set-sort-dir", sort!.dir > 0 ? "\u2191" : "\u2193"));
      el.addEventListener("click", () => { sort = { key, dir: on ? (sort!.dir > 0 ? -1 : 1) : 1 }; redraw(); });
    }
    cols.append(el);
  }
  table.append(cols);
  const rows = [...s.list].sort(compare), order = rows.map((x) => x.id), p = s.pick;
  const els = new Map<string, HTMLElement>();
  if (!rows.length) { const row = h("div", "set-row set-daemon-row"); row.setAttribute("role", "row"); const none = h("span", "c-span", t("activity.none")); none.setAttribute("role", "gridcell"); row.append(none); table.append(row); }
  for (const item of rows) {
    const w = workspace(item), row = h("div", `set-row set-daemon-row${w.out ? " out" : ""}`);
    row.setAttribute("role", "row"); row.dataset.id = item.id; els.set(item.id, row);
    // Click selects. Shift+click adds the range, Ctrl+click (⌘ on macOS) adds or drops the row. Reveal keeps its own click.
    row.addEventListener("mousedown", (e) => { if (e.shiftKey) e.preventDefault(); });
    row.addEventListener("click", (e) => {
      if ((e.target as HTMLElement).closest("button")) return;
      pickRow(p, order, item.id, { toggle: isMac ? e.metaKey : e.ctrlKey, range: e.shiftKey });
      paint(); row.focus();
    });
    const cell = (text: string, cls = "", title = text) => { const el = h("span", cls, text); el.title = title; el.setAttribute("role", "gridcell"); return el; };
    // The sidebar's state mark, in its state color.
    const state = h("span", "c-state"); state.setAttribute("role", "gridcell"); state.append(h("span", `dot ${item.state}`), h("span", "", t(`state.${item.state}`)));
    // The name alone, as the sidebar shows it. Hover shows the session id.
    row.append(state, cell(taskTitle(item), "c-name", `${taskTitle(item)} · ${item.id}`), cell(program(item)), cell(w.text, w.out ? "bad" : ""), cell(folder(item.cwd), "", item.cwd), cell(item.output_bytes ? bytes(item.output_bytes) : "—", "c-num"), cell(age(item.started_at), "c-num", item.started_at ? new Date(item.started_at).toLocaleString() : ""));
    const acts = h("span", "c-acts"); acts.setAttribute("role", "gridcell");
    const show = h("button", "set-rm", t("activity.reveal")); show.type = "button"; show.disabled = item.skipper != null && !item.coordinator && w.out; show.addEventListener("click", () => reveal(item.id, !!locate(S.closedProjects, item.cwd))); acts.append(show);
    row.append(acts); table.append(row);
  }

  /** Selection, the current row and the Sink button; no rebuild, so the keys stay on the row. */
  function paint() {
    const now = p.now !== null && els.has(p.now) ? p.now : order[0];
    for (const [id, row] of els) {
      const on = p.sel.has(id);
      row.classList.toggle("sel", on);
      row.setAttribute("aria-selected", String(on));
      row.tabIndex = id === now ? 0 : -1;
    }
    const n = picked(p, order).length;
    count.textContent = pickedText(n);
    sink.textContent = sinkLabel(n);
    // A Host without session lifecycle lists its sessions and sinks none.
    sink.disabled = !n || !hostCan("session_lifecycle");
  }
  paint();

  /**
   * The list keys on a row. Up/Down, Home/End: select that row; with Shift, the range;
   * with Ctrl (⌘ on macOS), move only. Space selects, Ctrl+Space adds or drops.
   * Ctrl+A (⌘A) selects all, anywhere in the grid. Esc clears the selection, else Settings closes.
   */
  table.addEventListener("keydown", (e) => {
    const mod = isMac ? e.metaKey : e.ctrlKey;
    const done = () => { e.preventDefault(); e.stopPropagation(); };
    if (mod && !e.shiftKey && !e.altKey && e.code === "KeyA" && order.length) {
      pickAll(p, order); paint(); return done();
    }
    if (e.key === "Escape" && p.sel.size) { p.sel.clear(); paint(); return done(); }
    const row = (e.target as HTMLElement).closest<HTMLElement>(".set-daemon-row");
    // Ctrl+Shift with an arrow is the app's list key: Settings steps its sections.
    if (e.target !== row || !row?.dataset.id || e.altKey || (mod && e.shiftKey)) return;
    const at = order.indexOf(row.dataset.id);
    const to = e.key === "ArrowDown" ? at + 1 : e.key === "ArrowUp" ? at - 1 : e.key === "Home" ? 0 : e.key === "End" ? order.length - 1 : null;
    if (to !== null) {
      if (to >= 0 && to < order.length) { moveTo(p, order, order[to], { range: e.shiftKey, keep: mod && !e.shiftKey }); paint(); els.get(order[to])?.focus(); }
      return done();
    }
    if (e.key === " ") { pickRow(p, order, row.dataset.id, { toggle: mod, range: e.shiftKey }); paint(); return done(); }
  });

  /** Asks with the Abandon Ship dialog, then kills the selected sessions. Cancel kills nothing. */
  async function sinkPicked() {
    const ids = picked(p, order);
    if (!ids.length) return;
    const names = ids.map((id) => taskTitle(rows.find((x) => x.id === id)!));
    const ok = await confirmAction(abandonShip(names));
    // A refresh can rebuild the list while the dialog is open: the keys go to the new copy.
    const back = (sel: string) => document.querySelector<HTMLElement>(sel)?.focus();
    if (!ok) return back(".set-daemon-band .set-sink");
    const results = await Promise.allSettled(ids.map((id) => hostInvoke("session_lifecycle", "kill_session", { session: id })));
    const failed = results.filter((r): r is PromiseRejectedResult => r.status === "rejected");
    p.sel.clear();
    await refreshActivity(s, redraw);
    if (failed.length) { s.error = String(failed[0].reason); redraw(); }
    back(`.set-daemon-row[tabindex="0"]`);
  }

  const out = [head, band, table]; if (s.error) out.push(h("div", "set-error", s.error)); return out;
}
