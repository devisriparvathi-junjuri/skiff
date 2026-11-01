import { open, reveal, copy } from "../platform/fileActions";
import { setRowActs, type Preview } from "./rowActs";
import { sectionHeader } from "../ui/sectionHeader";
/**
 * The Files section under each worktree. `files` in projects.toml turns it on
 * or off; without the key, a plain folder shows it and a git project does
 * not. The Host reads the key and sends the result as `files`.
 * Every folder comes from the selected Host (`files@1`), This Mac and a
 * remote Host alike: bounded, links listed and never followed. On This Mac
 * selection previews files with their Open with setting, on the Host's own
 * paths. On a remote Host the peek shows the Host's preview, and Open makes
 * a local copy: its paths never reach this Mac's openers. A folder is read
 * again when it opens, on Refresh, and when the data plane comes back.
 */
import type { HostEntry, HostListing, Project, Worktree } from "../platform/types";
import { h } from "../ui/dom";
import { keyLabel } from "../app/keys";
import { chevron, icon } from "../ui/icons";
import { animateFold } from "../ui/fold";
import { showError } from "../ui/alerts";
import { ctxMenu } from "../ui/contextMenu";
import { render } from "../app/render";
import { storedSet } from "../app/stored";
import { t, tn } from "../i18n";
import { hostCan, hostInvoke } from "../app/hostCaps";
import { isRemote } from "../app/hosts";
import { dataEpoch } from "../app/dataPlane";
import { openHostCopy } from "../terminal/peek";

type Entry = HostEntry;
type Listing = HostListing | { error: string };

/** Projects whose Host says they show files. */
const on = new Set<string>();
/** The last listing of each folder read. The sidebar renders from here, never waiting. */
const listings = new Map<string, Listing>();
/** Worktrees whose Files section is open. Closed by default. */
const shown = storedSet("skiff.filesOpen");
/** Open folders, by path. */
const expanded = storedSet("skiff.foldersOpen");
/** Folders being read. */
const reading = new Set<string>();
/** The worktree each folder read belongs to: the Host lists by worktree and relative folder. */
const roots = new Map<string, string>();

/** Counts the reads that brought a listing different from the last one, and the folders opened or closed. */
let version = 0;

const join = (dir: string, name: string) => `${dir}/${name}`;

/** The Host's setting, and a Host that lists its files. */
export const showsFiles = (p: Project) => on.has(p.name) && hostCan("files");

/** `path` relative to the worktree `root`: empty for the root itself. */
const relative = (root: string, path: string) => (path === root ? "" : path.slice(root.length + 1));

/** What the Files section draws for `w`, as a string. Equal strings mean an equal section. */
export const filesSig = (w: Worktree) => (shown.has(w.path) ? `open:${version}` : "closed");

/** Keeps the listing when the read brought the same one, so the sidebar does not rebuild. */
function store(dir: string, l: Listing) {
  const known = listings.get(dir);
  if (known && JSON.stringify(known) === JSON.stringify(l)) return;
  listings.set(dir, l);
  version++;
}

/** Which projects show files, from the Host's rows. Called with every project read. */
export function filesFrom(list: Project[]) {
  on.clear();
  for (const p of list) if (p.files) on.add(p.name);
}

/** The Host keeps the setting (`project_config@1`). */
export async function setShowFiles(p: Project, show: boolean) {
  try {
    await hostInvoke("project_config", "set_project_files", { project: p.name, on: show });
    if (show) on.add(p.name);
    else on.delete(p.name);
    render();
  } catch (e) {
    showError(e);
  }
}

