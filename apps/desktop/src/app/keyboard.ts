/** App keys, Tab order, F6 regions, arrows in the lists. */
import { isSkiffAgent, projectSkipperOf } from "./skiffAgent";
import { pickSkipperRow, skippersOn } from "../workspace/skippersView";
import { type Action, actionFor, isMac } from "./keys";
import { runAction } from "./actionDispatch";
import { switcher } from "./commandUi";
import { modeKeyBlocked } from "./modes";
import { $, modalOpen } from "../ui/dom";
import { host } from "../terminal/terminalHost";
import { deleteKeyMenu } from "../workspace/menus";
import { ctxMenu } from "../ui/contextMenu";
import { render } from "./render";
import { launchMenu } from "./panels";
import { closeEntries } from "../workspace/projectClose";
import { renameListItem } from "../workspace/rename";
import { clearSelection, extendSelection, keepRow, splitSelection } from "../workspace/selection";
import { collapsed, S, selectedWorktree, sessions } from "./state";

import { panes } from "../terminal/terminalState";
import { revealSession, selectProject, showGroup } from "../workspace/view";
import { closePreview, enterPreview, giveKeys, peekBlocksAction, previewOnScreen, startPreview } from "../terminal/peek";
import { rowActs } from "../workspace/rowActs";
import { worktreeLines } from "../workspace/sidebarLines";
import { ledgerKey } from "../workspace/ledger";
import { animateFold } from "../ui/fold";
import { fleetKey, isOpen, projectKey as projectRowKey, toggleRow, visibleProjects } from "../workspace/tree";
import { scopeFleet } from "../workspace/model";
import { missionOpen, pickSkipper } from "../workspace/missionControl";

/** Ctrl+Shift+1..9 picks a project row the tree shows (Command+Option+1..9 on macOS); in Mission control, a Skipper card. Not in the keymap: it is a range, not one key. */
function projectKey(e: KeyboardEvent): number | null {
  if (isMac ? !e.metaKey || !e.altKey || e.shiftKey || e.ctrlKey : !e.ctrlKey || !e.shiftKey || e.altKey || e.metaKey) return null;
  const m = /^Digit([1-9])$/.exec(e.code);
  return m ? Number(m[1]) : null;
}

/** Keys the app owns even while a terminal has focus. Everything else goes to the PTY. */
export function appKey(e: KeyboardEvent): Action | number | null {
  return actionFor(e) ?? projectKey(e);
}

window.addEventListener("keydown", (e) => {
  // The open palette runs no app key but its own, which closes it. The key still
  // reaches the palette input, so Ctrl+Shift+Enter picks and Ctrl+Shift+Up moves.
  if (switcher.isOpen()) {
    const key = appKey(e);
    if (key === null) return;
    e.preventDefault();
    if (key === "palette") {
      e.stopPropagation();
      runAction(key);
    }
    return;
  }
  // Mission control gives the number keys to its Skipper cards. The tree's projects wait until it closes.
  if (missionOpen()) {
    const n = projectKey(e);
    if (n !== null) {
      e.preventDefault();
      e.stopPropagation();
      pickSkipper(n);
      return;
    }
  }
  // An open menu or dialog keeps the keys until it closes. Holding Ctrl+Shift
  // from the shortcut that opened it, then pressing Enter, picks the item
  // instead of running Maximize.
  if (modalOpen(false)) return;
  const key = appKey(e);
  if (key === null) return;
  if (peekBlocksAction(key) && !(e.target as Element).closest?.("#sidebar-scroll")) return;
  e.preventDefault();
  e.stopPropagation();
  // The Ledger's lit row takes Ctrl+Shift+Enter before the tree row can block it.
  if (key === "maximize" && ledgerKey(key)) return;
  if (typeof key === "string" && modeKeyBlocked(key)) return;
  if (typeof key === "number") {
    pickProject(key);
  } else {
    runAction(key);
  }
}, true);

// Focus rings show only while the keyboard is in use (see body.kbd in styles.css).
const NAV_KEYS = new Set(["Tab", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "F6", "Home", "End"]);

window.addEventListener("keydown", (e) => {
  if (NAV_KEYS.has(e.key)) document.body.classList.add("kbd");
}, true);
window.addEventListener("mousedown", () => document.body.classList.remove("kbd"), true);

export const itemKey = (el: HTMLElement) => el.dataset.session ?? el.dataset.wt ?? el.dataset.group ?? el.dataset.key ?? "";

