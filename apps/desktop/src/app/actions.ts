import { type Action } from "./keys";

import { $ } from "../ui/dom";
import { projectKey } from "../workspace/tree";
import { copy, open, reveal } from "../platform/fileActions";
import { currentRow, cycleRegion, fromProject, listItems, stepList, toMainWorktree, toProject } from "./keyboard";

import { endEntry, killGroup, newWorktree, projectMenu, projectRun, splitMenu, ungroup } from "../workspace/menus";
import { ctxMenu } from "../ui/contextMenu";
import { isRemote } from "./hosts";
import { hostCan } from "./hostCaps";
import { openRemoteNewSession } from "../workspace/remoteLaunch";
import { isFolder } from "../workspace/model";
import { addProject, launchMenu, settings } from "./panels";

import { renameListItem, startRename, startSessionRename } from "../workspace/rename";

import { FONT_DEFAULT, S, sessions } from "./state";
import { currentProject, currentWorktree, shownIds, splitFull } from "./stateQueries";

import { paneCenter } from "../terminal/terminal";

import { openFind } from "../terminal/terminalSearch";
import { closePreview, previewOnScreen } from "../terminal/peek";
import { copySelection, pasteClipboard } from "../terminal/terminalClipboard";
import { openImages } from "../terminal/images";
import { fontTarget, setFontSize, stepPaneFont } from "../terminal/terminalFont";

import { modeGate, modeKeyBlocked, toggleFocusMode, toggleMaximize } from "./modes";
import { ledgerKey } from "../workspace/ledger";
import { closePane, goBack, moveFocus, nextWaiting, removeFromGroup } from "../workspace/view";
import { switcher } from "./commandUi";
import { missionKey, missionOpen, toggleRunning, toggleSearch } from "../workspace/missionControl";
import { closeMission, goToSession, maximizeLit, toggleMission } from "../workspace/missionMotion";
import { showShortcuts } from "../ui/infoDialogs";
import { isSkiffAgent, openSkiffAgent, skiffAgentSession } from "./skiffAgent";
import { groupThemeMenu, projectThemeMenu, sessionThemeMenu } from "../appearance/themes";
import { toggleLedger } from "../workspace/ledger";
import { setSidebarView, skippersOn } from "../workspace/skippersView";
import { quitApp } from "../workspace/profile";
function openNewSession() {
  // Its key works from inside a menu, such as the worktree menu that shows it: that menu goes.
  ctxMenu.close(false);
  // The highlighted row's worktree, as its + would; else the focused one.
  const row = currentRow();
  // A remote Host has no worktrees yet: a folder on that Host, the
  // highlighted session's first, else the focused one's.
  if (isRemote()) {
    const near = sessions.get(row?.dataset.session ?? "") ?? (S.focused ? sessions.get(S.focused) : undefined);
    const b = (row ?? $("sidebar-scroll")).getBoundingClientRect();
    return openRemoteNewSession(b.left + 24, row ? b.bottom : b.top + 24, near?.cwd);
  }
  const rowPlus = row?.closest("#sidebar-scroll .wt")?.querySelector<HTMLElement>(".wt-plus");
  if (rowPlus) return rowPlus.click();
  const at = currentWorktree();
  if (!at) return;
  const plus = [...document.querySelectorAll<HTMLElement>("#sidebar-scroll .wt-plus")].find((b) => b.dataset.wt === at.w.path);
  launchMenu.open(plus ?? $("sidebar-scroll"), at.p, at.w);
}

