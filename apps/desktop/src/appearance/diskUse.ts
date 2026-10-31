import { invoke } from "@tauri-apps/api/core";
import { h } from "../ui/dom";
import { actionDialog } from "../ui/dialogParts";
import { size } from "./skiffFolderDialog";
import { canFlush, expected, find, groups, idleReason, plan, rowState, visibleRows, type DiskNode, type Expected } from "./diskTree";

/** `disk_usage`: the Skiff agent's folder as a tree, each file with the rule's verdict. */
export interface DiskUsage {
  /** The folder with `~` for home. */
  shown: string;
  tree: DiskNode;
  /** When it was counted, in Unix seconds. */
  now: number;
  /** The daemon answered: the panes were checked. */
  daemon: boolean;
  ledger_error: string | null;
  unreadable: number;
  /** Auto flush's age in days. 0: off. */
  cache_days: number;
}

/** `flush_disk`'s answer. */
interface Flushed {
  files: number;
  bytes: number;
  kept: number;
  rows: number;
  errors: string[];
}

/** The page's state. It survives every redraw of Settings. */
export interface DiskState {
  usage: DiskUsage | null;
  counting: boolean;
  /** "Removed N files, X." after a flush. */
  removed: string | null;
  error: string | null;
  /** The open folders, by path. The agent folder and its top level open first. */
  open: Set<string> | null;
  /** The current row's path. */
  cur: string;
}

export const diskState = (): DiskState => ({ usage: null, counting: false, removed: null, error: null, open: null, cur: "" });

const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;

/** Counts the folder. The page shows "…" until the answer comes. */
export async function countDisk(s: DiskState, redraw: () => void) {
  s.counting = true;
  redraw();
  try {
    s.usage = await invoke<DiskUsage>("disk_usage");
    s.error = null;
  } catch (e) {
    s.error = String(e);
  } finally {
    s.counting = false;
    redraw();
  }
}

/** The status line after a flush. */
export function flushedLine(out: Flushed): string {
  let line = `Removed ${plural(out.files, "file")}, ${size(out.bytes)}.`;
  if (out.kept) line += ` Kept ${plural(out.kept, "item")} that changed since the count.`;
  if (out.rows) line += ` ${plural(out.rows, "ledger row")} now ${out.rows === 1 ? "shows" : "show"} “Result flushed”.`;
  return line;
}

/** One REMOVE or KEEP section of the dialog. */
function section(title: string, nodes: DiskNode[], scope: DiskNode): HTMLElement[] {
  const gs = groups(nodes, scope);
  if (!gs.length) return [];
  const files = gs.reduce((a, g) => a + g.files, 0);
  const bytes = gs.reduce((a, g) => a + g.bytes, 0);
  const band = h("div", "disk-band");
  band.append(h("span", "disk-band-title", title), h("span", "disk-band-count", `${plural(files, "file")} · ${size(bytes)}`));
  return [
    band,
    ...gs.map((g) => {
      const line = h("div", "disk-line");
      const words = h("span", "disk-line-words");
      const more = g.names.length > 3 ? ` · ${g.names.length - 3} more` : "";
      words.append(h("span", "disk-line-why", g.name), h("span", "disk-line-names mono", g.names.slice(0, 3).join(" · ") + more));
      const n = h("span", "disk-line-size");
      n.append(h("span", "", size(g.bytes)), h("span", "disk-line-files", plural(g.files, "file")));
      line.append(h("i", `disk-mk ${g.kind}`), words, n);
      return line;
    }),
  ];
}

/**
 * Flush on a row: the dialog lists what goes and what stays, by reason.
 * On Remove, `flush_disk` counts again and removes only what is still stale
 * and unchanged. Esc or Cancel gives the keys back to the row.
 */