export function listItems(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>("#sidebar-scroll .fleet-pick, #sidebar-scroll .project-pick, #sidebar-scroll .wt-pick, #sidebar-scroll .group-pick, #sidebar-scroll button.session-row, #sidebar-scroll .files-head, #sidebar-scroll .file-row")];
}

/**
 * The row with this key. A Skipper (or the Skiff agent) in a group has two
 * rows with one key, its own row and its line in the group: the one in the
 * group `S.roveAt` names, else its own row.
 */
export function findRow(items: HTMLElement[], key: string | null | undefined): HTMLElement | undefined {
  if (!key) return undefined;
  const rows = items.filter((x) => itemKey(x) === key);
  if (rows.length < 2) return rows[0];
  const group = S.roveAt?.key === key ? S.roveAt.group : null;
  return rows.find((x) => (x.dataset.inGroup ?? null) === group) ?? rows[0];
}

/** Makes this row the current one: its key, and which of its rows when it has two. */
export function roveTo(el: HTMLElement) {
  const key = itemKey(el);
  S.roveKey = key;
  S.roveAt = { key, group: el.dataset.inGroup ?? null };
}

/** Where we are in the list: the last-roved row, else the picked group, else the
 *  session on screen. One row at most, whatever its kind. */
export function currentRow(items = listItems()): HTMLElement | undefined {
  const roved = findRow(items, S.roveKey);
  if (roved) return roved;
  if (S.groupPicked) return items.find((x) => x.dataset.group === S.groupPicked);
  return findRow(items, S.focused);
}

/** The roving tab stop: the current row, else the first row. */
function roveTarget(): HTMLElement | undefined {
  return currentRow() ?? listItems()[0];
}

/** Moves the one highlight to the current row. The row of the pane on screen, when it is another one, carries `shown`. */
export function markCurrent() {
  const items = listItems();
  // A row in rename is its name field, not a list item. It stays the current row until the rename ends.
  const editing = document.querySelector<HTMLElement>("#sidebar-scroll .renaming");
  const cur = editing ? undefined : currentRow(items);
  for (const x of items) x.classList.toggle("current", x === cur);
  editing?.classList.add("current");
  for (const x of document.querySelectorAll<HTMLElement>("#sidebar-scroll [data-session]")) x.classList.toggle("shown", !!S.focused && x.dataset.session === S.focused && x !== cur);
  // The worktree marker goes with it: on the project row, no worktree is marked.
  const mark = editing ?? cur;
  if (mark) for (const w of document.querySelectorAll("#sidebar-scroll .wt")) w.classList.toggle("selected", w.contains(mark));
}

/**
 * One Tab stop per region: the session list, the terminal.
 * Everything else, the + buttons too, is reached by mouse, key, or palette.
 */
export function applyTabOrder() {
  const item = roveTarget();
  for (const el of document.querySelectorAll<HTMLElement>("#app button, #app [tabindex]")) {
    // Settings is its own screen with its own Tab order; xterm keeps its textarea.
    if (el.closest("#settings, .xterm")) continue;
    // Only a change is written: a write of the same value still restyles.
    if (el.tabIndex !== -1 && el !== item) el.tabIndex = -1;
  }
  if (item && item.tabIndex !== 0) item.tabIndex = 0;
  markCurrent();
}

function regionOf(el: Element | null): number {
  if (!el) return -1;
  if (el.closest("#sidebar-scroll")) return 0;
  if (host.contains(el) || el.closest(".peek")) return 1;
  return -1;
}

/** F6 and Shift+F6: the session list, then the terminal. */
export function cycleRegion(dir: 1 | -1) {
  document.body.classList.add("kbd");
  const stops: (() => boolean)[] = [
    () => focusEl(roveTarget() ?? null),
    () => {
      if (giveKeys()) return true;
      const t = S.focused ? panes.get(S.focused)?.term : undefined;
      if (!t) return false;
      t.focus();
      return true;
    },
  ];
  let i = regionOf(document.activeElement);
  for (let n = 0; n < stops.length; n++) {
    i = (i + dir + stops.length) % stops.length;
    if (stops[i]()) return;
  }
}

function focusEl(el: HTMLElement | null): boolean {
  if (!el) return false;
  el.focus();
  return document.activeElement === el;
}

