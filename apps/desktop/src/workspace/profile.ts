/**
 * Workspaces (#154): a `.skiffw` file names some projects and fleets. While
 * one is open, the tree shows only those (a fleet brings the projects in it
 * now), and the Ledger shows only their work and the work of no project. A
 * workspace changes what shows, never a project, a fleet, a session or the
 * ledger. With none open, the tree shows All projects.
 *
 * One document at a time: its path (null: Untitled), the profile in memory,
 * the profile last read or saved (the baseline), and the file's revision on
 * disk. Edited: the profile differs from the baseline. Name, inclusions,
 * repairs, and the user's folds and project picks are edits. Automatic opens
 * and picks are not.
 *
 * Every way out of an edited workspace (New, Open, a recent, Close, a file
 * from the OS, Quit, the window's close) asks Save / Don't Save / Cancel.
 * Saves are atomic and check the revision first. An outside change to the
 * file shows a notice and never reloads by itself.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";

import { render } from "../app/renderRequest";
import { S } from "../app/state";
import type { Project } from "../platform/types";
import { showError } from "../ui/alerts";
import { ctxMenu } from "../ui/contextMenu";
import { dialogFrame } from "../ui/dialogParts";
import { button, h, setHidden } from "../ui/dom";
import type { MenuEntry } from "../ui/menu";
import { treeSections, useTreeView } from "./tree";
import { selectProject } from "./view";
import { t, tn, upper } from "../i18n";
import { createMembership } from "./profile/membership";
import { canon, createWorkspaceState, emptyProfile, trimPath } from "./profile/state";
import type { Doc, Profile } from "./profile/state";
import { fileName, forget, forgetLast, last as lastPath, read, recents, remember, revision, write as writeFile } from "./profile/storage";
import { choose, trapTab } from "./profile/dialogs";
export type { Profile } from "./profile/state";

const EXT = "skiffw";

const state = createWorkspaceState();
const membership = createMembership(state, { announce, changed, showView });
const clone = (p: Profile): Profile => JSON.parse(JSON.stringify(p));
/** A transition or save runs. A second request is dropped, so one prompt and one write at a time. */
let busy = false;
export const workspaceOpen = state.workspaceOpen;
export const workspaceName = state.workspaceName;
export const workspaceEdited = state.workspaceEdited;
export const shows = state.shows;
export const ledgerScope = state.ledgerScope;
export const ledgerAllWork = state.ledgerAllWork;
export const setLedgerAllWork = state.setLedgerAllWork;
export const noteSelected = state.noteSelected;
export const { includeAdded, keepInView, renameWorkspaceFleet, projectsPlus, projectWorkspaceEntries, fleetWorkspaceEntries } = membership;

// ── Notices ────────────────────────────────────────────────────────────────

let wasEdited = false;

/** Says a change to screen readers without moving the keys. */
function announce(text: string) {
  const live = document.getElementById("ws-live");
  if (!live) return;
  live.textContent = "";
  requestAnimationFrame(() => (live.textContent = text));
}

/** The profile changed in memory. */
function changed() {
  const now = workspaceEdited();
  if (now !== wasEdited) announce(t(now ? "workspace.editedSay" : "workspace.cleanSay", { name: workspaceName() }));
  wasEdited = now;
  render();
}

state.setHooks({ announce, changed });

// ── Files ──────────────────────────────────────────────────────────────────

/** Makes the read file the document, Clean, and shows it. */
function commit(l: Awaited<ReturnType<typeof read>> | null, focus = true) {
  state.doc = l
    ? { path: l.path, profile: l.profile, base: canon(l.profile), revision: l.revision, disk: "ok", kept: false, answered: false }
    : null;
  state.autoOpened.clear();
  state.allWork = false;
  useTreeView(state.doc ? state.treeView : null);
  if (l) remember(l.path);
  wasEdited = workspaceEdited();
  showView(focus);
}

/** A new Untitled workspace: empty, Clean, no file. */
function commitUntitled() {
  state.doc = { path: null, profile: emptyProfile(), base: canon(emptyProfile()), revision: null, disk: "ok", kept: false, answered: false };
  state.autoOpened.clear();
  state.allWork = false;
  useTreeView(state.treeView);
  forgetLast();
  wasEdited = false;
  showView();
}

