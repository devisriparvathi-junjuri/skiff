/** The side panel's tree: fleets as section heads, projects as rows, each folded or open (#87). */

import { fleetSkipperOf } from "../app/skiffAgent";
import type { Project, SessionInfo, Worktree } from "../platform/types";

import { isFolder, isUnread } from "./model";
import { BELL, SKIFF } from "../appearance/stateIcon";
import { projectIcon } from "../appearance/projectIcon";
import { storedSet } from "../app/stored";
import { shownHost } from "../app/hosts";
import { removeErrors, removing, S, sessions } from "../app/state";
import { place } from "../app/stateQueries";
import { h } from "../ui/dom";
import { icon } from "../ui/icons";
import { animateFold } from "../ui/fold";

/** Folded rows in All projects: `fleet:<name>` and `project:<name>`. Every row starts open. */
const savedFolds = storedSet("skiff.tree.folded");

/**
 * What an open workspace (#154) puts over the tree: which projects show, and
 * its own folds. A user fold goes to `fold`; an automatic open (the session
 * on screen moved into a folded row) goes to `autoOpen` and is no edit.
 */
export interface TreeView {
  shows(p: Project): boolean;
  isFolded(key: string): boolean;
  fold(key: string, folded: boolean): void;
  autoOpen(key: string): void;
  keys(): string[];
}
let view: TreeView | null = null;

/** Puts a workspace over the tree, or null for All projects. */
export function useTreeView(v: TreeView | null) {
  view = v;
}

/** The folded rows the tree draws now. */
export const folded = {
  has: (key: string) => (view ? view.isFolded(key) : savedFolds.has(key)),
  keys: () => (view ? view.keys() : [...savedFolds]),
};

/** Opens a row with no edit to a workspace: the keys or the screen went there. */
function autoOpen(key: string) {
  if (view) view.autoOpen(key);
  else savedFolds.delete(key);
}

export const fleetKey = (name: string) => `fleet:${name}`;
export const projectKey = (name: string) => `project:${name}`;

export interface Section {
  /** null: the projects in no fleet. */
  fleet: string | null;
  projects: Project[];
}

/** Projects in no fleet first, then each fleet where its first project stands. */
export function treeSections(): Section[] {
  const loose: Project[] = [];
  const fleets = new Map<string, Project[]>();
  // A workspace shows some projects. A fleet with none of them shown has no head.
  for (const p of S.projects) {
    if (view && !view.shows(p)) continue;
    if (!p.fleet) loose.push(p);
    else if (fleets.has(p.fleet)) fleets.get(p.fleet)!.push(p);
    else fleets.set(p.fleet, [p]);
  }
  return [{ fleet: null, projects: loose }, ...[...fleets].map(([fleet, projects]) => ({ fleet, projects }))];
}

/** The project rows the tree shows, in order. A folded fleet, or one that hides its projects, has none. */
export function visibleProjects(): Project[] {
  return treeSections().flatMap((s) => (s.fleet && (folded.has(fleetKey(s.fleet)) || projectsHidden(s.fleet)) ? [] : s.projects));
}

/**
 * Fleets that hide their projects, in every workspace: `fleet:<name>` on this
 * computer, `host:<id>/fleet:<name>` on a remote Host, so two Hosts with a
 * fleet of one name keep their own choice. Every fleet starts with its projects shown.
 */
const savedHidden = storedSet("skiff.tree.hidden");
const hiddenKey = (fleet: string) => {
  const host = shownHost();
  return host ? `host:${host}/${fleetKey(fleet)}` : fleetKey(fleet);
};

export const projectsHidden = (fleet: string) => savedHidden.has(hiddenKey(fleet));

/** Hide projects / Show projects in the fleet menu, and the count on its head. */
export function setProjectsHidden(fleet: string, hide: boolean) {
  if (hide) savedHidden.add(hiddenKey(fleet));
  else savedHidden.delete(hiddenKey(fleet));
}

/** A renamed fleet keeps its choice. */
export function renameHidden(from: string, to: string) {
  if (!savedHidden.delete(hiddenKey(from))) return;
  savedHidden.add(hiddenKey(to));
}

/**
 * A fleet that hides its projects still shows the sessions that need a look:
 * each one that waits, and the one on screen, picked or current. The rest
 * are hidden, and the head counts them. The fleet Skipper has its own row.
 */