/** Shows a session or picks a group. The row keeps the keys, as a click does. */
function openRow(el: HTMLElement) {
  const sid = el.dataset.session;
  if (sid && (S.focused !== sid || S.groupPicked)) {
    clearSelection();
    revealSession(sid);
    requestAnimationFrame(() => keepRow(sid));
  } else if (el.classList.contains("group-pick") && el.dataset.group && (el.dataset.group !== S.activeGroup || el.dataset.group !== S.groupPicked)) {
    showGroup(el.dataset.group, true);
  }
}

/**
 * Ctrl+Shift+Down/Up: the next or previous row in the list. A session opens. A group
 * opens with its row keeping the keys. A changed file loads its diff, and a file
 * its preview; the preview takes the keys. Changes, Files, folders, worktrees and
 * other headers take focus.
 */
export function stepList(dir: 1 | -1) {
  const rows = listItems();
  if (!rows.length) return;
  // Moving on leaves any open menu behind.
  ctxMenu.close(false);
  launchMenu.close(false);
  // From the highlighted row, wherever the keys are: a row, a menu, a diff, a terminal.
  const cur = currentRow(rows);
  const i = cur ? rows.indexOf(cur) : -1;
  landOn(rows[i < 0 ? (dir > 0 ? 0 : rows.length - 1) : (i + dir + rows.length) % rows.length]);
}

/**
 * The one way the current row changes: a click or Ctrl+Shift+Up/Down. The
 * highlight moves, and the main area shows the row's preview, or the panes.
 * `keys`: the preview takes the keys, as a session's pane does.
 */
export function select(key: string, keys: boolean, fresh = false) {
  S.roveKey = key;
  // The highlight and the one Tab stop move together.
  applyTabOrder();
  startPreview(key, keys, fresh);
}

/** Ctrl+Shift+Home: the current project's main worktree becomes the current row. */
export function toMainWorktree() {
  const p = S.projects.find((x) => x.name === S.selectedProject);
  const w = p?.worktrees.find((x) => x.is_main) ?? p?.worktrees[0];
  if (!w) return;
  const row = listItems().find((el) => el.dataset.wt === w.path);
  if (!row) return;
  ctxMenu.close(false);
  launchMenu.close(false);
  landOn(row);
}

/** Makes a row the current one. A session opens. A group opens with its row keeping
 *  the keys. A file or change shows its preview, which takes the keys. Changes,
 *  Files and folders show theirs with the row keeping the keys. Other rows take focus. */
function landOn(next: HTMLElement) {
  const key = itemKey(next);
  document.body.classList.add("kbd");
  if (next.dataset.session) {
    roveTo(next);
    revealSession(next.dataset.session);
  } else if (next.dataset.group) {
    roveTo(next);
    showGroup(next.dataset.group, true);
  } else if (rowActs(key)?.preview && !rowActs(key)?.fold) {
    select(key, true);
  } else {
    // A row that opens and closes (Changes, Files, a folder) keeps the keys, so
    // Space and the arrows fold it. Its preview still shows; Enter goes into it.
    select(key, false);
    next.focus();
  }
}

/**
 * Ctrl+Shift+1..9: the project row with that number, counted down the tree.
 * The panes show its Skipper, else the first line under it (its first group,
 * else its first session), not the last one in use there. The keys go to the
 * project row. In the Skippers list the number counts the fleet and project
 * Skippers there, not Admiral, and opens that Skipper as a click does.
 */
function pickProject(n: number) {
  if (skippersOn()) return pickSkipperRow(n);
  const p = visibleProjects()[n - 1];
  if (!p) return;
  selectProject(p.name);
  const skipper = projectSkipperOf(p.name);
  const w = p.worktrees.find((x) => x.path === selectedWorktree.get(p.name));
  const first = w ? worktreeLines(w).find((l) => !l.pointer) : undefined;
  if (skipper) revealSession(skipper.id);
  else if (first?.group) showGroup(first.group.id);
  else if (first?.session) revealSession(first.session.id);
  toProject();
  listItems().find((el) => el.dataset.key === projectRowKey(p.name))?.scrollIntoView({ block: "nearest" });
}