/** After a switch: the saved pick if it shows, else the first project that shows, else none. The keys go to the sidebar's current row. */
function showView(focus = true) {
  const shown = treeSections().flatMap((s) => s.projects);
  const want = state.doc?.profile.selected ? shown.find((p) => trimPath(p.path) === trimPath(state.doc!.profile.selected!)) : undefined;
  const target = want ?? (S.selectedProject && shown.some((p) => p.name === S.selectedProject) ? undefined : shown[0]);
  state.quietly(() => {
    if (target) selectProject(target.name);
    render();
  });
  announce(state.doc ? t("workspace.openedSay", { name: workspaceName() }) : t("workspace.allProjects"));
  if (focus) requestAnimationFrame(() => home()?.focus());
}

/** Writes the document to `path`. `expect`: the revision there now, null for no file. */
async function write(path: string, expect: string | null): Promise<boolean> {
  if (!state.doc) return false;
  const d = state.doc;
  const snap = clone(d.profile);
  state.saving = true;
  render();
  announce(t("workspace.savingSay"));
  try {
    const revision = await writeFile(path, snap, expect);
    d.path = path;
    d.revision = revision;
    d.base = canon(snap);
    d.disk = "ok";
    d.kept = false;
    d.answered = false;
    remember(path);
    announce(t("workspace.savedSay", { file: fileName(path) }));
    return true;
  } catch (e) {
    const text = String(e);
    if (text.startsWith("conflict:") && path === d.path) d.disk = "changed";
    announce(t("workspace.notSavedSay", { error: text }));
    showError(t("workspace.notSaved", { error: text.replace(/^conflict: /, "") }));
    return false;
  } finally {
    state.saving = false;
    changed();
  }
}

/** Save: to the file, else Save As. A file that changed on disk or is gone goes to Save As too. */
async function saveNow(): Promise<boolean> {
  if (!state.doc) return false;
  if (!state.doc.path || state.doc.disk !== "ok") return saveAsNow();
  const path = state.doc.path;
  const ok = await write(path, state.doc.revision);
  // The file changed under the save: keep it, and offer another file.
  if (!ok && state.doc.disk !== "ok") return saveAsNow();
  return ok;
}

async function saveAsNow(): Promise<boolean> {
  if (!state.doc) return false;
  const name = workspaceName().replace(/[/\\]/g, "-");
  const picked = await saveDialog({
    title: t("workspace.saveAsTitle"),
    defaultPath: state.doc.path ?? `${name}.${EXT}`,
    filters: [{ name: t("workspace.fileType"), extensions: [EXT] }],
  }).catch((e) => {
    showError(e);
    return null;
  });
  if (!picked) return false;
  const path = picked.endsWith(`.${EXT}`) ? picked : `${picked}.${EXT}`;
  if (path === state.doc.path) {
    if (state.doc.disk !== "ok") {
      showError(t("workspace.changedOrGone", { file: fileName(path) }));
      return false;
    }
    return write(path, state.doc.revision);
  }
  let expect: string | null;
  try {
    expect = await revision(path);
  } catch (e) {
    showError(e);
    return false;
  }
  // The file dialog asked about `picked`. With the extension added, the file it replaces was never named.
  if (expect !== null && path !== picked) {
    const ok = await choose({
      title: t("workspace.replaceTitle"),
      body: t("workspace.replaceBody", { path }),
      choices: [{ id: "replace", label: t("workspace.replace"), tone: "danger" }],
    });
    if (ok !== "replace") return false;
  }
  return write(path, expect);
}

// ── The guard ──────────────────────────────────────────────────────────────

/** Runs `f` alone: a request while another runs is dropped. */
async function exclusive(f: () => Promise<boolean>): Promise<boolean> {
  if (busy) return false;
  busy = true;
  try {
    return await f();
  } finally {
    busy = false;
  }
}

/**
 * Leaves the document: asks first when it is Edited. Quit and the restart
 * after an update use it too. `go` does the transition and reports success.
 * Save runs first and must succeed; Cancel, a closed dialog or a failed
 * write stops here and the keys go back.
 */
