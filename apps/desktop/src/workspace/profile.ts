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
import { fleetKey, projectKey, treeSections, useTreeView } from "./tree";
import { selectProject } from "./view";
import { t, tn, upper } from "../i18n";

/** The profile as `workspace_read` gives it and `workspace_write` takes it. */
export interface Profile {
  name: string;
  /** Project folders. */
  projects: string[];
  /** Fleet names. */
  fleets: string[];
  folded_projects: string[];
  folded_fleets: string[];
  /** The folder of the last project the user picked here. */
  selected: string | null;
}

interface Doc {
  path: string | null;
  profile: Profile;
  /** `canon` of the profile last read or saved. */
  base: string;
  /** The revision last read or written. null: no file yet. */
  revision: string | null;
  /** The file on disk: as read, changed by something else, or gone or unreadable. */
  disk: "ok" | "changed" | "missing";
  /** The person chose Keep current while the disk differs: that counts as an edit. */
  kept: boolean;
  /** The notice for this disk state was answered. A new change shows it again. */
  answered: boolean;
}

const EXT = "skiffw";
const LAST_KEY = "skiff.workspace.last";
const RECENT_KEY = "skiff.workspace.recent";
const RECENT_MAX = 8;

let doc: Doc | null = null;
/** Rows the screen opened in this workspace. Not saved, not an edit. */
const autoOpened = new Set<string>();
/** A save is writing. Edits wait: a fold or pick now would miss the file. */
let saving = false;
/** A transition or save runs. A second request is dropped, so one prompt and one write at a time. */
let busy = false;
/** Above 0 while the app, not the user, moves the selection. */
let quiet = 0;
/** Off until the launch has opened its workspace: the first restore is no edit. */
let ready = false;
/** The Ledger shows every row, whatever the workspace. Never saved. */
let allWork = false;

const trimPath = (p: string) => (p.length > 1 ? p.replace(/\/+$/, "") : p);
const uniq = (xs: string[]) => [...new Set(xs)].sort();

/** The profile as a value: order and repeats do not count. */
function canon(p: Profile): string {
  return JSON.stringify([
    p.name.trim(),
    uniq(p.projects.map(trimPath)),
    uniq(p.fleets),
    uniq(p.folded_projects.map(trimPath)),
    uniq(p.folded_fleets),
    p.selected ? trimPath(p.selected) : null,
  ]);
}

const emptyProfile = (): Profile => ({ name: "", projects: [], fleets: [], folded_projects: [], folded_fleets: [], selected: null });
const clone = (p: Profile): Profile => JSON.parse(JSON.stringify(p));

export const workspaceOpen = () => doc !== null;
export const workspaceName = () => (doc ? doc.profile.name.trim() || t("workspace.untitled") : t("workspace.allProjects"));

export function workspaceEdited(): boolean {
  return !!doc && (canon(doc.profile) !== doc.base || (doc.kept && doc.disk !== "ok"));
}

// ── What shows ─────────────────────────────────────────────────────────────

/** The project is in the open workspace, by itself or by its fleet. Always true in All projects. */
export function shows(p: Project): boolean {
  if (!doc) return true;
  return doc.profile.projects.some((x) => trimPath(x) === trimPath(p.path)) || (!!p.fleet && doc.profile.fleets.includes(p.fleet));
}

const shownProjects = () => S.projects.filter(shows);

/**
 * The project names whose work the Ledger shows, or null for every row.
 * A row with no project shows in every workspace.
 */
export function ledgerScope(): { label: string; names: Set<string> } | null {
  if (!doc || allWork) return null;
  return { label: workspaceName(), names: new Set(shownProjects().map((p) => p.name)) };
}

export const ledgerAllWork = () => allWork;

/** All work, or back to the workspace's work. No edit, no prompt. */
export function setLedgerAllWork(on: boolean) {
  if (allWork === on) return;
  allWork = on;
  announce(on ? t("workspace.ledgerAll") : t("workspace.ledgerScoped", { name: workspaceName() }));
  render();
}

