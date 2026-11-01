import { runAction, openProjectMenu } from "./actions";
import { settings } from "./panels";
import { showAbout } from "../ui/infoDialogs";
import { checkForUpdates } from "./updater";
import { isSkiffAgent, skiffAgentFolder, skiffAgentName, skiffAgentSession } from "./skiffAgent";

/** One command list for the menu bar and the palette; key actions. */
import { type Action, isMac, keyLabel, menuAccel } from "./keys";
import { agentName, branchName, bySessionPriority, isFolder, taskTitle, isProjectSkipper } from "../workspace/model";
import { createSwitcher, type SwitchItem } from "../ui/switcher";
import { type MessageKey, t, tn } from "../i18n";

import { invoke } from "@tauri-apps/api/core";
import { hostCan } from "./hostCaps";
import { isRemote } from "./hosts";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "../platform/os";

import { showError, showNotice } from "../ui/alerts";
import { confirmAction } from "../ui/confirm";

import { copyReport, hasReport, startTrace, stopTrace, tracing } from "../diagnostics/latency";
import { groupCloseEntries } from "../workspace/menus";

import { closedProjectsMenu, closeProject, openClosedProject, removeProject } from "../workspace/projectClose";

import { scheduleRender } from "./render";
import { S, sessions } from "./state";
import { accent, activeGroupObj, currentProject, currentWorktree, inShownGroup, place, shownIds, splitFull, worktreeSessions } from "./stateQueries";

import { sessionThemeMenu } from "../appearance/themes";
import { modeKeyBlocked } from "./modes";
import { ledgerFolded, ledgerShown } from "../workspace/ledger";
import { skippersOn } from "../workspace/skippersView";
import { imageCount } from "../terminal/images";
import {
  closeWorkspace, editWorkspace, keepWorkspace, ledgerAllWork, newWorkspace, openWorkspace, recentWorkspacesMenu, reloadWorkspace,
  saveWorkspace, saveWorkspaceAs, setLedgerAllWork, workspaceNotice, workspaceOpen,
} from "../workspace/profile";
import { refocusTerminal, revealSession, selectWorktree } from "../workspace/view";
import { step } from "../diagnostics/stalls";

interface Cmd {
  section: "file" | "edit" | "view" | "help";
  /** The catalog key of its words; also its name in BREAK_BEFORE and APP_MENU. */
  id: string;
  label: string;
  key: string;
  /** The keymap action, so the menu bar can show and bind its key. */
  action?: Action;
  run: () => void;
  /** Shown greyed out in the menu, left out of the palette. */
  off?: boolean;
}

/** A command as listed: its words come from its id unless it sets its own. */
type Draft = Omit<Cmd, "id" | "label"> & ({ id: MessageKey; label?: string } | { id: string; label: string });

function commands(): Cmd[] {
  return drafts().map((c) => ({ ...c, label: c.label ?? t(c.id as MessageKey) }));
}