export function leave(what: string, go: () => Promise<boolean>): Promise<boolean> {
  const back = document.activeElement as HTMLElement | null;
  const restore = () => requestAnimationFrame(() => (back?.isConnected ? back : home())?.focus());
  return exclusive(async () => {
    if (workspaceEdited()) {
      const where = state.doc!.path ?? t("workspace.notSavedYet");
      const c = await choose({
        title: t("workspace.saveTitle", { name: workspaceName() }),
        body: t("workspace.saveBody", { where, what }),
        choices: [
          { id: "save", label: t("workspace.save"), tone: "go" },
          { id: "discard", label: t("workspace.dontSave"), tone: "plain" },
        ],
      });
      if (c === "cancel") return (restore(), false);
      if (c === "save" && !(await saveNow())) return (restore(), false);
    }
    const ok = await go();
    if (!ok) restore();
    return ok;
  });
}

/** Reads `path` and opens it. A bad file leaves the document as it was. */
async function openNow(path: string): Promise<boolean> {
  let l: Awaited<ReturnType<typeof read>>;
  try {
    l = await read(path);
  } catch (e) {
    const known = recents().includes(path);
    showError(t("workspace.cannotOpen", { error: String(e) }), known ? { label: t("workspace.removeRecent"), run: async () => forget(path, render) } : undefined);
    return false;
  }
  commit(l);
  return true;
}

export function newWorkspace() {
  void leave(t("workspace.leaveNew"), async () => {
    commitUntitled();
    return true;
  }).then((ok) => ok && editWorkspace());
}

/** Open…: the file first, so a closed file dialog asks nothing. */
let picking = false;

export async function openWorkspace(path?: string) {
  if (busy || picking) return;
  let target: string | string[] | null = path ?? null;
  if (!target) {
    picking = true;
    target = await openDialog({ title: t("workspace.openTitle"), filters: [{ name: t("workspace.fileType"), extensions: [EXT] }], multiple: false, directory: false })
      .catch((e) => {
        showError(e);
        return null;
      })
      .finally(() => (picking = false));
  }
  if (typeof target !== "string") return;
  await leave(t("workspace.leaveOpen", { file: fileName(target) }), () => openNow(target));
}

/** Close workspace: back to All projects. */
export function closeWorkspace() {
  if (!state.doc) return;
  void leave(t("workspace.leaveClose"), async () => {
    commit(null);
    forgetLast();
    return true;
  });
}

export function saveWorkspace() {
  void exclusive(saveNow);
}

export function saveWorkspaceAs() {
  void exclusive(saveAsNow);
}

/** Quit and the window's close: the same guard. Sessions keep running in skiffd. */
export function quitApp() {
  void leave(t("workspace.leaveQuit"), async () => {
    try {
      await invoke("quit_app");
      return true;
    } catch (e) {
      showError(e);
      return false;
    }
  });
}

// ── Outside changes ────────────────────────────────────────────────────────

async function checkDisk() {
  if (!state.doc?.path || state.saving || busy) return;
  const d = state.doc;
  let now: Doc["disk"];
  try {
    const rev = await revision(d.path!);
    now = rev === null ? "missing" : rev === d.revision ? "ok" : "changed";
  } catch {
    now = "missing";
  }
  if (state.doc !== d || state.saving || now === d.disk) return;
  d.disk = now;
  d.answered = false;
  if (now === "changed") announce(t("workspace.changedSay", { file: fileName(d.path!) }));
  else if (now === "missing") announce(t("workspace.unavailableSay", { file: fileName(d.path!) }));
  changed();
}

/** Reload from disk: read and check first, then ask before edits go. */
async function reload() {
  await exclusive(async () => {
    if (!state.doc?.path) return false;
    let l: Awaited<ReturnType<typeof read>>;
    try {
      l = await read(state.doc.path);
    } catch (e) {
      showError(t("workspace.cannotReload", { error: String(e) }));
      return false;
    }
    if (workspaceEdited()) {
      const c = await choose({
        title: t("workspace.reloadTitle", { name: workspaceName() }),
        body: t("workspace.reloadBody", { path: state.doc.path }),
        choices: [{ id: "reload", label: t("workspace.reload"), tone: "danger" }],
      });
      if (c !== "reload") return false;
    }
    commit(l);
    return true;
  });
}

/** Keep current: the view stays. While the disk differs, it counts as an edit. */
function keepCurrent() {
  if (!state.doc) return;
  state.doc.kept = true;
  state.doc.answered = true;
  changed();
  requestAnimationFrame(() => home()?.focus());
}

// ── Edit dialog ────────────────────────────────────────────────────────────

