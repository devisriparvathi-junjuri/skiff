import { invoke } from "@tauri-apps/api/core";
import { h } from "../ui/dom";
import { actionDialog } from "../ui/dialogParts";
import { rune } from "../ui/runes";
import { size } from "./skiffFolderDialog";
import { canFlush, expected, find, groups, idleReason, plan, rowState, visibleRows, type DiskNode, type Expected } from "./diskTree";
import { locale, type PluralKey, sentences, t, tn, upper } from "../i18n";

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

/** `disk_open`'s answer: what `open_file` says. */
type Opened = { kind: "run"; script: string; cwd: string } | { kind: "default" } | { kind: "no_app"; message: string };

/**
 * How the rows open and show what they name. `local`: false while a remote
 * Host is shown, where this computer's openers are refused. `peek` runs an
 * Open with command in the peek: Settings closes for it.
 */
export interface DiskOpen {
  local: boolean;
  peek: (script: string, cwd: string, name: string) => void;
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

/** A plural line with its count written as the locale writes numbers: "1,204 files". */
const count = (key: PluralKey, n: number) => tn(key, n, { count: n.toLocaleString(locale()) });

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
  const lines = [t("disk.removed", { files: count("disk.files", out.files), size: size(out.bytes) })];
  if (out.kept) lines.push(t("disk.kept", { items: count("disk.items", out.kept) }));
  if (out.rows) lines.push(count("disk.rowsFlushed", out.rows));
  return sentences(lines);
}