function load(root: string, dir: string) {
  if (reading.has(dir)) return;
  reading.add(dir);
  roots.set(dir, root);
  const at = dataEpoch();
  hostInvoke<HostListing>("files", "list_files", { worktree: root, dir: relative(root, dir) })
    .then((l) => {
      if (at === dataEpoch()) store(dir, l);
    })
    .catch((e) => {
      if (at !== dataEpoch()) return;
      const msg = String(e);
      // Deleted since its parent was read: forget it and read the parent
      // again, so its row goes. No note.
      const parent = dir.slice(0, dir.lastIndexOf("/"));
      if (/is not a folder in this worktree/.test(msg) && dir !== root && listings.has(parent)) {
        for (const d of [...listings.keys()]) if (d === dir || d.startsWith(dir + "/")) listings.delete(d);
        for (const d of [...expanded]) if (d === dir || d.startsWith(dir + "/")) expanded.delete(d);
        version++;
        load(root, parent);
      } else store(dir, { error: msg });
    })
    .finally(() => {
      reading.delete(dir);
      // Asked on a data connection that is gone since: ask the new one.
      if (at !== dataEpoch()) load(root, dir);
      render("files");
    });
}

/** The data plane is back, or another: every folder read is read again from it. */
export function reloadFiles() {
  for (const [dir, root] of roots) if (listings.has(dir)) load(root, dir);
}

/** Reads again `dir` of the worktree `root`, and every open folder under it. */
function refresh(root: string, dir: string) {
  load(root, dir);
  for (const d of expanded) if (d.startsWith(dir + "/")) load(root, d);
}

/** Opening reads again, so the tree is as fresh as the last click. */
function toggle(root: string, dir: string, set: Set<string>) {
  animateFold(`${set === shown ? "files" : "file"}:${dir}`, !set.has(dir), () => {
    if (set.has(dir)) set.delete(dir);
    else {
      set.add(dir);
      refresh(root, dir);
    }
    version++;
    render();
  });
}

export function filesBlock(w: Worktree): HTMLElement {
  const isOpen = shown.has(w.path);
  const block = h("div", "files-block");
  const glyph = icon('<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"></path><path d="M14 3v5h5"></path>');
  const key = `files:${w.path}`;
  // A click selects the row on mousedown (keyboard.ts), then opens or closes it.
  const head = sectionHeader(key, t("menu.files"), isOpen, glyph, () => toggle(w.path, w.path, shown));
  setRowActs(key, { preview: folderPreview(w.path), fold: folds(w.path, w.path, shown) });
  head.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    ctxMenu.open(e.clientX, e.clientY, t("menu.files"), [
      { icon: "schedule-refresh-cw", label: t("menu.refresh"), run: () => refresh(w.path, w.path) },
      !isRemote() && { icon: "indicators-square-arrow-out-up-right", label: t("menu.showInFileManager"), run: () => open(w.path) },
    ].filter((x) => !!x));
  });
  block.appendChild(head);
  if (isOpen) {
    const rows = h("div", "files-rows");
    rows.dataset.kids = key;
    folder(rows, w.path, w.path, 0);
    block.appendChild(rows);
  }
  return block;
}

function folder(into: HTMLElement, root: string, dir: string, depth: number) {
  const l = listings.get(dir);
  if (!l) {
    // Open from a past run: read it the first time it shows.
    load(root, dir);
    into.appendChild(note("…", depth));
  }
  else if ("error" in l) into.appendChild(note(l.error, depth, true));
  else if (!l.entries.length && !l.truncated) into.appendChild(note(t("files.empty"), depth));
  else {
    for (const e of l.entries) {
      const path = join(dir, e.name);
      into.appendChild(row(e, root, path, depth));
      if (e.kind === "dir" && expanded.has(path)) {
        // The open folder's rows sit in one box, so they can grow and fold together.
        const sub = h("div", "files-rows");
        sub.dataset.kids = `file:${path}`;
        folder(sub, root, path, depth + 1);
        into.appendChild(sub);
      }
    }
    // Cut: never "Empty", and the folder keeps its fold.
    if (l.truncated) into.appendChild(note(l.entries.length ? tn("files.truncated", l.entries.length) : t("files.tooMany"), depth));
  }
}

/** A folder row's card: on a remote Host, without a file manager to show it in. */
const folderPreview = (path: string): Preview => ({ kind: "folder", path, local: !isRemote() });