/** A reference with nothing to point at. It stays in the file until the user repairs or removes it. */
interface Missing {
  kind: "project" | "fleet";
  ref: string;
  why: string;
}

function missing(): Missing[] {
  if (!doc) return [];
  const out: Missing[] = [];
  for (const path of doc.profile.projects) {
    if (S.projects.some((p) => trimPath(p.path) === trimPath(path))) continue;
    const closed = S.closedProjects.find((p) => trimPath(p.path) === trimPath(path));
    out.push({ kind: "project", ref: path, why: closed ? t("workspace.whyClosed", { name: closed.name }) : t("workspace.whyNoProject") });
  }
  const fleets = new Set(S.projects.map((p) => p.fleet).filter((f): f is string => !!f));
  for (const f of doc.profile.fleets) if (!fleets.has(f)) out.push({ kind: "fleet", ref: f, why: t("workspace.whyNoFleet") });
  return out;
}

// ── Folds and picks ────────────────────────────────────────────────────────

const pathOf = (name: string) => {
  const p = S.projects.find((x) => x.name === name);
  return p ? trimPath(p.path) : null;
};

/** A tree key, as the file names it: a project by folder, a fleet by name. */
function refOf(key: string): { list: "folded_projects" | "folded_fleets"; ref: string } | null {
  if (key.startsWith("project:")) {
    const ref = pathOf(key.slice("project:".length));
    return ref ? { list: "folded_projects", ref } : null;
  }
  if (key.startsWith("fleet:")) return { list: "folded_fleets", ref: key.slice("fleet:".length) };
  return null;
}

const treeView = {
  shows,
  isFolded(key: string) {
    const r = doc && refOf(key);
    return !!r && !autoOpened.has(key) && doc!.profile[r.list].some((x) => x === r.ref);
  },
  fold(key: string, fold: boolean) {
    const r = doc && refOf(key);
    if (!r || saving) return;
    autoOpened.delete(key);
    const list = doc!.profile[r.list].filter((x) => x !== r.ref);
    if (fold) list.push(r.ref);
    doc!.profile[r.list] = list;
    changed();
  },
  autoOpen(key: string) {
    autoOpened.add(key);
  },
  keys() {
    if (!doc) return [];
    const out: string[] = [];
    for (const p of S.projects) if (treeView.isFolded(projectKey(p.name))) out.push(projectKey(p.name));
    for (const f of doc.profile.folded_fleets) if (treeView.isFolded(fleetKey(f))) out.push(fleetKey(f));
    return out;
  },
};

/** The user picked a project. In a workspace that includes it, that is an edit. */
export function noteSelected(name: string) {
  if (!doc || !ready || quiet || saving) return;
  const p = S.projects.find((x) => x.name === name);
  if (!p || !shows(p)) return;
  const path = trimPath(p.path);
  if (doc.profile.selected === path) return;
  doc.profile.selected = path;
  changed();
}

function quietly(f: () => void) {
  quiet++;
  try {
    f();
  } finally {
    quiet--;
  }
}

// ── Add and remove ─────────────────────────────────────────────────────────

const fleetNames = () => [...new Set(S.projects.map((p) => p.fleet).filter((f): f is string => !!f))];
const listed = (p: Project) => !!doc && doc.profile.projects.some((x) => trimPath(x) === trimPath(p.path));