/** One REMOVE or KEEP section of the dialog. */
function section(title: string, nodes: DiskNode[], scope: DiskNode): HTMLElement[] {
  const gs = groups(nodes, scope);
  if (!gs.length) return [];
  const files = gs.reduce((a, g) => a + g.files, 0);
  const bytes = gs.reduce((a, g) => a + g.bytes, 0);
  const band = h("div", "disk-band");
  band.append(h("span", "disk-band-title", title), h("span", "disk-band-count", `${count("disk.files", files)} · ${size(bytes)}`));
  return [
    band,
    ...gs.map((g) => {
      const line = h("div", "disk-line");
      const words = h("span", "disk-line-words");
      const more = g.names.length > 3 ? ` · ${t("disk.more", { count: g.names.length - 3 })}` : "";
      words.append(h("span", "disk-line-why", g.name), h("span", "disk-line-names mono", g.names.slice(0, 3).join(" · ") + more));
      const n = h("span", "disk-line-size");
      n.append(h("span", "", size(g.bytes)), h("span", "disk-line-files", count("disk.files", g.files)));
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
  const scope = h("div", "disk-scope mono", t("disk.in", { folder: `${s.usage?.shown ?? "~/skiff-agent"}/${n.path ? n.path + "/" : ""}` }));
  const list = h("div", "disk-list");
  list.append(...section(t("disk.sectionRemove"), go, n), ...section(t("disk.sectionKeep"), keep, n));
  const limit = h("div", "disk-limit", t("disk.limit"));
  const back = () => document.querySelector<HTMLElement>(`.set-disk-row[data-path="${CSS.escape(n.path)}"]`)?.focus();
  actionDialog({
    title: t("disk.removeTitle", { files: count("disk.files", files), size: size(bytes) }),
    content: [scope, list, limit],
    action: t("disk.remove"),
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

/**
 * A file row's click: `disk_open` checks that the file is in the agent
 * folder, then opens it as Settings › Open with says. It changes nothing.
 */
async function openFile(s: DiskState, n: DiskNode, redraw: () => void, o: DiskOpen) {
  try {
    const out = await invoke<Opened>("disk_open", { path: n.path });
    if (out.kind === "run") return o.peek(out.script, out.cwd, n.name);
    if (out.kind === "default") return;
    s.error = out.message;
  } catch (e) {
    s.error = String(e);
  }
  redraw();
}

/** A folder row's Show in file manager: `disk_reveal` checks that the folder is in the agent folder. */
function revealFolder(s: DiskState, n: DiskNode, redraw: () => void) {
  invoke("disk_reveal", { path: n.path }).catch((e) => {
    s.error = String(e);
    redraw();
  });
}

const CHEVRON = '<svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.5"/></svg>';

/**
 * Settings › Disk use: the Skiff agent's folder as a tree. SIZE, CAN FLUSH,
 * STATE and Flush… on each row; CAN FLUSH shows the size, so the button has
 * none. The rule is in `skiff_client::disk`.
 * A click on a file row opens the file. A folder row has Show in file
 * manager. `setDays` saves `[skiff_agent] cache_days`.
 */
export function diskUseSection(s: DiskState, redraw: () => void, setDays: (days: number) => void, opener: DiskOpen): HTMLElement[] {
  const u = s.usage;
  const head = h("div", "set-head");
  const title = h("div", "set-h", t("settings.tab.disk"));
  title.id = "set-h-disk";
  head.append(title, h("div", "set-sub mono", u?.shown ?? ""));

  const table = h("div", "set-disk");
  table.setAttribute("role", "treegrid");
  table.setAttribute("aria-labelledby", title.id);
  const cols = h("div", "set-row set-cols");
  cols.setAttribute("role", "row");
  cols.append(h("span", "", upper(t("disk.colName"))), h("span", "c-num", upper(t("disk.colSize"))), h("span", "c-num", upper(t("disk.colCanFlush"))), h("span", "", upper(t("disk.colState"))), h("span"));
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
        chev.setAttribute("aria-label", t(s.open.has(n.path) ? "disk.fold" : "menu.open"));
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
        const b = h("button", "set-rm", t("disk.flushButton"));
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
      if (n.dir && opener.local) {
        const b = h("button", "set-rm set-ico");
        b.type = "button";
        b.dataset.key = `reveal:${n.path}`;
        b.title = t("menu.showInFileManager");
        b.setAttribute("aria-label", t("menu.showInFileManager"));
        b.appendChild(rune("indicators-square-arrow-out-up-right"));
        b.addEventListener("click", (e) => {
          e.stopPropagation();
          const moved = s.cur !== n.path;
          s.cur = n.path;
          revealFolder(s, n, redraw);
          if (moved) redraw();
        });
        acts.append(b);
      }
      r.append(name, h("span", "c-num", size(n.bytes)), h("span", "c-num" + (flush ? " disk-can" : " disk-zero"), flush ? size(flush) : "—"), state, acts);
      r.addEventListener("click", () => {
        const moved = s.cur !== n.path;
        s.cur = n.path;
        if (!n.dir && opener.local) void openFile(s, n, redraw, opener);
        if (moved) redraw();
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

  const status = h("div", "set-hint", s.removed ?? (u && !s.counting && canFlush(u.tree) === 0 ? t("disk.nothing") : ""));
  status.setAttribute("role", "status");
  if (s.removed) status.dataset.kind = "added";
  const errors = [
    s.error,
    u?.ledger_error ? t("disk.ledgerError", { error: u.ledger_error }) : null,
    u && !u.daemon ? t("disk.noDaemon") : null,
    u?.unreadable ? t("disk.unreadable", { files: count("disk.files", u.unreadable) }) : null,
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
  sw.setAttribute("aria-label", t(on ? "disk.autoOn" : "disk.autoOff"));
  sw.appendChild(h("span", "switch"));
  sw.addEventListener("click", () => setDays(on ? 0 : 30));
  // Flush now: the whole folder's stale files, through the same dialog as a row's Flush….
  const now = h("span", "c-acts");
  const all = h("button", "set-rm", t("disk.flushNow"));
  all.type = "button";
  all.dataset.key = "flush-now";
  all.disabled = !u || s.counting || canFlush(u.tree) === 0;
  all.addEventListener("click", () => {
    if (!u) return;
    s.cur = "";
    askFlush(s, u.tree, redraw);
  });
  now.append(all);
  auto.append(h("span", "c-name", t("disk.auto")), h("span", "c-cmd", on ? count("disk.days", days) : ""), sw, now);

  // The status, the errors and the flush controls stay at the bottom as the tree scrolls.
  const foot = h("div", "set-disk-foot");
  foot.append(status, ...errors, auto);
  return [head, table, foot];
}