/** Name, projects, fleets, and a repair or remove for each missing reference. Apply changes the profile in memory. */
export function editWorkspace() {
  if (!state.doc || state.saving) return;
  const d = state.doc;
  const back = document.activeElement as HTMLElement | null;
  const { overlay, panel } = dialogFrame("confirm-panel ws-edit", "dialog", t("workspace.editLabel", { name: workspaceName() }));
  const title = h("div", "confirm-title", t("workspace.editTitle"));
  const body = h("div", "ws-edit-body");
  const nameLabel = h("label", "ws-field");
  const name = h("input", "prompt-input");
  name.type = "text";
  name.value = d.profile.name;
  name.placeholder = t("workspace.untitled");
  name.spellcheck = false;
  nameLabel.append(h("span", "ws-label", t("workspace.name")), name);
  body.append(nameLabel, h("div", "ws-note", t("workspace.editNote")));

  const fleets = [...new Set(S.projects.map((p) => p.fleet).filter((f): f is string => !!f))];
  // One tree in the sidebar's order: projects in no fleet, then each fleet with its projects under it.
  const box = (label: string, checked: boolean, nested = false) => {
    const l = h("label", nested ? "ws-check nested" : "ws-check");
    const c = h("input");
    c.type = "checkbox";
    c.checked = checked;
    l.append(c, h("span", "ws-check-name", label));
    return { l, c };
  };
  const fleetBoxes = new Map<string, HTMLInputElement>();
  const projectBoxes = new Map<string, HTMLInputElement>();
  // Fleet coverage changes the checkbox display, not an explicit inclusion.
  const explicitProjects = new Set(d.profile.projects.map(trimPath));
  const kidsOf = new Map<string, HTMLInputElement[]>();
  const loose: HTMLInputElement[] = [];
  const tree = h("div", "ws-tree");
  tree.setAttribute("role", "group");
  tree.setAttribute("aria-label", t("workspace.treeLabel"));
  const inList = (p: Project) => d.profile.projects.some((x) => trimPath(x) === trimPath(p.path));
  const projectBox = (p: Project, nested: boolean) => {
    const { l, c } = box(p.name, inList(p), nested);
    c.value = trimPath(p.path);
    l.title = p.path;
    projectBoxes.set(trimPath(p.path), c);
    c.addEventListener("change", () => {
      if (c.checked) explicitProjects.add(trimPath(p.path));
      else explicitProjects.delete(trimPath(p.path));
    });
    tree.append(l);
    return c;
  };
  for (const p of S.projects) if (!p.fleet) loose.push(projectBox(p, false));
  for (const f of fleets) {
    const { l, c } = box(f, d.profile.fleets.includes(f));
    fleetBoxes.set(f, c);
    tree.append(l);
    kidsOf.set(f, S.projects.filter((p) => p.fleet === f).map((p) => projectBox(p, true)));
  }

  /** A checked fleet covers its projects: they show checked and cannot change. Some projects alone: the fleet is mixed. */
  const syncFleet = (f: string) => {
    const fc = fleetBoxes.get(f)!;
    const kids = kidsOf.get(f)!;
    for (const k of kids) {
      k.checked = fc.checked || explicitProjects.has(k.value);
      k.disabled = fc.checked;
    }
    fc.indeterminate = !fc.checked && kids.some((k) => k.checked);
  };
  const all = h("input");
  all.type = "checkbox";
  const syncAll = () => {
    const tops = [...fleetBoxes.values(), ...loose];
    const on = tops.filter((c) => c.checked).length;
    const some = on > 0 || [...projectBoxes.values()].some((c) => c.checked);
    all.checked = tops.length > 0 && on === tops.length;
    all.indeterminate = !all.checked && some;
  };
  for (const [f, fc] of fleetBoxes) {
    fc.addEventListener("change", () => {
      syncFleet(f);
      syncAll();
    });
    for (const k of kidsOf.get(f)!)
      k.addEventListener("change", () => {
        syncFleet(f);
        syncAll();
      });
  }
  for (const c of loose) c.addEventListener("change", syncAll);
  all.addEventListener("change", () => {
    for (const c of [...fleetBoxes.values(), ...loose]) c.checked = all.checked;
    if (!all.checked) explicitProjects.clear();
    else for (const p of S.projects) if (!p.fleet) explicitProjects.add(trimPath(p.path));
    for (const f of fleets) syncFleet(f);
    syncAll();
  });
  for (const f of fleets) syncFleet(f);
  syncAll();
  // Up and Down step through the boxes that can change.
  const step = (e: KeyboardEvent) => {
    if ((e.key !== "ArrowDown" && e.key !== "ArrowUp") || e.ctrlKey || e.metaKey || e.altKey) return;
    const stops = [all, ...tree.querySelectorAll<HTMLInputElement>("input:not(:disabled)")];
    const i = stops.indexOf(document.activeElement as HTMLInputElement);
    if (i < 0) return;
    stops[(i + (e.key === "ArrowDown" ? 1 : -1) + stops.length) % stops.length].focus();
    e.preventDefault();
  };
  tree.addEventListener("keydown", step);

  const head = h("label", "ws-sec");
  if (S.projects.length) {
    all.setAttribute("aria-label", t("workspace.selectAllLabel"));
    head.append(all, h("span", "", t("workspace.selectAll")));
    all.addEventListener("keydown", step);
  } else head.textContent = t("workspace.projects");
  body.append(head);
  if (!S.projects.length) body.append(h("div", "ws-note", t("workspace.noProjects")));
  body.append(tree);
  const repairs = new Map<ReturnType<typeof state.missing>[number], HTMLSelectElement>();
  const lost = state.missing();
  if (lost.length) {
    body.append(h("div", "ws-sec", t("workspace.missing")));
    for (const m of lost) {
      const row = h("div", "ws-missing");
      const sel = h("select", "ws-repair");
      sel.setAttribute("aria-label", t("workspace.repairLabel", { ref: m.ref }));
      const opt = (value: string, text: string) => {
        const o = h("option", "", text);
        o.value = value;
        sel.append(o);
      };
      opt("keep", t("workspace.keepUnresolved"));
      opt("remove", t("workspace.removeProject"));
      if (m.kind === "project") for (const p of S.projects) opt(`use:${trimPath(p.path)}`, t("workspace.useProject", { name: p.name, path: p.path }));
      else for (const f of fleets) opt(`use:${f}`, t("workspace.useFleet", { name: f }));
      row.append(h("span", "ws-missing-ref", m.ref), h("span", "ws-missing-why", m.why), sel);
      repairs.set(m, sel);
      body.append(row);
    }
  }
  const foot = h("div", "confirm-foot");
  const cancel = button("confirm-cancel", t("workspace.cancel"), () => close());
  const apply = button("confirm-act go", t("workspace.apply"), () => submit());
  foot.append(cancel, apply);
  panel.append(title, body, foot);
  overlay.append(panel);
  document.body.append(overlay);

  function close() {
    overlay.remove();
    (back?.isConnected ? back : home())?.focus();
  }
  function submit() {
    if (state.doc !== d || state.saving) return close();
    const next = clone(d.profile);
    next.name = name.value.trim();
    const shownBefore = new Set(S.projects.filter(shows).map((p) => trimPath(p.path)));
    // Missing references keep their place unless repaired or removed.
    let projects = d.profile.projects.filter((x) => !S.projects.some((p) => trimPath(p.path) === trimPath(x)));
    let fleetsNext = d.profile.fleets.filter((f) => !fleets.includes(f));
    for (const path of projectBoxes.keys()) if (explicitProjects.has(path)) projects.push(path);
    for (const [f, c] of fleetBoxes) if (c.checked) fleetsNext.push(f);
    for (const [m, sel] of repairs) {
      if (sel.value === "keep") continue;
      const to = sel.value.startsWith("use:") ? sel.value.slice(4) : null;
      if (m.kind === "project") {
        projects = projects.filter((x) => x !== m.ref);
        if (to) projects.push(to);
        next.folded_projects = next.folded_projects.map((x) => (x === m.ref && to ? to : x)).filter((x) => x !== m.ref);
        if (next.selected === m.ref) next.selected = to;
      } else {
        fleetsNext = fleetsNext.filter((x) => x !== m.ref);
        if (to) fleetsNext.push(to);
        next.folded_fleets = next.folded_fleets.map((x) => (x === m.ref && to ? to : x)).filter((x) => x !== m.ref);
      }
    }
    next.projects = [...new Set(projects)];
    next.fleets = [...new Set(fleetsNext)];
    d.profile = next;
    // A project that left the view takes its fold and pick with it.
    const shownNow = new Set(S.projects.filter(shows).map((p) => trimPath(p.path)));
    const left = (x: string) => shownBefore.has(trimPath(x)) && !shownNow.has(trimPath(x));
    next.folded_projects = next.folded_projects.filter((x) => !left(x));
    if (next.selected && left(next.selected)) next.selected = null;
    overlay.remove();
    changed();
    showView();
  }
  overlay.addEventListener("mousedown", (e) => e.target === overlay && close());
  overlay.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Escape") close();
    else if (e.key === "Enter" && e.target === name) submit();
    else if (e.key === "Tab") trapTab(panel, e);
    else return;
    e.preventDefault();
  });
  name.focus();
  name.select();
}