export function runAction(a: Action) {
  const shown = S.focused && shownIds().includes(S.focused);
  // Mission control holds the keys as a dialog does. Its own key closes it; maximize goes to the lit session;
  // the palette key searches the board; the focus mode key switches Running / All; the list keys move
  // through the graph; Next waiting lights the next waiting card; the Ledger key folds the panel; the
  // menu key opens the board menu. Settings and the Skiff agent close the board first.
  if (missionOpen() && a === "maximize") return maximizeLit();
  if (missionOpen() && a === "palette") return toggleSearch();
  // The focus mode key toggles Running / All. The menu bar sends it here.
  if (missionOpen() && a === "focus-mode") return toggleRunning();
  if (missionKey(a)) return;
  // The top bar shows over the board, so its actions work there: Next waiting leaves the board on that session; Settings closes the board.
  // The Skiff agent's key leaves the board on its pane, or closes the board and starts it.
  if (missionOpen() && (a === "settings" || (a === "skiff-agent" && !skiffAgentSession()))) closeMission();
  else if (missionOpen() && a !== "mission-control" && a !== "quit" && a !== "next-waiting" && a !== "skiff-agent") return;
  // The Ledger takes the list keys while it has the keys, and Ctrl+Shift+Right into it. Focus mode keeps it.
  if (ledgerKey(a)) return;
  // The native menu runs its accelerators through here, not through the key handler.
  if (modeKeyBlocked(a) || modeGate(a)) return;
  // The open palette holds every other action; its own key closes it.
  if (switcher.isOpen() && a !== "palette") return;
  // Settings holds the keys as a dialog does. The list keys step its sections.
  if (settings.isOpen && (settings.runKey(a) || (a !== "settings" && a !== "quit"))) return;
  switch (a) {
    case "palette": return switcher.toggle();
    case "new-session": return openNewSession();
    case "new-worktree": {
      // The Host makes it, when it offers worktrees.
      if (!hostCan("worktrees")) return;
      const p = currentProject();
      return p && !isFolder(p) ? newWorktree(p) : undefined;
    }
    case "add-project": return void addProject.open();
    case "project-menu": {
      // The highlighted row's menu, as a right-click opens it: a worktree, a
      // group, a session, or a Changes or Files row. The
      // highlight, not DOM focus: a picked group can be current while focus
      // rests elsewhere. With no row, the project menu.
      const row = currentRow();
      if (!row) return openProjectMenu();
      const b = row.getBoundingClientRect();
      row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: b.left + 24, clientY: b.bottom }));
      return;
    }
    case "project-folder":
    case "project-copy-path": {
      // On a file or change row, the key acts on that file.
      const path = rowPath();
      // A remote Host's paths are copied as text, never opened on this Mac.
      if (isRemote() && a !== "project-copy-path") return;
      if (path) return a === "project-copy-path" ? copy(path.path) : path.dir ? open(path.path) : reveal(path.path);
      const p = currentProject();
      return p ? projectRun(p)[a]() : undefined;
    }
    case "project-color":
    case "project-changes":
    case "project-files": {
      // The Host keeps these settings, and lists the Files tree (`files@1`).
      if (!hostCan("project_config") || (a === "project-files" && !hostCan("files"))) return;
      const p = currentProject();
      return p ? projectRun(p)[a]() : undefined;
    }
    case "split-right":
    case "split-down": {
      if (!S.focused || !shown || splitFull() || isSkiffAgent(S.focused)) return;
      const [x, y] = paneCenter();
      return splitMenu(S.focused, a === "split-right" ? "row" : "col", x, y);
    }
    // Over a file or diff, the key closes the preview, even when a terminal tool has the keys.
    case "close-pane": return previewOnScreen() ? closePreview() : closeKey(!!shown);
    case "next-waiting": return goToSession(nextWaiting());
    case "skiff-agent": {
      const s = skiffAgentSession();
      if (s && missionOpen()) return goToSession(s.id);
      // Again from its pane: the side panel switches between the tree and the Skippers list. The pane keeps focus.
      if (isSkiffAgent(S.focused)) return setSidebarView(skippersOn() ? "tree" : "skippers");
      return openSkiffAgent();
    }
    case "ledger": return toggleLedger();
    case "sidebar-view": return setSidebarView(skippersOn() ? "tree" : "skippers");
    case "mission-control": return toggleMission();
    case "back": return goBack();
    case "rename": {
      // In the list, the row with the keys: arrowing to a row does not open it at once.
      if (renameListItem(document.activeElement as HTMLElement | null)) return;
      const pg = S.groups.find((x) => x.id === S.groupPicked);
      return pg ? startRename(pg) : S.focused && !isSkiffAgent(S.focused) ? startSessionRename(S.focused) : undefined;
    }
    case "theme": return openTheme();
    case "end-session": return endCurrentSession();
    case "copy": return void copySelection();
    case "paste": return void pasteClipboard();
    case "images": return openImages();
    case "images-gallery": return void settings.open("images");
    case "find": return openFind();
    case "font-bigger": return fontKey(1);
    case "font-smaller": return fontKey(-1);
    case "font-reset": return fontKey(0);
    case "app-font-bigger": return setFontSize(S.fontSize + 1);
    case "app-font-smaller": return setFontSize(S.fontSize - 1);
    case "app-font-reset": return setFontSize(FONT_DEFAULT);
    case "settings": return void settings.open();
    case "quit": return quitApp();
    case "focus-left": return moveFocus("left");
    case "focus-right": return moveFocus("right");
    case "focus-up": return moveFocus("up");
    case "focus-down": return moveFocus("down");
    case "region-next": return cycleRegion(1);
    case "region-prev": return cycleRegion(-1);
    case "session-next": return stepList(1);
    case "session-prev": return stepList(-1);
    // The sidebar is the combined rail: its rows are already the project list.
    case "list-project": return void (document.activeElement?.closest("#sidebar") || toProject());
    case "list-back": return fromProject();
    case "list-main": return toMainWorktree();
    case "shortcuts": return showShortcuts();
    case "focus-mode": return toggleFocusMode();
    case "maximize": return toggleMaximize();
  }
}