function askFlush(s: DiskState, n: DiskNode, redraw: () => void) {
  const { go, keep } = plan(n);
  const files = go.reduce((a, x) => a + x.files, 0);
  const bytes = go.reduce((a, x) => a + x.bytes, 0);
  const items: Expected[] = go.map(expected);
  const scope = h("div", "disk-scope mono", `In ${s.usage?.shown ?? "~/skiff-agent"}/${n.path ? n.path + "/" : ""}`);
  const list = h("div", "disk-list");
  list.append(...section("Remove", go, n), ...section("Keep", keep, n));
  const limit = h("div", "disk-limit", "A job that writes here again later can't be predicted.");
  const back = () => document.querySelector<HTMLElement>(`.set-disk-row[data-path="${CSS.escape(n.path)}"]`)?.focus();
  actionDialog({
    title: `Remove ${plural(files, "file")} (${size(bytes)})?`,
    content: [scope, list, limit],
    action: "Remove",
    tone: "danger",
    submit: async () => {
      const out = await invoke<Flushed>("flush_disk", { items });
      s.removed = flushedLine(out);
      s.error = out.errors.length ? out.errors.join("\n") : null;
      void countDisk(s, redraw);
    },
    onClose: back,
  });
}

const CHEVRON = '<svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.5"/></svg>';

/**
 * Settings › Disk use: the Skiff agent's folder as a tree. SIZE, CAN FLUSH,
 * STATE and Flush on each row; the rule is in `skiff_client::disk`.
 * `setDays` saves `[skiff_agent] cache_days`.
 */