// ── Menus and the status bar ───────────────────────────────────────────────

/** Where the keys go when what had them is gone: the sidebar's current row. */
const home = () => document.querySelector<HTMLElement>('#sidebar-scroll [tabindex="0"]');

const recentEntries = (): MenuEntry[] =>
  recents().map((path) => ({
    label: path === state.doc?.path ? t("workspace.recentOpen", { file: fileName(path) }) : fileName(path),
    run: () => void openWorkspace(path),
  }));

/** File › Open recent workspace…: the recent files, under the Projects head as Open closed project does. */
export function recentWorkspacesMenu() {
  const r = document.querySelector<HTMLElement>("#sidebar-scroll .tree-head:not(.fleet-head)")?.getBoundingClientRect();
  const entries = recentEntries();
  if (!entries.length) entries.push({ label: t("workspace.noRecent"), disabled: true });
  ctxMenu.open(r ? r.left + 24 : 80, r ? r.bottom : 80, t("workspace.recentTitle"), entries);
}

/** What the open workspace needs a look for, for the File menu: a disk change, and missing references. */
export function workspaceNotice(): { disk: "changed" | "missing" | null; missing: number } {
  return { disk: state.doc && state.doc.disk !== "ok" && !state.doc.answered ? state.doc.disk : null, missing: state.missing().length };
}