function drafts(): Draft[] {
  const s = S.focused ? sessions.get(S.focused) : undefined;
  const shown = !!(S.focused && shownIds().includes(S.focused));
  const g = activeGroupObj();
  const k = (a: Action) => keyLabel(a);
  const act = (a: Action) => () => runAction(a);
  const ws = workspaceNotice();
  return [
    { section: "file", id: "cmd.newSession", key: k("new-session"), action: "new-session", run: act("new-session"), off: (!currentWorktree() && !isRemote()) || !hostCan("session_lifecycle") },
    { section: "file", id: "cmd.newWorktree", key: k("new-worktree"), action: "new-worktree", run: act("new-worktree"), off: !currentProject() || isFolder(currentProject()!) },
    { section: "file", id: "cmd.addProject", key: k("add-project"), action: "add-project", run: act("add-project") },
    { section: "file", id: "cmd.openClosedProject", key: "", run: openClosedMenu, off: !S.closedProjects.length },
    { section: "file", id: "cmd.projectMenu", key: "", run: openProjectMenu, off: !currentProject() },
    { section: "file", id: "cmd.closeProject", key: "", run: () => withProject(closeProject), off: !currentProject() },
    { section: "file", id: "cmd.removeProject", key: "", run: () => withProject(removeProject), off: !currentProject() },
    { section: "file", id: "cmd.newWorkspace", key: "", run: newWorkspace },
    { section: "file", id: "cmd.openWorkspace", key: "", run: () => void openWorkspace() },
    { section: "file", id: "cmd.openRecentWorkspace", key: "", run: recentWorkspacesMenu },
    { section: "file", id: "cmd.editWorkspace", key: "", run: editWorkspace, off: !workspaceOpen() },
    { section: "file", id: "cmd.saveWorkspace", key: "", run: saveWorkspace, off: !workspaceOpen() },
    { section: "file", id: "cmd.saveWorkspaceAs", key: "", run: saveWorkspaceAs, off: !workspaceOpen() },
    { section: "file", id: "cmd.closeWorkspace", key: "", run: closeWorkspace, off: !workspaceOpen() },
    // While the file changed on disk or names something that is gone: the way to answer it.
    { section: "file", id: "cmd.reloadWorkspace", key: "", run: reloadWorkspace, off: ws.disk !== "changed" },
    { section: "file", id: "cmd.keepWorkspace", key: "", run: keepWorkspace, off: !ws.disk },
    { section: "file", id: "cmd.repairWorkspace", key: "", run: editWorkspace, off: !ws.missing },
    { section: "file", id: "cmd.settings", key: k("settings"), action: "settings", run: act("settings") },
    { section: "file", id: "cmd.quit", key: k("quit"), action: "quit", run: act("quit") },
    // In Settings, they act on the text field with the keys.
    { section: "edit", id: "cmd.copy", key: k("copy"), action: "copy", run: act("copy"), off: !s && !settings.isOpen },
    { section: "edit", id: "cmd.paste", key: k("paste"), action: "paste", run: act("paste"), off: !s && !settings.isOpen },
    { section: "edit", id: "cmd.images", key: k("images"), action: "images", run: act("images"), off: !imageCount() || !s },
    { section: "edit", id: "cmd.imagesGallery", key: k("images-gallery"), action: "images-gallery", run: act("images-gallery") },
    { section: "edit", id: "cmd.find", key: k("find"), action: "find", run: act("find"), off: !shown },
    { section: "edit", id: "cmd.rename", key: k("rename"), action: "rename", run: act("rename"), off: !s || isSkiffAgent(s.id) || isProjectSkipper(s) || !hostCan("session_lifecycle") },
    { section: "edit", id: "cmd.sessionTheme", key: k("theme"), run: () => s && void sessionThemeMenu(s), off: !s || !hostCan("session_lifecycle") },
    {
      section: "edit", id: "cmd.endSession", key: k("end-session"), action: "end-session", off: !s || !hostCan("session_lifecycle"),
      run: act("end-session"),
    },
    { section: "view", id: "cmd.palette", key: k("palette"), action: "palette", run: act("palette") },
    { section: "view", id: "cmd.sessionNext", key: k("session-next"), action: "session-next", run: act("session-next") },
    { section: "view", id: "cmd.sessionPrev", key: k("session-prev"), action: "session-prev", run: act("session-prev") },
    { section: "view", id: "cmd.listMain", key: k("list-main"), action: "list-main", run: act("list-main") },
    { section: "view", id: "cmd.splitRight", key: k("split-right"), action: "split-right", run: act("split-right"), off: !shown || splitFull() || isSkiffAgent(S.focused) || !hostCan("session_lifecycle") },
    { section: "view", id: "cmd.splitDown", key: k("split-down"), action: "split-down", run: act("split-down"), off: !shown || splitFull() || isSkiffAgent(S.focused) || !hostCan("session_lifecycle") },
    { section: "view", id: "cmd.removeFromGroup", key: k("close-pane"), action: "close-pane", run: act("close-pane"), off: !(S.focused && inShownGroup(S.focused)) || !hostCan("session_groups") },
    ...groupCloseEntries(g).map((entry): Draft => ({
      section: "view", id: entry.label, label: entry.label, key: "", run: () => entry.run?.(), off: entry.disabled,
    })),
    { section: "view", id: "cmd.missionControl", key: k("mission-control"), action: "mission-control", run: act("mission-control"), off: !hostCan("session_screen") },
    { section: "view", id: "cmd.nextWaiting", key: k("next-waiting"), action: "next-waiting", run: act("next-waiting") },
    {
      section: "view", id: "cmd.showSkiffAgent", label: t(skiffAgentSession() ? "cmd.showSkiffAgent" : "cmd.startSkiffAgent", { name: skiffAgentName() }), key: k("skiff-agent"), action: "skiff-agent",
      run: act("skiff-agent"), off: !skiffAgentFolder(),
    },
    { section: "view", id: "cmd.showTree", label: t(skippersOn() ? "cmd.showTree" : "cmd.showSkippers"), key: k("sidebar-view"), action: "sidebar-view", run: act("sidebar-view") },
    { section: "view", id: "cmd.showLedger", label: t(ledgerFolded() ? "cmd.showLedger" : "cmd.hideLedger"), key: k("ledger"), action: "ledger", run: act("ledger"), off: !ledgerShown() },
    {
      section: "view", id: "cmd.ledgerAllWork", label: t(ledgerAllWork() ? "cmd.ledgerWorkspaceWork" : "cmd.ledgerAllWork"), key: "",
      run: () => setLedgerAllWork(!ledgerAllWork()), off: !workspaceOpen(),
    },
    { section: "view", id: "cmd.back", key: k("back"), action: "back", run: act("back"), off: !S.previous },
    { section: "view", id: "cmd.focusMode", label: t(S.focusMode ? "cmd.leaveFocusMode" : "cmd.focusMode"), key: k("focus-mode"), action: "focus-mode", run: act("focus-mode"), off: (!S.focusMode && !shown) || modeKeyBlocked("focus-mode") },
    { section: "view", id: "cmd.maximize", label: t(S.maximized ? "cmd.restore" : "cmd.maximize"), key: k("maximize"), action: "maximize", run: act("maximize"), off: (!S.maximized && !shown) || modeKeyBlocked("maximize") },
    { section: "view", id: "cmd.fontBigger", key: k("font-bigger"), action: "font-bigger", run: act("font-bigger") },
    { section: "view", id: "cmd.fontSmaller", key: k("font-smaller"), action: "font-smaller", run: act("font-smaller") },
    { section: "view", id: "cmd.fontReset", key: k("font-reset"), action: "font-reset", run: act("font-reset") },
    { section: "view", id: "cmd.appFontBigger", key: k("app-font-bigger"), action: "app-font-bigger", run: act("app-font-bigger") },
    { section: "view", id: "cmd.appFontSmaller", key: k("app-font-smaller"), action: "app-font-smaller", run: act("app-font-smaller") },
    { section: "view", id: "cmd.appFontReset", key: k("app-font-reset"), action: "app-font-reset", run: act("app-font-reset") },
    { section: "help", id: "cmd.shortcuts", key: k("shortcuts"), action: "shortcuts", run: act("shortcuts") },
    { section: "help", id: "cmd.openConfig", key: "", run: () => void openConfig() },
    { section: "help", id: "cmd.installCli", key: "", run: () => void installCli() },
    {
      section: "help", id: "cmd.startTrace", label: t(tracing() ? "cmd.stopTrace" : "cmd.startTrace"), key: "",
      run: () => {
        tracing() ? stopTrace() : startTrace();
        scheduleRender();
      },
    },
    { section: "help", id: "cmd.copyReport", key: "", run: () => void copyReport(), off: !hasReport() },
    { section: "help", id: "cmd.reportIssue", key: "", run: () => void openUrl(ISSUES_URL).catch(showError) },
    { section: "help", id: "cmd.checkUpdates", key: "", run: checkForUpdates },
    { section: "help", id: "cmd.about", key: "", run: () => void showAbout() },
  ];
}

