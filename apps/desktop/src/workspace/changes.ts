import { open, reveal, copy } from "../platform/fileActions";
import { sectionHeader } from "../ui/sectionHeader";
/**
 * The Changes section under each worktree, and the +/- counts on its row.
 * On a branch it lists every file changed since the branch left the default
 * branch, committed or not; on the default branch, the uncommitted files. It
 * shows their diffs in the peek, and opens files in the selected apps. A
 * remote Host's diffs are its git's text, in the page's own viewer; its files
 * are never opened here.
 */
import { invoke } from "@tauri-apps/api/core";
import type { Project, Worktree } from "../platform/types";
import { h } from "../ui/dom";
import { keyLabel } from "../app/keys";
import { icon } from "../ui/icons";
import { animateFold } from "../ui/fold";
import { showError } from "../ui/alerts";
import type { MenuEntry } from "../ui/menu";
import { ctxMenu } from "../ui/contextMenu";
import { branchName, isFolder } from "./model";
import { setRowActs } from "./rowActs";
import { select } from "../app/keyboard";
import { render, scheduleRender } from "../app/render";
import { collapsed } from "../app/state";

import { storedSet } from "../app/stored";
import { t } from "../i18n";
import { isRemote } from "../app/hosts";
import { hostCan, hostInvoke } from "../app/hostCaps";
import { dataEpoch } from "../app/dataPlane";
import type { Preview } from "./rowActs";

/** `committed`: the file is the same in the working tree and HEAD; the change sits in the branch's commits. */
type Change = { path: string; status: "M" | "A" | "D" | "U"; added: number | null; removed: number | null; committed?: boolean };
/** `base`: the default branch the files count from. Absent on the default branch itself: against HEAD. */
type Changes = { base?: string | null; changes: Change[] };

/** The project says it itself, from its Host's projects.toml. Changes show by default. A plain folder has no changes: it never reads git. */
export const showsChanges = (p: Project) => !isFolder(p) && p.changes !== false;

/** The Host keeps it (`project_config@1`); the next project read brings it back. */
export async function setShowChanges(p: Project, show: boolean) {
  try {
    await hostInvoke("project_config", "set_project_changes", { project: p.name, on: show });
    p.changes = show ? null : false;
    render();
  } catch (e) {
    showError(e);
  }
}

/** The last read of each worktree. The sidebar renders from here, never waiting. */
const lists = new Map<string, Changes>();
const loading = new Set<string>();
/** Worktrees whose Changes section is open. Closed by default. */
const shown = storedSet("skiff.changesOpen");

/** Counts the reads that brought a list different from the last one. */
let version = 0;

/** What the Changes section and the counts draw for `w`, as a string. Equal strings mean an equal section. */
export const changesSig = (w: Worktree) => `${shown.has(w.path) ? "open" : "closed"}:${version}`;

/** Keeps the list when the read brought the same one, so the sidebar does not rebuild. True on a change. */
function store(wt: string, list: Changes): boolean {
  const known = lists.get(wt);
  if (known && JSON.stringify(known) === JSON.stringify(list)) return false;
  lists.set(wt, list);
  version++;
  return true;
}

function load(wt: string) {
  if (loading.has(wt)) return;
  loading.add(wt);
  const at = dataEpoch();
  invoke<Changes>("git_changes", { path: wt })
    // Not a repository, or git is missing: the worktree shows no changes.
    .catch((): Changes => ({ changes: [] }))
    .then((list) => {
      loading.delete(wt);
      // Asked on a data connection that is gone since: ask the new one.
      if (at !== dataEpoch()) return load(wt);
      // Each turn that ends reads its worktrees again, mostly to the same list.
      if (store(wt, list)) scheduleRender("git");
    });
}

/** The diff a change row or Changes row shows: this Mac's diff command, or
 *  a remote Host's text in the viewer when that Host offers it. */
function diffPreview(w: Worktree, file?: string): Preview | undefined {
  const base = { wt: w.path, branch: branchName(w), base: lists.get(w.path)?.base ?? undefined, file, wait: file === undefined ? true : undefined };
  if (!isRemote()) return { kind: "diff", ...base };
  return hostCan("git_diff") ? { kind: "host-diff", ...base } : undefined;
}

/** Reads the known worktrees in `only` again, or every known worktree without it. */
export function reloadChanges(only?: Iterable<string>) {
  if (!only) {
    for (const wt of lists.keys()) load(wt);
    return;
  }
  for (const wt of only) if (lists.has(wt)) load(wt);
}

function listFor(w: Worktree): Changes | undefined {
  const list = lists.get(w.path);
  if (!list) load(w.path);
  return list;
}

/** `+12 −3` for the worktree row, or null when nothing changed. */
export function changeCounts(w: Worktree): HTMLElement | null {
  const list = listFor(w);
  if (!list?.changes.length) return null;
  let add = 0;
  let del = 0;
  for (const c of list.changes) {
    add += c.added ?? 0;
    del += c.removed ?? 0;
  }
  const el = h("span", "wt-diff");
  el.append(h("span", "diff-add", `+${add}`), h("span", "diff-del", `−${del}`));
  return el;
}