export const reloadWorkspace = () => void reload();
export const keepWorkspace = () => keepCurrent();

/** The status bar item's menu: the same workspace commands as the File menu. */
function workspaceMenu(at: HTMLElement) {
  const r = at.getBoundingClientRect();
  // The keys come back to the item: on Esc, and before a pick runs, so its dialog returns them here.
  const back = () => (at.isConnected && !at.hidden ? at : home())?.focus();
  const wrap = (es: MenuEntry[]): MenuEntry[] =>
    es.map((e) => ("head" in e || "sep" in e ? e : { ...e, run: e.run && (() => (back(), e.run!())), sub: e.sub && wrap(e.sub) }));
  ctxMenu.open(r.left, r.top, state.doc?.path ?? workspaceName(), wrap(menuEntries()), () => requestAnimationFrame(back));
}

function menuEntries(): MenuEntry[] {
  const on = !!state.doc;
  const recent = recentEntries();
  const n = workspaceNotice();
  const notice: MenuEntry[] = [];
  if (n.disk) {
    notice.push({ head: upper(t(n.disk === "changed" ? "workspace.changedOnDisk" : "workspace.fileMissing")) });
    if (n.disk === "changed") notice.push({ label: t("workspace.reloadFromDisk"), run: reloadWorkspace });
    notice.push({ label: t("workspace.keepCurrent"), run: keepWorkspace });
  }
  if (n.missing) notice.push({ icon: "tools-pencil", label: tn("workspace.repair", n.missing), run: editWorkspace });
  if (notice.length) notice.push({ head: upper(t("workspace.head")) });
  return [
    ...notice,
    { icon: "indicators-plus", label: t("cmd.newWorkspace"), run: newWorkspace },
    { icon: "documents-folder-open", label: t("cmd.openWorkspace"), run: () => void openWorkspace() },
    { icon: "documents-folder-open", label: t("workspace.openRecent"), sub: recent.length ? recent : [{ label: t("workspace.noRecent"), disabled: true }], disabled: !recent.length },
    { icon: "tools-pencil", label: t("cmd.editWorkspace"), run: editWorkspace, disabled: !on },
    { label: t("workspace.save"), run: saveWorkspace, disabled: !on },
    { label: t("workspace.saveAs"), run: saveWorkspaceAs, disabled: !on },
    { icon: "code-ungroup", label: t("workspace.closeToAll"), run: closeWorkspace, disabled: !on },
    { label: state.allWork ? t("workspace.ledgerShowWorkspace") : t("workspace.ledgerShowAll"), run: () => setLedgerAllWork(!state.allWork), disabled: !on },
  ];
}