/** Puts a project or fleet in the open workspace, or takes it out. An edit, like a change in the Edit dialog. */
function setIncluded(kind: "project" | "fleet", ref: string, on: boolean) {
  if (!doc || saving) return;
  const d = doc;
  const shownBefore = new Set(S.projects.filter(shows).map((p) => trimPath(p.path)));
  if (kind === "project") {
    const rest = d.profile.projects.filter((x) => trimPath(x) !== trimPath(ref));
    d.profile.projects = on ? [...rest, trimPath(ref)] : rest;
  } else {
    const rest = d.profile.fleets.filter((x) => x !== ref);
    d.profile.fleets = on ? [...rest, ref] : rest;
  }
  // A project that left the view takes its fold and pick with it.
  const left = (x: string) => shownBefore.has(trimPath(x)) && !S.projects.some((p) => trimPath(p.path) === trimPath(x) && shows(p));
  d.profile.folded_projects = d.profile.folded_projects.filter((x) => !left(x));
  if (d.profile.selected && left(d.profile.selected)) d.profile.selected = null;
  if (kind === "fleet" && !on) d.profile.folded_fleets = d.profile.folded_fleets.filter((x) => x !== ref);
  const workspace = workspaceName();
  if (kind === "fleet") announce(t(on ? "workspace.fleetAdded" : "workspace.fleetRemoved", { fleet: ref, workspace }));
  else {
    const project = S.projects.find((p) => trimPath(p.path) === trimPath(ref))?.name ?? ref;
    announce(t(on ? "workspace.projectAdded" : "workspace.projectRemoved", { project, workspace }));
  }
  changed();
  if (!on) showView(false);
}

/** A project Skiff just added: in a workspace, it goes in too, so it shows where the person added it. */
export function includeAdded(p: Project) {
  if (doc && !shows(p)) setIncluded("project", p.path, true);
}

/** A project that goes to a fleet the workspace does not name, or to none, stays in by its folder. Call it before the change. */
export function keepInView(p: Project, fleet: string | null) {
  if (doc && shows(p) && !listed(p) && !(fleet && doc.profile.fleets.includes(fleet))) setIncluded("project", p.path, true);
}

/** A renamed fleet stays in the workspace, folded or open as it was. */
export function renameWorkspaceFleet(from: string, to: string) {
  if (!doc || saving || from === to) return;
  const d = doc.profile;
  if (!d.fleets.includes(from) && !d.folded_fleets.includes(from)) return;
  const swap = (xs: string[]) => [...new Set(xs.map((x) => (x === from ? to : x)))];
  d.fleets = swap(d.fleets);
  d.folded_fleets = swap(d.folded_fleets);
  changed();
}

/** The + over Projects: in a workspace, the projects and fleets it does not show yet, then Add project. */
export function projectsPlus(at: HTMLElement, addProject: () => void) {
  if (!doc) return addProject();
  const r = at.getBoundingClientRect();
  const fleets = fleetNames().filter((f) => !doc!.profile.fleets.includes(f));
  const hidden = S.projects.filter((p) => !shows(p));
  const entries: MenuEntry[] = [];
  if (fleets.length || hidden.length) entries.push({ head: upper(t("workspace.headAdd")) });
  for (const f of fleets) entries.push({ icon: "code-group", label: t("menu.fleet", { name: f }), run: () => setIncluded("fleet", f, true) });
  for (const p of hidden) entries.push({ label: p.fleet ? t("workspace.projectInFleet", { name: p.name, fleet: p.fleet }) : p.name, run: () => setIncluded("project", p.path, true) });
  if (entries.length) entries.push({ head: upper(t("workspace.headSkiff")) });
  entries.push({ icon: "indicators-plus", label: t("cmd.addProject"), run: addProject });
  ctxMenu.open(r.left, r.bottom, workspaceName(), entries);
}

/** The workspace items in a project's menu. None in All projects. */
export function projectWorkspaceEntries(p: Project): MenuEntry[] {
  if (!doc) return [];
  if (listed(p)) return [{ icon: "code-ungroup", label: t("workspace.removeProject"), run: () => setIncluded("project", p.path, false) }];
  // In by its fleet only: the fleet's menu takes it out.
  return [{ icon: "code-ungroup", label: t("workspace.inByFleet", { fleet: p.fleet ?? "" }), disabled: true }];
}

/** The workspace items in a fleet's menu. None in All projects. */
export function fleetWorkspaceEntries(name: string): MenuEntry[] {
  if (!doc) return [];
  if (doc.profile.fleets.includes(name)) return [{ icon: "code-ungroup", label: t("workspace.removeFleet"), run: () => setIncluded("fleet", name, false) }];
  return [{ icon: "indicators-plus", label: t("workspace.addFleet"), run: () => setIncluded("fleet", name, true) }];
}

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

// ── Files ──────────────────────────────────────────────────────────────────