const ISSUES_URL = "https://github.com/solutionscay/skiff/issues/new";

/** Where a group of commands starts a new block in its menu. */
const BREAK_BEFORE = new Set(["cmd.projectMenu", "cmd.newWorkspace", "cmd.settings", "cmd.copy", "cmd.rename", "cmd.palette", "cmd.splitRight", "cmd.nextWaiting", "cmd.focusMode", "cmd.fontBigger", "cmd.openConfig", "cmd.checkUpdates"]);

interface MenuItem { id?: string; label?: string; accel?: string; enabled?: boolean; native?: string }

/** On macOS these commands move from File and Help to the app menu, with the Mac names. */
const APP_MENU: Record<string, MessageKey> = { "cmd.about": "cmd.about", "cmd.settings": "cmd.settingsMac", "cmd.quit": "cmd.quitMac" };

let menuRuns = new Map<string, () => void>();
let menuSent = "";
let menuTimer: number | undefined;

/**
 * Renders ask for the menu sync; it runs once, MENU_MS after the first ask.
 * Agents make many renders a second, and the menu shows none of them until
 * it opens. The items keep the runs they were sent with until the sync.
 */
const MENU_MS = 250;
export function syncMenuSoon() {
  if (menuTimer !== undefined) return;
  menuTimer = window.setTimeout(() => {
    menuTimer = undefined;
    step("render.menu", syncMenu);
  }, MENU_MS);
}