/** The one short mark beside the name: what needs a look first. */
function markOf(lost: number): { text: string; cls: string; say: string } | null {
  if (state.saving) return { text: t("workspace.markSaving"), cls: "", say: t("workspace.saySaving") };
  if (state.doc && state.doc.disk !== "ok" && !state.doc.answered) {
    const changedOnDisk = state.doc.disk === "changed";
    return {
      text: t(changedOnDisk ? "workspace.changedOnDisk" : "workspace.fileMissing"),
      cls: "edited",
      say: t(changedOnDisk ? "workspace.sayChanged" : "workspace.sayFileMissing"),
    };
  }
  if (workspaceEdited()) return { text: t("workspace.markEdited"), cls: "edited", say: t("workspace.sayEdited") };
  if (lost) return { text: tn("workspace.markMissing", lost), cls: "", say: tn("workspace.sayMissing", lost) };
  return state.doc ? { text: "", cls: "", say: t("workspace.saySaved") } : null;
}

/**
 * The status bar's sign that a workspace is open: its name and one mark.
 * Hidden in All projects. A click opens the workspace menu. Runs on each render.
 */
export function renderWorkspaceStatus() {
  if (!document.getElementById("ws-live")) {
    const live = h("div", "visually-hidden");
    live.id = "ws-live";
    live.setAttribute("role", "status");
    live.setAttribute("aria-live", "polite");
    document.body.append(live);
  }
  const el = document.getElementById("ws-status");
  if (!el) return;
  setHidden(el, !state.doc);
  if (!state.doc) return;
  const lost = state.missing();
  const mark = markOf(lost.length);
  const name = workspaceName();
  const why = lost.map((m) => t(m.kind === "project" ? "workspace.missingProject" : "workspace.missingFleet", { ref: m.ref, why: m.why }));
  const sig = JSON.stringify([name, state.doc.path, mark, why]);
  if (el.dataset.sig === sig) return;
  el.dataset.sig = sig;
  el.replaceChildren(h("span", "ws-status-name", name));
  if (mark?.text) el.append(h("span", `ws-status-mark ${mark.cls}`.trim(), mark.text));
  el.title = [state.doc.path ?? t("workspace.notSavedYet"), ...why].join("\n");
  el.setAttribute("aria-label", mark ? t("workspace.statusLabelSay", { name, state: mark.say }) : t("workspace.statusLabel", { name }));
  if (!el.dataset.wired) {
    el.dataset.wired = "1";
    el.addEventListener("click", () => workspaceMenu(el));
  }
}

// ── Start ──────────────────────────────────────────────────────────────────

/** Register both exit routes before menus or documents become interactive. */
export async function startWorkspaceQuitGuard() {
  await listen("skiff:quit-requested", () => quitApp());
  await getCurrentWindow().onCloseRequested((e) => {
    e.preventDefault();
    quitApp();
  });
  await invoke("guard_quit");
}

/**
 * At start, once projects are in: the file the app was started with, else
 * the last saved workspace, read from disk as Clean. A file that cannot
 * open says why and leaves All projects; its recent entry stays.
 */
export async function startWorkspaces() {
  void listen<string>("skiff:open-workspace", (e) => {
    // Before the start is done, the start takes it from the pending list.
    if (!state.ready) return;
    void invoke("workspace_took", { path: e.payload }).catch(() => null);
    void openWorkspace(e.payload);
  });
  window.addEventListener("focus", () => void checkDisk());
  setInterval(() => void checkDisk(), 2000);

  const launch = await invoke<string | null>("workspace_launch_file").catch(() => null);
  let last: string | null = null;
  try {
    last = lastPath();
  } catch {
    /* none */
  }
  const path = launch ?? last;
  if (path) {
    try {
      commit(await read(path), false);
    } catch (e) {
      forgetLast();
      const known = recents().includes(path);
      showError(t("workspace.cannotOpenStart", { error: String(e) }), known ? { label: t("workspace.removeRecent"), run: async () => forget(path, render) } : undefined);
    }
  }
  state.ready = true;
  render();
  // A file the OS sent while the first one opened.
  const late = await invoke<string | null>("workspace_launch_file").catch(() => null);
  if (late) void openWorkspace(late);
}