/** Ctrl+Shift+Left: the keys go to the current project's row in the tree. The panes stay as they are. */
export function toProject() {
  ctxMenu.close(false);
  launchMenu.close(false);
  clearSelection();
  S.groupPicked = null;
  // The Skiff agent has no project: its row stays the current one.
  if (isSkiffAgent(S.focused) && currentRow()?.dataset.session === S.focused) return;
  // A fleet Skipper has no project: the keys go to its fleet's row.
  const fleet = scopeFleet(S.focused ? sessions.get(S.focused)?.skipper : null);
  const name = S.selectedProject;
  const key = fleet !== null ? fleetKey(fleet) : name ? projectRowKey(name) : "";
  const row = listItems().find((el) => el.dataset.key === key);
  if (!row) return;
  render();
  landOn(listItems().find((el) => el.dataset.key === row.dataset.key) ?? row);
}

/** Ctrl+Shift+Right: from a project or fleet row back to the session on screen. */
export function fromProject() {
  const el = document.activeElement as HTMLElement | null;
  if (el && !el.classList.contains("project-pick") && !el.classList.contains("fleet-pick")) return;
  const row = S.focused ? listItems().find((x) => x.dataset.session === S.focused) : undefined;
  if (row) landOn(row);
}

/** A Changes or Files header, or a row under one. Not a session, group or worktree. */
const isSection = (el: HTMLElement) =>
  !!el.dataset.key && !el.dataset.session && !el.dataset.wt && !el.dataset.group && !el.classList.contains("project-pick") && !el.classList.contains("fleet-pick");

/** Arrows inside the session list; Enter, Left/Right, Menu key. */
$("sidebar-scroll").addEventListener("keydown", (e) => {
  const el = document.activeElement as HTMLElement | null;
  if (!el || el.tagName === "INPUT") return;
  const items = listItems();
  const i = items.indexOf(el);
  if (i < 0) return;
  const wt = el.dataset.wt;
  if (e.shiftKey && (e.key === "ArrowDown" || e.key === "ArrowUp") && el.dataset.session) extendSelection(e.key === "ArrowDown" ? 1 : -1, el);
  else if (e.key === "Enter" && S.selection.length >= 2) splitSelection();
  else if (e.key === "Escape" && S.selection.length) {
    const key = itemKey(el);
    clearSelection();
    render();
    keepRow(key);
  } else if (e.key === "Escape" && !e.shiftKey && previewOnScreen()) closePreview();
  else if ((el.classList.contains("project-pick") || el.classList.contains("fleet-pick")) && (e.key === "Enter" || e.key === " " || e.key === "ArrowLeft" || e.key === "ArrowRight")) {
    // A project with nothing under it does not open or fold.
    if (!el.hasAttribute("aria-expanded")) return e.preventDefault();
    // A project or fleet row: Enter or Space opens or folds it, Right opens, Left folds.
    // A project's menu is the menu key or Ctrl+Shift+M, as on every row.
    const key = el.dataset.key!;
    S.roveKey = key;
    toggleRow(key, () => {
      render();
      roveTarget()?.focus();
    }, e.key === "ArrowRight" || (e.key !== "ArrowLeft" && !isOpen(key)));
  } else if (el.classList.contains("project-pick") && (e.key === "Delete" || e.key === "Backspace") && !e.ctrlKey && !e.altKey && !e.metaKey) {
    const p = S.projects.find((x) => `project:${x.name}` === el.dataset.key);
    const r = el.getBoundingClientRect();
    if (p) ctxMenu.open(r.left + 24, r.bottom, p.name, closeEntries(p));
  } else if (el.dataset.key === "skiff-agent" && e.key === "Enter") {
    // The Skiff agent row with no session: Enter starts it, as a click does.
    el.click();
  } else if (isSection(el) && e.key === "Enter" && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
    // Shift+Enter on a change opens the file itself.
    rowActs(itemKey(el))?.open?.();
  } else if (isSection(el) && e.key === "Enter" && !e.ctrlKey && !e.altKey && !e.metaKey) {
    // Enter goes into what the row shows: a tool or diff takes the keys, a
    // card runs its button. A row with no preview opens or closes.
    const key = itemKey(el);
    if (rowActs(key)?.preview) {
      select(key, false);
      enterPreview();
    } else rowActs(key)?.fold?.();
  } else if (isSection(el) && e.key === " ") {
    // Space opens or closes a Changes, Files or folder row.
    rowActs(itemKey(el))?.fold?.();
  } else if (isSection(el) && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
    // Right opens, Left closes. On a file row they do nothing.
    rowActs(itemKey(el))?.fold?.(e.key === "ArrowRight");
  } else if (!el.dataset.session && ["ArrowUp", "ArrowDown", "Home", "End"].includes(e.key)) {
    // Rows move with Ctrl+Shift+Up/Down only. On a session row these keys go
    // to its terminal below; elsewhere they do nothing, not even scroll.
  } else if ((e.key === " " || e.key === "ArrowLeft" || e.key === "ArrowRight") && wt && !el.hasAttribute("aria-expanded")) {
    // A worktree with nothing under it does not open or fold.
  } else if ((e.key === "Enter" || e.key === " " || e.key === "ArrowLeft" || e.key === "ArrowRight") && wt && el.hasAttribute("aria-expanded")) {
    // A worktree: Enter or Space opens or closes it, Right opens, Left closes.
    // Its + (start a session) is Ctrl+Shift+T, or New session in its menu.
    const close = e.key === "ArrowLeft" || ((e.key === "Enter" || e.key === " ") && !collapsed.has(wt));
    S.roveKey = wt;
    if (close === collapsed.has(wt)) return e.preventDefault();
    animateFold(wt, !close, () => {
      if (close) collapsed.add(wt);
      else collapsed.delete(wt);
      render();
      roveTarget()?.focus();
    });
  } else if (e.key === "Enter") {
    // Enter opens the row right away, instead of waiting out the pause.
    openRow(el);
  } else if (e.key === "F2" && !e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && renameListItem(el)) {
    // F2 renames the row in place, as in a file manager.
  } else if ((e.key === "Delete" || e.key === "Backspace") && !e.ctrlKey && !e.altKey && !e.metaKey && (el.dataset.session || el.dataset.group)) {
    deleteKeyMenu(el);
  } else if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
    const r = el.getBoundingClientRect();
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.left + 24, clientY: r.bottom }));
  } else if (!(el.dataset.session && passToTerminal(e, el.dataset.session))) return;
  e.preventDefault();
  e.stopPropagation();
});