/**
 * The close key picks its target as Rename does. A session row leaves its
 * group, and a group row ungroups. Else the focused pane leaves its group;
 * a single pane has none, so nothing happens. Sessions keep running in every case.
 */
function closeKey(shown: boolean) {
  const el = document.activeElement as HTMLElement | null;
  const row = el && listItems().includes(el) ? el : undefined;
  if (row?.dataset.session) return removeFromGroup(row.dataset.session);
  const gid = row ? row.dataset.group : S.groupPicked;
  const g = gid ? S.groups.find((x) => x.id === gid) : undefined;
  if (g) return ungroup(g);
  if (!row && shown) closePane(S.focused!);
}

/**
 * The theme key acts on the highlighted row, as the menu key does: a session
 * row gets its own theme and a group row every terminal in it. On a project, worktree, Changes or Files row, the project theme. With no row,
 * the focused terminal.
 */
function openTheme() {
  const project = () => {
    const p = currentProject();
    // A project theme is its Host's setting: it changes when that Host takes project changes.
    if (p && hostCan("project_config")) void projectThemeMenu(p);
  };
  const row = currentRow();
  const sid = row ? row.dataset.session : S.focused;
  const s = sid ? sessions.get(sid) : undefined;
  if (s) return void sessionThemeMenu(s);
  const g = row?.dataset.group ? S.groups.find((x) => x.id === row.dataset.group) : undefined;
  if (g) return void groupThemeMenu(g);
  project();
}

/** End the highlighted session, or the focused pane's session, as its menu item does. */
function endCurrentSession() {
  const row = currentRow();
  // On a group row the key acts on the group: all its sessions.
  if (row?.dataset.group) return void killGroup(row.dataset.group);
  const session = sessions.get(row?.dataset.session ?? S.focused ?? "");
  const entry = session && endEntry(session);
  if (entry && "run" in entry) entry.run?.();
}

/** The selected project's menu, under its row in the tree, as the menu key opens a row's. */
export function openProjectMenu() {
  const p = currentProject();
  if (!p) return;
  const row = listItems().find((el) => el.dataset.key === projectKey(p.name));
  const r = (row ?? $("sidebar-scroll")).getBoundingClientRect();
  projectMenu(p, r.left + 24, row ? r.bottom : r.top);
}

/** The file of the highlighted Files or Changes row, if that is the row. */
function rowPath(): { path: string; dir: boolean } | null {
  const row = currentRow();
  const m = row?.dataset.key?.match(/^(file|change):(.*)$/);
  return m ? { path: m[2], dir: row!.classList.contains("dir") } : null;
}

/** On a highlighted session, that pane's text. Elsewhere, the app's. */
function fontKey(step: -1 | 0 | 1) {
  const id = fontTarget();
  if (id) return stepPaneFont(id, step);
  setFontSize(step ? S.fontSize + step : FONT_DEFAULT);
}