interface Loaded {
  path: string;
  profile: Profile;
  revision: string;
}

function storedList(key: string): string[] {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function store(key: string, value: string | null) {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    /* it lasts for this run */
  }
}

const recents = () => storedList(RECENT_KEY);

function remember(path: string) {
  store(RECENT_KEY, JSON.stringify([path, ...recents().filter((x) => x !== path)].slice(0, RECENT_MAX)));
  store(LAST_KEY, path);
}

function forget(path: string) {
  store(RECENT_KEY, JSON.stringify(recents().filter((x) => x !== path)));
  render();
}

const fileName = (path: string) => path.split("/").pop() ?? path;

/** Makes the read file the document, Clean, and shows it. */
function commit(l: Loaded | null, focus = true) {
  doc = l
    ? { path: l.path, profile: l.profile, base: canon(l.profile), revision: l.revision, disk: "ok", kept: false, answered: false }
    : null;
  autoOpened.clear();
  allWork = false;
  useTreeView(doc ? treeView : null);
  if (l) remember(l.path);
  wasEdited = workspaceEdited();
  showView(focus);
}

/** A new Untitled workspace: empty, Clean, no file. */
function commitUntitled() {
  doc = { path: null, profile: emptyProfile(), base: canon(emptyProfile()), revision: null, disk: "ok", kept: false, answered: false };
  autoOpened.clear();
  allWork = false;
  useTreeView(treeView);
  store(LAST_KEY, null);
  wasEdited = false;
  showView();
}

/** After a switch: the saved pick if it shows, else the first project that shows, else none. The keys go to the sidebar's current row. */
function showView(focus = true) {
  const shown = treeSections().flatMap((s) => s.projects);
  const want = doc?.profile.selected ? shown.find((p) => trimPath(p.path) === trimPath(doc!.profile.selected!)) : undefined;
  const target = want ?? (S.selectedProject && shown.some((p) => p.name === S.selectedProject) ? undefined : shown[0]);
  quietly(() => {
    if (target) selectProject(target.name);
    render();
  });
  announce(doc ? t("workspace.openedSay", { name: workspaceName() }) : t("workspace.allProjects"));
  if (focus) requestAnimationFrame(() => home()?.focus());
}

async function read(path: string): Promise<Loaded> {
  return invoke<Loaded>("workspace_read", { path });
}

/** Writes the document to `path`. `expect`: the revision there now, null for no file. */
async function write(path: string, expect: string | null): Promise<boolean> {
  if (!doc) return false;
  const d = doc;
  const snap = clone(d.profile);
  saving = true;
  render();
  announce(t("workspace.savingSay"));
  try {
    const revision = await invoke<string>("workspace_write", { path, profile: snap, expect });
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
    saving = false;
    changed();
  }
}

/** Save: to the file, else Save As. A file that changed on disk or is gone goes to Save As too. */
async function saveNow(): Promise<boolean> {
  if (!doc) return false;
  if (!doc.path || doc.disk !== "ok") return saveAsNow();
  const path = doc.path;
  const ok = await write(path, doc.revision);
  // The file changed under the save: keep it, and offer another file.
  if (!ok && doc.disk !== "ok") return saveAsNow();
  return ok;
}