/**
 * A clicked session keeps the keys on its row. A key the list does not use
 * moves them to the open terminal, and the terminal gets that key too.
 */
const ARROWS: Record<string, string> = { ArrowUp: "A", ArrowDown: "B", ArrowRight: "C", ArrowLeft: "D" };

function passToTerminal(e: KeyboardEvent, id: string): boolean {
  const term = S.focused === id ? panes.get(id)?.term : undefined;
  if (!term || e.altKey || e.metaKey) return false;
  let data = "";
  const arrow = ARROWS[e.key];
  if (arrow && !e.ctrlKey && !e.shiftKey) data = (term.modes.applicationCursorKeysMode ? "\x1bO" : "\x1b[") + arrow;
  else if (e.key === "Enter") data = "\r";
  else if (e.key.length === 1 && !e.ctrlKey) data = e.key;
  else if (e.ctrlKey && /^[a-z]$/i.test(e.key)) data = String.fromCharCode(e.key.toUpperCase().charCodeAt(0) - 64);
  else if (e.key !== "Escape") return false;
  term.focus();
  if (data) term.input(data);
  return true;
}

// A render can drop the element that had the keys. Focus then falls to the
// body and typed keys go nowhere, while a pane still shows as focused.
window.addEventListener("keydown", (e) => {
  const a = document.activeElement;
  if (e.defaultPrevented || (a && a !== document.body) || modalOpen() || !S.focused) return;
  if (!passToTerminal(e, S.focused)) return;
  e.preventDefault();
  e.stopPropagation();
});

// A pressed row is where we are. Not on focus: a render puts focus back on the
// row that had it, which may no longer be the current one.
// A right-click on a file, folder or change selects it too, so its menu acts on what shows.
$("sidebar-scroll").addEventListener("mousedown", (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>(".wt-pick, .group-pick, button.session-row, .files-head, .file-row");
  if (!el) return;
  const key = itemKey(el);
  const preview = !!rowActs(key)?.preview;
  if (e.button !== 0 && !(e.button === 2 && preview)) return;
  if (preview && (e.ctrlKey || e.metaKey || e.shiftKey)) return;
  select(key, false);
  // WebKit does not focus a clicked button. The row keeps the keys for Enter and Space.
  if (preview) el.focus();
});

// Tab from the list goes into a preview on screen, not to a pane under it.
window.addEventListener("keydown", (e) => {
  if (e.key !== "Tab" || e.shiftKey || e.ctrlKey || e.altKey || e.metaKey || modalOpen()) return;
  if (!(e.target as Element).closest?.("#sidebar-scroll") || !previewOnScreen()) return;
  e.preventDefault();
  giveKeys();
}, true);