export function hiddenSplit(projects: Project[]): { shown: SessionInfo[]; hidden: number } {
  const shown: SessionInfo[] = [];
  let hidden = 0;
  for (const s of sessions.values()) {
    const p = place(s)?.project;
    if (!p || !projects.includes(p) || s.skipper?.startsWith("fleet:")) continue;
    if (s.state === "waiting" || s.id === S.focused || s.id === S.roveKey || S.selection.includes(s.id)) shown.push(s);
    else hidden++;
  }
  // In tree order: by project, then as the sessions came.
  shown.sort((a, b) => projects.indexOf(place(a)!.project) - projects.indexOf(place(b)!.project));
  return { shown, hidden };
}

export const isOpen = (key: string) => !folded.has(key);

/** The user opens or folds a row. In a workspace, that is an edit. */
export function setOpen(key: string, open: boolean) {
  if (view) view.fold(key, !open);
  else if (open) savedFolds.delete(key);
  else savedFolds.add(key);
}

/** The session on screen moved into a folded project or fleet: open them, so its row shows. */
let shownFor: string | null = null;
export function openToFocused() {
  if (S.focused === shownFor) return;
  shownFor = S.focused;
  const s = S.focused ? sessions.get(S.focused) : undefined;
  const p = s ? place(s)?.project : null;
  if (!p) return;
  autoOpen(projectKey(p.name));
  if (p.fleet) autoOpen(fleetKey(p.fleet));
}

/** A folded row's marks: how many sessions under it wait, work, or have unread output. */
export function rollup(projects: Project[], fleet: string | null = null): HTMLElement | null {
  // A folded fleet counts its Skipper too.
  const skipper = fleet ? fleetSkipperOf(fleet)?.id : null;
  const under = (s: SessionInfo) => s.id === skipper || projects.includes(place(s)?.project as Project);
  let waiting = 0;
  let working = 0;
  let unread = 0;
  for (const s of sessions.values()) {
    if (!under(s)) continue;
    if (s.state === "waiting") waiting++;
    else if (s.state === "working") working++;
    else if (isUnread(s)) unread++;
  }
  return rollCounts(waiting, working, unread);
}

/** The amber, blue and green counts of a folded row. Null when all are 0. */
export function rollCounts(waiting: number, working: number, unread = 0): HTMLElement | null {
  if (!waiting && !working && !unread) return null;
  const el = h("span", "tree-roll");
  const part = (cls: string, paths: string, n: number, label: string) => {
    if (!n) return;
    const p = h("span", cls);
    p.title = `${n} ${label}`;
    p.append(icon(paths), String(n));
    el.appendChild(p);
  };
  part("roll-waiting", BELL, waiting, "waiting");
  part("roll-working", SKIFF, working, "working");
  part("roll-unread", '<circle cx="12" cy="12" r="5" fill="currentColor"></circle>', unread, "unread");
  return el;
}

/** The project's icon, or its letters when it has none (Icon › Letters only). */
export function projectBadge(p: Project): HTMLElement {
  const box = h("span", "tree-icon");
  if (p.icon) box.appendChild(projectIcon(p, 16));
  else box.textContent = p.short;
  return box;
}

/** Open or fold a fleet or project row: `want`, or the other way. */
export function toggleRow(key: string, render: () => void, want = !isOpen(key)) {
  if (want === isOpen(key)) return;
  animateFold(key, want, () => {
    setOpen(key, want);
    render();
  });
}

/** True while a project or fleet is in the air. The tree does not rebuild under it. */
export const treeDrag = { active: false };

/** A worktree that needs no row of its own: the only one, on its usual branch. Its sessions sit under the project. */
export function bareWorktree(p: Project): Worktree | null {
  if (p.worktrees.length !== 1) return null;
  const w = p.worktrees[0];
  if (isFolder(p)) return w;
  const usual = p.default_branch ? w.branch === p.default_branch : ["main", "master"].includes(w.branch ?? "");
  return w.is_main && usual && !w.locked && !w.prunable && !removing.has(w.path) && !removeErrors.has(w.path) ? w : null;
}

/**
 * The list row for a worktree: its own row, or the project's row when it has none.
 * Opens its fleet and project. A fleet that hides its projects shows them again.
 */
export function worktreeRowKey(p: Project, w: Worktree): string {
  autoOpen(projectKey(p.name));
  if (p.fleet) autoOpen(fleetKey(p.fleet));
  if (p.fleet) setProjectsHidden(p.fleet, false);
  return bareWorktree(p) === w ? projectKey(p.name) : w.path;
}