async function saveAsNow(): Promise<boolean> {
  if (!doc) return false;
  const name = workspaceName().replace(/[/\\]/g, "-");
  const picked = await saveDialog({
    title: t("workspace.saveAsTitle"),
    defaultPath: doc.path ?? `${name}.${EXT}`,
    filters: [{ name: t("workspace.fileType"), extensions: [EXT] }],
  }).catch((e) => {
    showError(e);
    return null;
  });
  if (!picked) return false;
  const path = picked.endsWith(`.${EXT}`) ? picked : `${picked}.${EXT}`;
  if (path === doc.path) {
    if (doc.disk !== "ok") {
      showError(t("workspace.changedOrGone", { file: fileName(path) }));
      return false;
    }
    return write(path, doc.revision);
  }
  let expect: string | null;
  try {
    expect = await invoke<string | null>("workspace_revision", { path });
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
      const where = doc!.path ?? t("workspace.notSavedYet");
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
  let l: Loaded;
  try {
    l = await read(path);
  } catch (e) {
    const known = recents().includes(path);
    showError(t("workspace.cannotOpen", { error: String(e) }), known ? { label: t("workspace.removeRecent"), run: async () => forget(path) } : undefined);
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
  if (!doc) return;
  void leave(t("workspace.leaveClose"), async () => {
    commit(null);
    store(LAST_KEY, null);
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
  if (!doc?.path || saving || busy) return;
  const d = doc;
  let now: Doc["disk"];
  try {
    const rev = await invoke<string | null>("workspace_revision", { path: d.path });
    now = rev === null ? "missing" : rev === d.revision ? "ok" : "changed";
  } catch {
    now = "missing";
  }
  if (doc !== d || saving || now === d.disk) return;
  d.disk = now;
  d.answered = false;
  if (now === "changed") announce(t("workspace.changedSay", { file: fileName(d.path!) }));
  else if (now === "missing") announce(t("workspace.unavailableSay", { file: fileName(d.path!) }));
  changed();
}

/** Reload from disk: read and check first, then ask before edits go. */
async function reload() {
  await exclusive(async () => {
    if (!doc?.path) return false;
    let l: Loaded;
    try {
      l = await read(doc.path);
    } catch (e) {
      showError(t("workspace.cannotReload", { error: String(e) }));
      return false;
    }
    if (workspaceEdited()) {
      const c = await choose({
        title: t("workspace.reloadTitle", { name: workspaceName() }),
        body: t("workspace.reloadBody", { path: doc.path }),
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
  if (!doc) return;
  doc.kept = true;
  doc.answered = true;
  changed();
  requestAnimationFrame(() => home()?.focus());
}

// ── Edit dialog ────────────────────────────────────────────────────────────

/** Name, projects, fleets, and a repair or remove for each missing reference. Apply changes the profile in memory. */
export function editWorkspace() {
  if (!doc || saving) return;
  const d = doc;
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
  const repairs = new Map<Missing, HTMLSelectElement>();
  const lost = missing();
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
    if (doc !== d || saving) return close();
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

function trapTab(panel: HTMLElement, e: KeyboardEvent) {
  const stops = [...panel.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled)")];
  const i = stops.indexOf(document.activeElement as HTMLElement);
  stops[(i + (e.shiftKey ? -1 : 1) + stops.length) % stops.length]?.focus();
}

// ── Choice dialog ──────────────────────────────────────────────────────────

/**
 * A modal with Cancel and the given choices. Cancel has the keys, and Enter
 * on anything but a choice is Cancel. Esc or a click outside is Cancel.
 */
function choose(o: { title: string; body: string; choices: { id: string; label: string; tone: "go" | "danger" | "plain" }[] }): Promise<string> {
  return new Promise((resolve) => {
    const { overlay, panel } = dialogFrame("confirm-panel", "alertdialog", o.title);
    const title = h("div", "confirm-title", o.title);
    const body = h("div", "confirm-body", o.body);
    title.id = "ws-choose-title";
    body.id = "ws-choose-body";
    panel.setAttribute("aria-labelledby", title.id);
    panel.setAttribute("aria-describedby", body.id);
    panel.removeAttribute("aria-label");
    const foot = h("div", "confirm-foot");
    const done = (id: string) => {
      overlay.remove();
      resolve(id);
    };
    const cancel = button("confirm-cancel", t("workspace.cancel"), () => done("cancel"));
    foot.append(cancel);
    for (const c of o.choices) {
      const cls = c.tone === "go" ? "confirm-act go" : c.tone === "danger" ? "confirm-act" : "confirm-cancel";
      foot.append(button(cls, c.label, () => done(c.id)));
    }
    panel.append(title, body, foot);
    overlay.append(panel);
    document.body.append(overlay);
    overlay.addEventListener("mousedown", (e) => e.target === overlay && done("cancel"));
    overlay.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") done("cancel");
      else if (e.key === "Enter") {
        // Enter presses the button with the keys; anywhere else it cancels.
        const b = document.activeElement;
        if (b instanceof HTMLButtonElement && panel.contains(b)) b.click();
        else done("cancel");
      } else if (e.key === "Tab") trapTab(panel, e);
      else return;
      e.preventDefault();
    });
    cancel.focus();
  });
}

// ── Menus and the status bar ───────────────────────────────────────────────

/** Where the keys go when what had them is gone: the sidebar's current row. */
const home = () => document.querySelector<HTMLElement>('#sidebar-scroll [tabindex="0"]');

const recentEntries = (): MenuEntry[] =>
  recents().map((path) => ({
    label: path === doc?.path ? t("workspace.recentOpen", { file: fileName(path) }) : fileName(path),
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
  return { disk: doc && doc.disk !== "ok" && !doc.answered ? doc.disk : null, missing: missing().length };
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
  ctxMenu.open(r.left, r.top, doc?.path ?? workspaceName(), wrap(menuEntries()), () => requestAnimationFrame(back));
}

function menuEntries(): MenuEntry[] {
  const on = !!doc;
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
    { label: allWork ? t("workspace.ledgerShowWorkspace") : t("workspace.ledgerShowAll"), run: () => setLedgerAllWork(!allWork), disabled: !on },
  ];
}

/** The one short mark beside the name: what needs a look first. */
function markOf(lost: number): { text: string; cls: string; say: string } | null {
  if (saving) return { text: t("workspace.markSaving"), cls: "", say: t("workspace.saySaving") };
  if (doc && doc.disk !== "ok" && !doc.answered) {
    const changedOnDisk = doc.disk === "changed";
    return {
      text: t(changedOnDisk ? "workspace.changedOnDisk" : "workspace.fileMissing"),
      cls: "edited",
      say: t(changedOnDisk ? "workspace.sayChanged" : "workspace.sayFileMissing"),
    };
  }
  if (workspaceEdited()) return { text: t("workspace.markEdited"), cls: "edited", say: t("workspace.sayEdited") };
  if (lost) return { text: tn("workspace.markMissing", lost), cls: "", say: tn("workspace.sayMissing", lost) };
  return doc ? { text: "", cls: "", say: t("workspace.saySaved") } : null;
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
  setHidden(el, !doc);
  if (!doc) return;
  const lost = missing();
  const state = markOf(lost.length);
  const name = workspaceName();
  const why = lost.map((m) => t(m.kind === "project" ? "workspace.missingProject" : "workspace.missingFleet", { ref: m.ref, why: m.why }));
  const sig = JSON.stringify([name, doc.path, state, why]);
  if (el.dataset.sig === sig) return;
  el.dataset.sig = sig;
  el.replaceChildren(h("span", "ws-status-name", name));
  if (state?.text) el.append(h("span", `ws-status-mark ${state.cls}`.trim(), state.text));
  el.title = [doc.path ?? t("workspace.notSavedYet"), ...why].join("\n");
  el.setAttribute("aria-label", state ? t("workspace.statusLabelSay", { name, state: state.say }) : t("workspace.statusLabel", { name }));
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
    if (!ready) return;
    void invoke("workspace_took", { path: e.payload }).catch(() => null);
    void openWorkspace(e.payload);
  });
  window.addEventListener("focus", () => void checkDisk());
  setInterval(() => void checkDisk(), 2000);

  const launch = await invoke<string | null>("workspace_launch_file").catch(() => null);
  let last: string | null = null;
  try {
    last = localStorage.getItem(LAST_KEY);
  } catch {
    /* none */
  }
  const path = launch ?? last;
  if (path) {
    try {
      commit(await read(path), false);
    } catch (e) {
      store(LAST_KEY, null);
      const known = recents().includes(path);
      showError(t("workspace.cannotOpenStart", { error: String(e) }), known ? { label: t("workspace.removeRecent"), run: async () => forget(path) } : undefined);
    }
  }
  ready = true;
  render();
  // A file the OS sent while the first one opened.
  const late = await invoke<string | null>("workspace_launch_file").catch(() => null);
  if (late) void openWorkspace(late);
}