/** Whether the worktree has changes, as last read. */
export const hasChanges = (w: Worktree) => !!lists.get(w.path)?.changes.length;

const join = (wt: string, rel: string) => `${wt}/${rel}`;

function toggle(wt: string) {
  animateFold(`changes:${wt}`, !shown.has(wt), () => {
    if (shown.has(wt)) shown.delete(wt);
    else {
      shown.add(wt);
      load(wt);
    }
    render();
  });
}

/** Review all changes: the worktree's Changes row becomes current, and its diff takes the keys. */
export function reviewChanges(w: Worktree) {
  // The row must be in the list to be current.
  collapsed.delete(w.path);
  render();
  select(`changes:${w.path}`, true, true);
}

/** The section, or null when the worktree has no changes. */
export function changesBlock(w: Worktree): HTMLElement | null {
  const list = listFor(w);
  if (!list?.changes.length) return null;
  const isOpen = shown.has(w.path);
  const block = h("div", "files-block changes-block");
  const glyph = icon('<path d="M12 3v12"></path><path d="M6 9h12"></path><path d="M6 21h12"></path>');
  const key = `changes:${w.path}`;
  // A click selects the row on mousedown (keyboard.ts), then opens or closes it.
  // The header names what the files count from. On the default branch, that is HEAD.
  const head = sectionHeader(key, list.base ? t("changes.since", { base: list.base }) : t("menu.changes"), isOpen, glyph, () => toggle(w.path));
  const preview = diffPreview(w);
  setRowActs(key, {
    preview,
    fold: (open) => { if (open === undefined || open !== shown.has(w.path)) toggle(w.path); },
  });
  // Open, the rows show how many.
  if (!isOpen) head.appendChild(h("span", "changes-n", String(list.changes.length)));
  head.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    ctxMenu.open(e.clientX, e.clientY, t("menu.changes"), [
      ...(preview ? [{ icon: "code-git-branch", label: t("menu.reviewChanges"), run: () => reviewChanges(w) }] : []),
      { icon: "schedule-refresh-cw", label: t("menu.refresh"), run: () => load(w.path) },
    ]);
  });
  block.appendChild(head);
  if (isOpen) {
    const rows = h("div", "files-rows");
    rows.dataset.kids = key;
    for (const c of list.changes) rows.appendChild(row(w, c));
    block.appendChild(rows);
  }
  return block;
}

function row(w: Worktree, c: Change): HTMLElement {
  const wt = w.path;
  const path = join(wt, c.path);
  const key = `change:${path}`;
  const gone = c.status === "D";
  // The row's diff shows while it is current. Shift+Enter opens the file itself.
  // A remote Host's file is that Host's: its diff shows from its git, its path
  // is copied, and it is never opened here.
  const remote = isRemote();
  const preview = diffPreview(w, c.path);
  setRowActs(key, { preview, open: gone || remote ? undefined : () => open(path) });
  const show = () => select(key, true, true);
  const slash = c.path.lastIndexOf("/");
  const name = c.path.slice(slash + 1);
  const dir = slash < 0 ? "" : c.path.slice(0, slash);
  const r = h("button", "file-row change-row" + (gone ? " st-gone" : "") );
  r.type = "button";
  r.dataset.key = key;
  // The mousedown selects the row, and its diff shows (keyboard.ts). Enter
  // gives the diff the keys; Shift+Enter opens the file (keyboard.ts).
  // A colored letter is an uncommitted edit. A committed change settles to grey, in place.
  const st = h("span", `change-st st-${c.status}${c.committed ? " st-done" : ""}`, c.status);
  const counts = h("span", "change-counts");
  if (c.added === null) counts.textContent = "bin";
  else {
    if (c.added) counts.appendChild(h("span", "diff-add", `+${c.added}`));
    if (c.removed) counts.appendChild(h("span", "diff-del", `−${c.removed}`));
  }
  r.append(st, h("span", "file-name", name), h("span", "change-dir", dir), counts);
  r.addEventListener("contextmenu", (m) => {
    m.preventDefault();
    m.stopPropagation();
    const items: MenuEntry[] = preview ? [
      { icon: "code-git-branch", label: t("menu.showDiff"), hint: "Enter", run: show },
    ] : [];
    if (!gone && !remote) items.push(
      { icon: "indicators-square-arrow-out-up-right", label: t("menu.openFile"), hint: "Shift+Enter", run: () => open(path) },
      { icon: "documents-folder-open", label: t("menu.showInFileManager"), hint: keyLabel("project-folder"), run: () => reveal(path) },
    );
    items.push({ icon: "code-copy", label: t("menu.copyPath"), hint: keyLabel("project-copy-path"), run: () => copy(path) });
    ctxMenu.open(m.clientX, m.clientY, name, items);
  });
  return r;
}