/** Sends the command list to the native menu bar when it changes. */
export function syncMenu() {
  const menus: { title: string; items: MenuItem[] }[] = [];
  const runs = new Map<string, () => void>();
  const app: Record<string, MenuItem> = {};
  for (const [i, c] of commands().entries()) {
    const id = `cmd:${i}`;
    runs.set(id, c.run);
    const item = { id, label: c.label, accel: c.action ? menuAccel(c.action) : "", enabled: !c.off };
    const appLabel = isMac ? APP_MENU[c.id] : undefined;
    if (appLabel) {
      app[c.id] = { ...item, label: t(appLabel) };
      continue;
    }
    const title = t(`section.${c.section}`);
    let m = menus.at(-1);
    if (m?.title !== title) menus.push((m = { title, items: [] }));
    else if (BREAK_BEFORE.has(c.id)) m.items.push({});
    m.items.push(item);
  }
  // macOS puts the first menu under the app name. Hide, Hide Others and Show All are the system's own.
  if (isMac) menus.unshift({
    title: "Skiff",
    items: [
      app["cmd.about"], {}, app["cmd.settings"], {},
      { native: "hide" }, { native: "hide-others" }, { native: "show-all" }, {},
      app["cmd.quit"],
    ].filter(Boolean),
  });
  menuRuns = runs;
  const json = JSON.stringify(menus);
  if (json === menuSent) return;
  menuSent = json;
  invoke("set_menu", { menus }).catch(console.error);
}

void listen<string>("skiff:menu", (e) => menuRuns.get(e.payload)?.());
// The search button in the GNOME header bar.
void listen("skiff:search", () => runAction("palette"));

async function installCli() {
  const ok = await confirmAction({
    title: t("installCli.title"),
    body: t("installCli.body"),
    action: t("installCli.action"),
  });
  if (!ok) return;
  try {
    const path = await invoke<string>("install_cli");
    showNotice(t("installCli.done", { path }));
  } catch (e) {
    if (String(e) !== "cancelled") showError(e);
  }
}

async function openConfig() {
  await invoke("open_config").catch(showError);
}

/** The palette lists every enabled command, then sessions and worktrees. */
function paletteCommands(): SwitchItem[] {
  return commands()
    .filter((c) => !c.off && c.action !== "palette")
    .map((c) => {
      const section = t(`section.${c.section}`);
      return {
        kind: "command" as const,
        color: "var(--text-3)",
        primary: c.label,
        secondary: section,
        meta: c.key,
        text: `${section} ${c.label}`,
        run: c.run,
      };
    });
}

export const switcher = createSwitcher(() => {
  const items: SwitchItem[] = paletteCommands();
  const found: SwitchItem[] = [...sessions.values()].sort(bySessionPriority).map((s) => {
    const at = place(s);
    const where = at ? `${at.project.name} / ${branchName(at.worktree)}` : `other / ${s.cwd}`;
    return {
      kind: "session",
      color: accent(at?.project),
      primary: `${agentName(s)} · ${taskTitle(s)}`,
      secondary: where,
      state: s.state,
      meta: s.state === "waiting" || s.state === "idle" ? t(`state.${s.state}`) : "",
      // The state matches by its shown word and by its English id.
      text: `${where} ${agentName(s)} ${taskTitle(s)} ${t(`state.${s.state}`)} ${s.state}`,
      run: () => revealSession(s.id),
    };
  });
  items.push(...found);
  for (const p of S.closedProjects) {
    const primary = t("palette.openProject", { name: p.name });
    const secondary = t("palette.closed");
    items.push({
      kind: "command",
      color: accent(p),
      primary,
      secondary,
      meta: "",
      text: `${primary} ${secondary}`,
      run: () => void openClosedProject(p),
    });
  }
  // A plain folder has no worktrees to pick: its project entry opens it.
  for (const p of S.projects.filter((p) => !isFolder(p))) {
    for (const w of p.worktrees) {
      items.push({
        kind: "worktree",
        color: accent(p),
        primary: branchName(w),
        secondary: p.name,
        meta: tn("palette.sessions", worktreeSessions(w).length),
        text: `${p.name} / ${branchName(w)} ${t("switcher.kind.worktree")} worktree`,
        run: () => selectWorktree(p, w),
      });
    }
  }
  return items;
}, refocusTerminal);

const withProject = (f: (p: NonNullable<ReturnType<typeof currentProject>>) => Promise<void>) => {
  const p = currentProject();
  if (p) void f(p);
};

/** The closed projects, under the tree's Projects head. */
function openClosedMenu() {
  const r = document.querySelector<HTMLElement>("#sidebar-scroll .tree-head:not(.fleet-head)")?.getBoundingClientRect();
  closedProjectsMenu(r ? r.left + 24 : 80, r ? r.bottom : 80);
}