export function diskUseSection(s: DiskState, redraw: () => void, setDays: (days: number) => void): HTMLElement[] {
  const u = s.usage;
  const head = h("div", "set-head");
  const title = h("div", "set-h", "Disk use");
  title.id = "set-h-disk";
  head.append(title, h("div", "set-sub mono", u?.shown ?? ""));

  const table = h("div", "set-disk");
  table.setAttribute("role", "treegrid");
  table.setAttribute("aria-labelledby", title.id);
  const cols = h("div", "set-row set-cols");
  cols.setAttribute("role", "row");
  cols.append(h("span", "", "NAME"), h("span", "c-num", "SIZE"), h("span", "c-num", "CAN FLUSH"), h("span", "", "STATE"), h("span"));
  table.append(cols);

  if (!u) {
    const r = h("div", "set-row set-agent set-disk-row");
    r.append(h("span", "c-name", s.counting ? "…" : ""), h("span", "c-num", "…"), h("span", "c-num", "…"), h("span"), h("span"));
    table.append(r);
  } else {
    const tree = { ...u.tree, name: `${u.shown.split("/").pop() || u.shown}/` };
    if (!s.open) s.open = new Set(["", ...(tree.children ?? []).filter((c) => c.path === "results" || c.path === "projects").map((c) => c.path)]);
    if (!find(tree, s.cur)) s.cur = "";
    const rows = visibleRows(tree, s.open);
    const toggle = (n: DiskNode) => {
      if (!n.children?.length) return;
      if (s.open!.has(n.path)) s.open!.delete(n.path);
      else s.open!.add(n.path);
      s.cur = n.path;
      redraw();
    };
    for (const { node: n, depth } of rows) {
      const cur = n.path === s.cur;
      const r = h("div", "set-row set-agent on set-disk-row" + (cur ? " cur" : "") + (n.path === "" ? " root" : ""));
      r.setAttribute("role", "row");
      r.setAttribute("aria-level", String(depth + 1));
      r.setAttribute("aria-selected", String(cur));
      if (n.children?.length) r.setAttribute("aria-expanded", String(s.open.has(n.path)));
      r.dataset.path = n.path;
      r.tabIndex = cur ? 0 : -1;
      const name = h("span", "c-name");
      name.style.paddingLeft = `calc(${12 + depth * 16} * var(--u))`;
      if (n.children?.length) {
        const chev = h("button", "disk-chev" + (s.open.has(n.path) ? " open" : ""));
        chev.type = "button";
        chev.tabIndex = -1;
        chev.innerHTML = CHEVRON;
        chev.setAttribute("aria-label", s.open.has(n.path) ? "Close" : "Open");
        chev.addEventListener("click", (e) => {
          e.stopPropagation();
          toggle(n);
        });
        name.append(chev);
      } else name.append(h("span", "disk-leaf"));
      name.append(h("span", "disk-name mono", n.name));
      const st = rowState(n, u.now);
      const state = h("span", "c-state");
      state.append(h("i", `disk-mk ${st.kind}`), h("span", "disk-state", st.text), h("span", "dim", st.label));
      const flush = canFlush(n);
      const acts = h("span", "c-acts");
      if (flush > 0) {
        const b = h("button", "set-rm", `Flush ${size(flush)}…`);
        b.type = "button";
        b.dataset.key = `flush:${n.path}`;
        b.disabled = s.counting;
        b.addEventListener("click", (e) => {
          e.stopPropagation();
          s.cur = n.path;
          askFlush(s, n, redraw);
        });
        acts.append(b);
      } else acts.append(h("span", "disk-why", idleReason(n, u.now)));
      r.append(name, h("span", "c-num", size(n.bytes)), h("span", "c-num" + (flush ? " disk-can" : " disk-zero"), flush ? size(flush) : "—"), state, acts);
      r.addEventListener("click", () => {
        if (s.cur === n.path) return;
        s.cur = n.path;
        redraw();
      });
      r.addEventListener("dblclick", () => toggle(n));
      table.append(r);
    }
    // Up and Down move the current row. Right opens, Left closes or goes up.
    // Space opens or closes. Enter opens the flush dialog.
    table.addEventListener("keydown", (e: KeyboardEvent) => {
      const i = Math.max(0, rows.findIndex((x) => x.node.path === s.cur));
      const n = rows[i].node;
      const go = (to: string) => {
        e.preventDefault();
        s.cur = to;
        redraw();
      };
      if (e.key === "ArrowDown") go(rows[Math.min(rows.length - 1, i + 1)].node.path);
      else if (e.key === "ArrowUp") go(rows[Math.max(0, i - 1)].node.path);
      else if (e.key === "ArrowRight" && n.children?.length && !s.open!.has(n.path)) {
        e.preventDefault();
        toggle(n);
      } else if (e.key === "ArrowLeft") {
        if (s.open!.has(n.path) && n.children?.length) {
          e.preventDefault();
          toggle(n);
        } else if (n.path) go(n.path.includes("/") ? n.path.slice(0, n.path.lastIndexOf("/")) : "");
      } else if (e.key === " " && n.children?.length) {
        e.preventDefault();
        toggle(n);
      } else if (e.key === "Enter" && canFlush(n) > 0 && !s.counting) {
        e.preventDefault();
        askFlush(s, n, redraw);
      }
    });
  }

  const status = h("div", "set-hint", s.removed ?? (u && !s.counting && canFlush(u.tree) === 0 ? "Nothing to flush." : ""));
  status.setAttribute("role", "status");
  if (s.removed) status.dataset.kind = "added";
  const errors = [
    s.error,
    u?.ledger_error ? `Skiff cannot read the ledger: ${u.ledger_error}` : null,
    u && !u.daemon ? "Skiff cannot reach skiffd. A flush needs the session list." : null,
    u?.unreadable ? `Skiff could not read ${plural(u.unreadable, "file")}.` : null,
  ]
    .filter((e): e is string => !!e)
    .map((e) => {
      const el = h("div", "set-error", e);
      el.setAttribute("role", "alert");
      return el;
    });

  const days = u?.cache_days ?? 0;
  const on = days > 0;
  const auto = h("div", "set-row set-open set-agent set-disk-auto" + (on ? " on" : ""));
  const sw = h("button", "c-mode set-switch");
  sw.type = "button";
  sw.disabled = !u;
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-checked", String(on));
  sw.setAttribute("aria-label", `Auto flush ${on ? "on" : "off"}`);
  sw.appendChild(h("span", "switch"));
  sw.addEventListener("click", () => setDays(on ? 0 : 30));
  auto.append(h("span", "c-name", "Auto flush"), h("span", "c-cmd", on ? plural(days, "day") : ""), sw);

  return [head, table, status, ...errors, auto];
}