/**
 * What a row that is not a folder shows. This Mac: a regular file its Open
 * with setting on the Host's own path, as before. A link, FIFO, device or
 * socket only in the Host's bounded viewer, never followed or opened. A
 * remote Host: its preview from the Host, and Open makes a local copy of a
 * regular file only.
 */
function filePreview(e: Entry, root: string, path: string): { preview: Preview; open?: () => void } {
  const file = relative(root, path);
  if (!isRemote() && e.kind === "file") return { preview: { kind: "file", path }, open: () => open(path) };
  const copy = isRemote() && e.kind === "file";
  return { preview: { kind: "host-file", wt: root, file, path, copy }, open: copy ? () => openHostCopy(root, file) : undefined };
}

function row(e: Entry, root: string, path: string, depth: number): HTMLElement {
  // A folder read as empty has nothing to open: no chevron, no fold. One cut
  // short has entries, shown or not.
  const dir = e.kind === "dir";
  const l = dir ? listings.get(path) : undefined;
  const foldable = dir && !(l && !("error" in l) && !l.entries.length && !l.truncated);
  const cls = ["file-row"];
  if (dir) cls.push("dir");
  if (e.name.startsWith(".")) cls.push("hidden-file");
  const r = h("button", cls.join(" "));
  r.type = "button";
  r.dataset.key = `file:${path}`;
  if (foldable) r.dataset.fold = r.dataset.key;
  const acts = dir ? undefined : filePreview(e, root, path);
  setRowActs(r.dataset.key, dir
    ? { preview: folderPreview(path), fold: foldable ? folds(root, path, expanded) : undefined }
    : acts!);
  r.style.setProperty("--depth", String(depth));
  if (foldable) r.setAttribute("aria-expanded", String(expanded.has(path)));
  // The mousedown selected the row and its preview (keyboard.ts). A click on a
  // folder opens or closes it; a double-click on a file opens it.
  r.addEventListener("click", (m) => {
    // detail counts clicks across the re-render the first click causes.
    if (foldable) toggle(root, path, expanded);
    else if (m.detail >= 2) acts?.open?.();
  });
  const lead = h("span", "file-lead");
  if (foldable) lead.appendChild(chevron(expanded.has(path)));
  r.append(lead, h("span", "file-name", e.kind === "link" ? `${e.name} →` : e.name));
  const local = !isRemote();
  r.addEventListener("contextmenu", (m) => {
    m.preventDefault();
    m.stopPropagation();
    const items = dir
      ? [
          { icon: "schedule-refresh-cw", label: t("menu.refresh"), run: () => refresh(root, path) },
          local && { icon: "indicators-square-arrow-out-up-right", label: t("menu.showInFileManager"), hint: keyLabel("project-folder"), run: () => open(path) },
          { icon: "code-copy", label: t("menu.copyPath"), hint: keyLabel("project-copy-path"), run: () => copy(path) },
        ]
      : [
          acts?.open && { icon: "indicators-square-arrow-out-up-right", label: t(local ? "menu.open" : "menu.openCopy"), hint: "Enter", run: acts.open },
          local && { icon: "documents-folder-open", label: t("menu.showInFileManager"), hint: keyLabel("project-folder"), run: () => reveal(path) },
          { icon: "code-copy", label: t("menu.copyPath"), hint: keyLabel("project-copy-path"), run: () => copy(path) },
        ];
    ctxMenu.open(m.clientX, m.clientY, e.name, items.filter((x) => !!x));
  });
  return r;
}

/** Space, Left and Right on a folder row: `open` sets it, no argument flips it. */
const folds = (root: string, path: string, set: Set<string>) => (open?: boolean) => {
  if (open === undefined || open !== set.has(path)) toggle(root, path, set);
};

function note(text: string, depth: number, error = false): HTMLElement {
  const n = h("div", "file-note" + (error ? " error" : ""), text);
  n.style.setProperty("--depth", String(depth));
  return n;
}
