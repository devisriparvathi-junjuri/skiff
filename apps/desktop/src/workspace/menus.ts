import { ctxMenu } from "../ui/contextMenu";
/** Right-click menus for projects, sessions, panes and groups. */
import { imageCount, openImages } from "../terminal/images";
import { fleetSkipperOf, fleetSkippersHere, isSkiffAgent, joinFleet, renameFleetOn, openWatchLog, projectSkipperOf, startFleetSkipper, startProjectSkipper, skiffAgentFolder, skiffAgentName, skiffWatch, skipperWord, startSkiffWatch } from "../app/skiffAgent";
import { launchIcon } from "../appearance/agentIcon";
import { confirmAction, promptAction } from "../ui/confirm";
import { h } from "../ui/dom";
import { pickColor } from "../appearance/colorPicker";
import { pickBackgroundOpacity } from "../appearance/backdropOpacity";
import { keyLabel } from "../app/keys";
import { t, tn, upper } from "../i18n";
import { toggleFocusMode, toggleMaximize } from "../app/modes";
import { type MenuEntry } from "../ui/menu";
import { isRemote } from "../app/hosts";
import type { SkipperStart } from "../ui/launchMenu";
import { agentName, basename, branchName, isFolder, locate, taskTitle, isProjectSkipper, scopeName } from "./model";
import { setAvatarDialog } from "./skipperFace";
import { cwdOf } from "./canvas";
import { filledOf, slotsOf } from "./layoutSlots";
import type { Group, HostBranches, Project, SessionInfo, SplitDir, Worktree } from "../platform/types";
import type { Fleet } from "./fleets";
import { fleetWorkspaceEntries, keepInView, projectWorkspaceEntries, renameWorkspaceFleet } from "./profile";
import { invoke } from "@tauri-apps/api/core";
import { open as openFile } from "@tauri-apps/plugin-dialog";
import { openPath } from "../platform/os";
import { loadProjects, newSession } from "../app/daemon";
import { launchMenu } from "../app/panels";
import { showError } from "../ui/alerts";
import { hasChanges, reviewChanges, setShowChanges, showsChanges } from "./changes";
import { setShowFiles, showsFiles } from "./files";
import { deleteGroup } from "../app/groups";
import { closeEntries } from "./projectClose";
import { sessionsOf } from "./layout";
import { bareWorktree, projectsHidden, renameHidden, setProjectsHidden } from "./tree";

import { startRename, startSessionRename } from "./rename";
import { render } from "../app/render";
import { clearSelection, selectionPlan, splitSelection } from "./selection";
import { MAX_PANES, removeErrors, removing, S, selectedWorktree, sessions } from "../app/state";
import { groupOf, launchChoices, launchDigit, place, shownIds, splitFull } from "../app/stateQueries";

import { groupThemeMenu, projectThemeMenu, sessionThemeMenu } from "../appearance/themes";
import { focusPane, refocusTerminal, removeFromGroup, showGroup, unsplit } from "./view";
import { resumeAllEntry, resumeOffer, resumePane } from "../terminal/resume";
import { ledgerFolded, ledgerShown, toggleLedger } from "./ledger";
import { hostCan, hostInvoke } from "../app/hostCaps";

async function setIcon(p: Project, icon: string | null) {
  try {
    await invoke("set_project_icon", { project: p.name, icon });
  } catch (e) {
    showError(e);
  }
}

const DEFAULT_COLOR = "#b69cff";

/** The color picker for a project's accent. */
function pickProjectColor(p: Project) {
  pickColor({
    title: t("color.title", { name: p.name }),
    start: p.color ?? DEFAULT_COLOR,
    action: t("color.action"),
    submit: (hex) => hostInvoke("project_config", "set_project_color", { project: p.name, color: hex }),
    onClose: refocusTerminal,
  });
}

async function setBackground(p: Project, background: string | null) {
  try {
    await invoke("set_project_background", { project: p.name, background });
  } catch (e) {
    showError(e);
  }
}

/** One image behind every pane of the project's terminal area. */
function backgroundEntries(p: Project): MenuEntry[] {
  return [
    {
      icon: "playback-image-plus",
      label: t("menu.chooseImage"),
      run: async () => {
        const file = await openFile({
          title: t("background.title", { name: p.name }),
          defaultPath: p.path,
          filters: [{ name: t("dialog.imageFiles"), extensions: ["png", "jpg", "jpeg", "webp", "gif", "svg"] }],
        }).catch(() => null);
        if (typeof file === "string") await setBackground(p, file);
      },
    },
    // Only an image shows through the panes, so the opacity waits for one.
    ...(p.background ? [{ icon: "playback-image", label: t("menu.opacity"), run: () => pickBackgroundOpacity(p) }] : []),
    { icon: "indicators-minus", label: t("menu.none"), disabled: !p.background, run: () => void setBackground(p, null) },
  ];
}

/** One place for the project icon: pick a file, detect one, or use letters. */
function iconEntries(p: Project): MenuEntry[] {
  return [
    {
      icon: "documents-folder-open",
      label: t("menu.chooseFile"),
      run: async () => {
        const file = await openFile({
          title: t("icon.title", { name: p.name }),
          defaultPath: p.path,
          filters: [{ name: t("dialog.imageFiles"), extensions: ["png", "svg", "ico", "jpg", "jpeg", "webp", "gif"] }],
        }).catch(() => null);
        if (typeof file !== "string") return;
        const rel = file.startsWith(p.path + "/") ? file.slice(p.path.length + 1) : file;
        await setIcon(p, rel);
      },
    },
    { icon: "schedule-refresh-cw", label: t("menu.detect"), run: () => void setIcon(p, null) },
    { icon: "indicators-minus", label: t("menu.lettersOnly"), run: () => void setIcon(p, "") },
  ];
}

/** Ask for the letters on the project's row. An empty field goes back to the first three letters of the name. */
function setInitials(p: Project) {
  promptAction({
    title: t("initials.title", { name: p.name }),
    body: t("initials.body"),
    placeholder: t("initials.placeholder"),
    value: p.short,
    allowEmpty: true,
    maxLength: 3,
    action: t("initials.action"),
    submit: (short) => hostInvoke("project_config", "set_project_short", { project: p.name, short }),
    onClose: refocusTerminal,
  });
}

/** The fleets now, for the picker. */
const fleetNames = () => [...new Set(S.projects.map((x) => x.fleet).filter((f): f is string => !!f))].sort((a, b) => a.localeCompare(b));

/** Pick the fleet the project sits in. Projects with the same fleet go under one fleet head in the tree. */
function setFleet(p: Project) {
  promptAction({
    title: t("fleet.title", { name: p.name }),
    body: t("fleet.body"),
    placeholder: t("fleet.placeholder"),
    value: p.fleet ?? "",
    allowEmpty: true,
    options: fleetNames(),
    clearLabel: p.fleet ? t("fleet.clear") : undefined,
    action: t("fleet.action"),
    // A project Skipper that comes under a running fleet Skipper is taken over, after one confirm.
    submit: async (fleet) => {
      const joined = await joinFleet(p.name, fleet.trim());
      if (joined === null) return;
      // A fleet the open workspace does not name, or none, would take the project out of view.
      keepInView(p, fleet.trim() || null);
      if (joined === false) await hostInvoke("project_config", "set_project_fleet", { project: p.name, fleet });
    },
    onClose: refocusTerminal,
  });
}

/** Give the fleet a new name: its projects, and its Skipper's folder, rows and session. */
function renameFleet(f: Fleet, name: string) {
  renameHidden(f.name, name);
  renameWorkspaceFleet(f.name, name.trim());
  return renameFleetOn(f.name, name, f.projects.map((p) => p.name));
}

/** Take every project out of the fleet. Each goes back under Projects. */
function dissolveFleet(f: Fleet) {
  for (const p of f.projects) keepInView(p, null);
  return hostInvoke<void>("project_config", "reorder_projects", { order: S.projects.map((x) => x.name), fleets: f.projects.map((p) => [p.name, ""]) });
}

/**
 * The fleet head's Skipper items, as a project row has them: Start fleet
 * Skipper; while one runs, Open fleet Skipper and Stop fleet Skipper….
 */
function fleetSkipperEntries(f: Fleet): MenuEntry[] {
  if (!fleetSkippersHere()) return [];
  const s = fleetSkipperOf(f.name);
  if (!s) {
    if (!skiffAgentFolder()) return [];
    return [{ icon: "tools-sparkles", label: t("menu.startFleetSkipper"), run: () => void startFleetSkipper(f.name) }];
  }
  return [
    { icon: "tools-sparkles", label: t("menu.openFleetSkipper"), run: () => void startFleetSkipper(f.name) },
    {
      icon: "indicators-square-stop",
      label: t("menu.stopFleetSkipper"),
      danger: true,
      run: async () => {
        if (await confirmAction({ title: t("stopSkipper.title", { name: f.name }), body: "", action: t("stopFleetSkipper.action") })) void endSession(s.id);
        else refocusTerminal();
      },
    },
  ];
}

/** A right-click on a fleet tile. */
export function fleetMenu(f: Fleet, x: number, y: number) {
  // The Host keeps its fleets: one that takes no project changes keeps them as they are.
  if (!hostCan("project_config")) return;
  const skipper = fleetSkipperEntries(f);
  ctxMenu.open(x, y, t("menu.fleet", { name: f.name }), [
    ...skipper,
    ...(skipper.length ? [{ sep: true as const }] : []),
    // The label says what a click does. Hidden, the head keeps its Skipper and the sessions that need a look.
    {
      icon: projectsHidden(f.name) ? "senses-eye" : "senses-eye-off",
      label: t(projectsHidden(f.name) ? "menu.showProjects" : "menu.hideProjects"),
      run: () => {
        setProjectsHidden(f.name, !projectsHidden(f.name));
        render();
      },
    },
    { sep: true },
    {
      icon: "tools-pencil",
      label: t("menu.renameFleet"),
      run: () =>
        promptAction({
          title: t("renameFleet.title", { name: f.name }),
          body: tn("renameFleet.body", f.projects.length),
          placeholder: t("fleet.placeholder"),
          value: f.name,
          action: t("renameFleet.action"),
          submit: (name) => renameFleet(f, name),
          onClose: refocusTerminal,
        }),
    },
    {
      icon: "code-ungroup",
      label: t(fleetSkipperOf(f.name) ? "menu.removeFleetAsk" : "menu.removeFleet"),
      run: async () => {
        // The fleet is gone, so its Skipper stops: one confirm.
        const s = fleetSkipperOf(f.name);
        if (s && !(await confirmAction({ title: t("removeFleet.title", { name: f.name }), body: t("removeFleet.body", { projects: f.projects.map((p) => p.name).join(", "), name: f.name }), action: t("removeFleet.action") }))) return refocusTerminal();
        if (s) await endSession(s.id);
        await dissolveFleet(f).catch(showError);
      },
    },
    ...fleetWorkspaceEntries(f.name),
  ]);
}

/** Ask for a branch, then make a worktree for it. A name the list shows is an
 *  existing branch with no worktree: git checks it out. Any other name is a new
 *  branch from the From branch. Errors stay in the dialog. */
export async function newWorktree(p: Project) {
  S.justAdded = null;
  // A Host without `branch_list` still makes new branches from the folder's.
  const b = await hostInvoke<HostBranches>("worktrees", "list_branches", { project: p.name }).catch(() => null);
  const local = new Set(b?.branches.filter((x) => !x.remote).map((x) => x.name));
  // A remote's branch is offered by its own name, as git checks it out.
  const free = [
    ...new Set(
      (b?.branches ?? []).flatMap((x) => {
        if (!x.remote) return x.worktree ? [] : [x.name];
        const name = x.name.slice(x.name.indexOf("/") + 1);
        return local.has(name) ? [] : [name];
      }),
    ),
  ];
  const isFree = (v: string) => free.some((n) => n.toLowerCase() === v.toLowerCase());

  const from = h("label", "prompt-from");
  const base = h("select", "");
  base.setAttribute("aria-label", t("newWorktree.fromAria"));
  const head = b?.head ?? "";
  for (const name of [...(b && !b.head ? [""] : []), ...(b?.branches.map((x) => x.name) ?? [])]) {
    const o = h("option", "", name || "HEAD");
    o.value = name;
    base.append(o);
  }
  base.value = head;
  from.append(h("span", "", t("newWorktree.from")), base);
  from.hidden = !b;

  promptAction({
    title: t("newWorktree.title", { name: p.name }),
    body: free.length
      ? t("newWorktree.bodyFree")
      : t("newWorktree.body"),
    placeholder: t("newWorktree.placeholder"),
    ...(free.length ? { options: free, optionsHead: t("newWorktree.freeHead") } : {}),
    after: [from],
    onInput: (v) => (from.hidden = !b || isFree(v)),
    action: t("newWorktree.action"),
    submit: async (branch) => {
      const checkout = isFree(branch);
      const start = checkout || base.value === head ? null : base.value;
      const w = await hostInvoke<Worktree>("worktrees", "add_worktree", { project: p.name, branch, base: start, checkout });
      selectedWorktree.set(p.name, w.path);
      await loadProjects();
    },
    onClose: refocusTerminal,
  });
}

/** What each project key does to the selected project. The project menu runs the same. */
export function projectRun(p: Project) {
  return {
    "project-folder": () => void openPath(p.path).catch(showError),
    "project-copy-path": () => void navigator.clipboard.writeText(p.path).catch(showError),
    "project-color": () => pickProjectColor(p),
    "project-changes": () => void (isFolder(p) || setShowChanges(p, !showsChanges(p))),
    "project-files": () => void setShowFiles(p, !showsFiles(p)),
  } as const;
}

/** The project row's ⋯, a right-click on it, or the menu key. */
export function projectMenu(p: Project, x: number, y: number) {
  const run = projectRun(p);
  // A remote Host's path is that Host's: nothing here opens it on this Mac,
  // and its images are paths there, which no capability sends yet. Its
  // settings and worktrees change on that Host, when it takes the change.
  const local = !isRemote();
  const edits = hostCan("project_config");
  const starts = hostCan("session_lifecycle");
  // One worktree: its start choices show here, as on a worktree's +. More: pick the worktree first.
  // A Host that starts no sessions shows New session, off.
  const bare = bareWorktree(p);
  const off = !!p.error || !p.worktrees.length || !starts;
  const start: MenuEntry[] = bare && !p.error && starts
    ? launchMenu.entries(p, bare)
    : [{ icon: "indicators-plus", label: t("menu.newSessionIn"), disabled: off, sub: p.worktrees.map((w) => ({
        icon: "code-git-branch",
        label: branchName(w),
        run: () => launchMenu.open(plusOf(w) ?? { x, y }, p, w),
      })) }];
  // New group stands on its own. One worktree: its layouts. More: the
  // worktree, then its layouts at that worktree's +, since a flyout goes one
  // level deep. The start choices of one worktree carry it already.
  const group: MenuEntry | false = !(bare && !p.error && starts) && {
    icon: "code-group",
    label: t("select.newGroup"),
    disabled: off,
    sub: bare
      ? launchMenu.groups(p, bare)
      : p.worktrees.map((w) => ({
          icon: "code-git-branch",
          label: branchName(w),
          run: () => {
            const r = plusOf(w)?.getBoundingClientRect();
            ctxMenu.open(r ? r.right : x, r ? r.top : y, branchName(w), launchMenu.groups(p, w));
          },
        })),
  };
  // How the project looks. Icon and Background are sections: a flyout goes one level deep.
  const appearance = tidy([
    edits && { icon: "tools-sparkles", label: t("menu.color"), hint: keyLabel("project-color"), run: run["project-color"] },
    edits && { icon: "tools-sparkles", label: t("menu.projectTheme"), hint: keyLabel("theme"), run: () => void projectThemeMenu(p) },
    edits && { icon: "tools-pencil", label: t("menu.initials"), run: () => setInitials(p) },
    ...(local ? [{ head: upper(t("menu.headIcon")) }, ...iconEntries(p), { head: upper(t("menu.headBackground")) }, ...backgroundEntries(p)] : []),
  ]);
  // What the project's row shows in the sidebar. A check marks what shows.
  const sidebar = tidy([
    // A plain folder has no git: no Changes.
    edits && !isFolder(p) && { icon: showsChanges(p) ? "indicators-check" : "", label: t("menu.changes"), hint: keyLabel("project-changes"), run: run["project-changes"] },
    edits && hostCan("files") && { icon: showsFiles(p) ? "indicators-check" : "", label: t("menu.files"), hint: keyLabel("project-files"), run: run["project-files"] },
  ]);
  // This Mac starts them itself; a remote Host, through its skiff-host.
  // The start choices carry the Skipper first when they show; else its items lead the menu.
  const carried = !!bare && !p.error && starts;
  const skipper = local || hostCan("skipper_run") ? projectSkipperEntries(p, !carried) : [];
  ctxMenu.open(x, y, p.path, tidy([
    ...(carried ? [] : skipper),
    { sep: true },
    ...start,
    group,
    !!bare && { sep: true },
    // A plain folder has no git: no worktrees.
    hostCan("worktrees") && !isFolder(p) && { icon: "code-git-branch", label: t("cmd.newWorktree"), hint: keyLabel("new-worktree"), disabled: !!p.error, run: () => newWorktree(p) },
    ...(carried ? skipper : []),
    { sep: true },
    local && { icon: "indicators-square-arrow-out-up-right", label: t("menu.showInFileManager"), hint: keyLabel("project-folder"), run: run["project-folder"] },
    { icon: "code-copy", label: t("menu.copyPath"), hint: keyLabel("project-copy-path"), run: run["project-copy-path"] },
    appearance.length > 0 && { icon: "tools-sparkles", label: t("menu.appearance"), sub: appearance },
    sidebar.length > 0 && { icon: "layouts-panel-left", label: t("menu.sidebar"), sub: sidebar },
    { sep: true },
    // Where the project sits: its fleet, then the workspace. The Host keeps its fleets.
    edits && { icon: "code-group", label: t(p.fleet ? "menu.moveToFleet" : "menu.addToFleet"), run: () => setFleet(p) },
    ...(local ? projectWorkspaceEntries(p) : []),
    ...(edits ? closeEntries(p) : []),
  ]));
}

/** Drops the entries a Host gate left out, and any rule that no longer sits
 *  between two items: none first, last or twice in a row. */
function tidy(entries: (MenuEntry | false)[]): MenuEntry[] {
  const out: MenuEntry[] = [];
  for (const e of entries) {
    if (!e) continue;
    if ("sep" in e && (!out.length || "sep" in out[out.length - 1])) continue;
    out.push(e);
  }
  while (out.length && "sep" in out[out.length - 1]) out.pop();
  return out;
}

/** The + on a worktree's row in the sidebar, if it shows. */
function plusOf(w: Worktree): HTMLElement | undefined {
  return [...document.querySelectorAll<HTMLElement>("#sidebar-scroll .wt-plus")].find((b) => b.dataset.wt === w.path);
}

/** A right-click on a worktree's branch row. */
export function worktreeMenu(p: Project, w: Worktree, x: number, y: number) {
  const plus = plusOf(w);
  // A remote Host's path: start a session there or copy it, never open it on this Mac.
  const local = !isRemote();
  const off = !!p.error || !hostCan("session_lifecycle");
  const entries: (MenuEntry | false)[] = [
    { icon: "indicators-plus", label: t("cmd.newSession"), hint: keyLabel("new-session"), disabled: off, run: () => launchMenu.open(plus ?? { x, y }, p, w) },
    // A new group stands on its own, beside New session: its layouts.
    { icon: "code-group", label: t("select.newGroup"), disabled: off, sub: launchMenu.groups(p, w) },
    showsChanges(p) && hasChanges(w) && { icon: "code-git-branch", label: t("menu.reviewChanges"), run: () => reviewChanges(w) },
    local && { icon: "indicators-square-arrow-out-up-right", label: t("menu.showInFileManager"), run: () => void openPath(w.path).catch(showError) },
    { icon: "code-copy", label: t("menu.copyPath"), run: () => void navigator.clipboard.writeText(w.path).catch(showError) },
    // The Host removes it, and refuses while one of its sessions runs there.
    !w.is_main && hostCan("worktrees") && { icon: "tools-trash-2", label: t("menu.removeWorktree"), danger: true, disabled: removing.has(w.path), run: () => void removeWorktree(p, w) },
  ];
  ctxMenu.open(x, y, branchName(w), entries.filter((e): e is MenuEntry => !!e));
}

function sessionLabel(s: SessionInfo) {
  return `${agentName(s)} · ${taskTitle(s)}`;
}

export function rowMenu(s: SessionInfo, x: number, y: number) {
  if (isSkiffAgent(s.id)) return skiffAgentMenu(s, x, y);
  const ids = S.selection.filter((id) => sessions.has(id));
  if (ids.length > 1 && ids.includes(s.id)) return selectionMenu(ids, x, y);
  ctxMenu.open(x, y, sessionLabel(s), [
    ...(groupOf(s.id)
      ? [{ icon: "code-ungroup", label: t("cmd.removeFromGroup"), hint: keyLabel("close-pane"), disabled: !hostCan("session_groups"), run: () => removeFromGroup(s.id) }]
      : []),
    // A project Skipper's name is derived from the Skiff agent's name and its project: nothing to type.
    ...(isProjectSkipper(s) ? [] : [{ icon: "tools-pencil", label: t("menu.rename"), hint: keyLabel("rename"), disabled: !hostCan("session_lifecycle"), run: () => startSessionRename(s.id) }]),
    themeEntry(s),
    ...(isProjectSkipper(s) && s.skipper ? [avatarEntry(s.skipper, t("skipper.avatarTitle", { name: t(s.skipper.startsWith("fleet:") ? "skipper.avatarFleet" : "skipper.avatarProject", { scope: scopeName(s.skipper), skipper: skipperWord() }) }))] : []),
    endEntry(s),
  ]);
}

/**
 * The menu of a row in a multi-selection acts on every selected session. Rename and
 * the theme act on one session, so they are left out. No hints: the keys act on one row.
 */
function selectionMenu(ids: string[], x: number, y: number) {
  const plan = selectionPlan();
  const grouped = ids.filter((id) => groupOf(id));
  ctxMenu.open(x, y, tn("menu.selected", ids.length), [
    ...(plan
      ? [{
          icon: "code-group",
          label: plan.kind === "new" ? tn("menu.groupSelected", ids.length) : plan.label,
          disabled: plan.size > MAX_PANES,
          run: splitSelection,
        }]
      : []),
    ...(grouped.length
      ? [{
          icon: "code-ungroup",
          label: tn("menu.ungroupSelected", grouped.length),
          run: () => {
            clearSelection();
            for (const id of grouped) removeFromGroup(id);
          },
        }]
      : []),
    endSelectedEntry(ids),
  ]);
}

const pick = (m: MenuEntry) => void ("run" in m && m.run?.());

/**
 * Delete or Backspace on a list row. With one way to remove it, that runs
 * (End or removal asks first). With more, a menu at the row asks which.
 */
export function deleteKeyMenu(el: HTMLElement) {
  // A key opened it: show focus rings, so the first item reads as the one picked.
  document.body.classList.add("kbd");
  const r = el.getBoundingClientRect();
  const [x, y] = [r.left + 24, r.bottom];
  const g = S.groups.find((x) => x.id === el.dataset.group);
  if (g && !el.dataset.session) return ctxMenu.open(x, y, t("menu.group", { name: g.name }), groupCloseEntries(g));
  const s = sessions.get(el.dataset.session ?? "");
  if (!s) return;
  const ids = S.selection.filter((id) => sessions.has(id));
  if (ids.length > 1 && ids.includes(s.id)) return pick(endSelectedEntry(ids));
  if (!groupOf(s.id)) return pick(endEntry(s));
  ctxMenu.open(x, y, sessionLabel(s), [
    { icon: "code-ungroup", label: t("cmd.removeFromGroup"), hint: keyLabel("close-pane"), disabled: !hostCan("session_groups"), run: () => removeFromGroup(s.id) },
    endEntry(s),
  ]);
}

/** A Skipper's face: `scope` "fleet:<name>" or a project. Kept in its folder on this Mac. The Admiral wears the mascot's head. */
function avatarEntry(scope: string, title: string): MenuEntry {
  return { icon: "playback-image", label: t("menu.avatar"), disabled: isRemote(), run: () => setAvatarDialog(scope, title, () => {}) };
}

function themeEntry(s: SessionInfo): MenuEntry {
  return { icon: "tools-sparkles", label: t("cmd.sessionTheme"), hint: keyLabel("theme"), disabled: !hostCan("session_lifecycle"), run: () => void sessionThemeMenu(s) };
}

/** Right-click, End session: stops the process and drops it from the list. */
export function endEntry(s: SessionInfo): MenuEntry {
  if (s.state === "done") {
    return {
      icon: "tools-trash-2",
      label: t("menu.removeFromList"),
      hint: keyLabel("end-session"),
      danger: true,
      disabled: !hostCan("session_lifecycle"),
      run: async () => {
        const ok = await confirmAction({
          title: t("removeSession.title", { name: taskTitle(s) }),
          body: t("removeSession.body"),
          action: t("removeSession.action"),
        });
        if (ok) void endSession(s.id);
        else refocusTerminal();
      },
    };
  }
  return {
    icon: "indicators-square-stop",
    label: t("cmd.endSession"),
    hint: keyLabel("end-session"),
    danger: true,
    disabled: !hostCan("session_lifecycle"),
    // A running agent loses its work: ask once more.
    run: async () => {
      const ok = await confirmAction({
        title: t("endSession.title", { name: taskTitle(s) }),
        body: t("endSession.body", { agent: agentName(s) }),
        action: t("endSession.action"),
      });
      if (ok) void endSession(s.id);
      else refocusTerminal();
    },
  };
}

/** Right-click on a multi-selection: ends every selected session, with one confirm. */
function endSelectedEntry(ids: string[]): MenuEntry {
  return endManyEntry(ids, tn("menu.endSelected", ids.length), tn("endSessions.title", ids.length));
}

/** Ends several sessions after one confirm. Sessions that already exited go without asking. */
function endManyEntry(ids: string[], label: string, title: string): MenuEntry {
  const running = ids.filter((id) => sessions.get(id)?.state !== "done");
  const end = () => {
    clearSelection();
    render();
    for (const id of ids) void endSession(id);
  };
  if (!running.length) {
    return { icon: "tools-trash-2", label: tn("menu.removeSelected", ids.length), danger: true, disabled: !hostCan("session_lifecycle"), run: end };
  }
  return {
    icon: "indicators-square-stop",
    label,
    danger: true,
    disabled: !hostCan("session_lifecycle"),
    run: async () => {
      const ok = await confirmAction({
        title,
        body: tn("endSessions.body", running.length),
        action: t("endSessions.action"),
      });
      if (ok) end();
      else refocusTerminal();
    },
  };
}

/**
 * The Skipper the start menus offer first: Open fleet Skipper while the
 * fleet's runs, Open project Skipper while the project's runs, else Start
 * project Skipper. Start opens no dialog.
 */
export function skipperStart(p: Project): SkipperStart | null {
  // This Mac starts them itself; a remote Host, through its skiff-host.
  if (isRemote() && !hostCan("skipper_run")) return null;
  const s = projectSkipperOf(p.name);
  // One Skipper per project: the fleet's Skipper commands it while it runs.
  if (p.fleet && !s && fleetSkipperOf(p.fleet)) return { label: t("menu.openFleetSkipper"), run: () => void startFleetSkipper(p.fleet!) };
  // startProjectSkipper shows the one that runs.
  if (s) return { label: t("menu.openProjectSkipper"), run: () => startProjectSkipper(p.name) };
  if (!skiffAgentFolder()) return null;
  return { label: t("menu.startProjectSkipper"), disabled: !!p.error, run: () => startProjectSkipper(p.name) };
}

/**
 * The project menu's Skipper entries. While one runs, Stop project Skipper…
 * asks first, as End does. Its panes keep running. `start` false leaves out
 * the skipperStart entry: the start choices above carry it.
 */
function projectSkipperEntries(p: Project, start = true): MenuEntry[] {
  const s = projectSkipperOf(p.name);
  const go = start ? skipperStart(p) : null;
  const entries: MenuEntry[] = go ? [{ icon: "tools-sparkles", ...go }] : [];
  if (s) entries.push({
    icon: "indicators-square-stop",
    label: t("menu.stopProjectSkipper"),
    danger: true,
    run: async () => {
      const ok = await confirmAction({
        title: t("stopSkipper.title", { name: p.name }),
        body: t("stopProjectSkipper.body", { agent: agentName(s) }),
        action: t("stopProjectSkipper.action"),
      });
      if (ok) void endSession(s.id);
      else refocusTerminal();
    },
  });
  return entries;
}

async function endSession(id: string) {
  try {
    await hostInvoke("session_lifecycle", "kill_session", { session: id });
  } catch (e) {
    showError(e);
  }
}

export function paneMenu(id: string, x: number, y: number) {
  const s = sessions.get(id);
  if (!s) return;
  if (id !== S.focused) focusPane(id);
  if (isSkiffAgent(id)) return skiffAgentMenu(s, x, y);
  const full = splitFull();
  const entries: MenuEntry[] = [
    { icon: "layouts-panel-left:flip", label: t("cmd.splitRight"), hint: keyLabel("split-right"), disabled: full || !hostCan("session_lifecycle"), run: () => splitMenu(id, "row", x, y) },
    { icon: "layouts-panel-bottom", label: t("cmd.splitDown"), hint: keyLabel("split-down"), disabled: full || !hostCan("session_lifecycle"), run: () => splitMenu(id, "col", x, y) },
    ...(isProjectSkipper(s) ? [] : [{ icon: "tools-pencil", label: t("menu.rename"), hint: keyLabel("rename"), disabled: !hostCan("session_lifecycle"), run: () => startSessionRename(id) }]),
  ];
  const offer = resumeOffer(s);
  if (offer) entries.unshift({ icon: "schedule-refresh-cw", label: t("menu.resume", { agent: offer.agent }), run: () => resumePane(id, { x, y }) });
  // These act on the pane you right-clicked, which the line above just focused.
  if (shownIds().length > 1) {
    entries.push({ icon: S.maximized ? "view-restore" : "view-maximize", label: t(S.maximized ? "cmd.restore" : "cmd.maximize"), hint: keyLabel("maximize"), run: toggleMaximize });
  }
  entries.push({ icon: "view-focus", label: t(S.focusMode ? "cmd.leaveFocusMode" : "cmd.focusMode"), hint: keyLabel("focus-mode"), run: toggleFocusMode });
  entries.push(themeEntry(s));
  entries.push({ icon: "playback-image", label: t("cmd.images"), hint: keyLabel("images"), disabled: !imageCount(), run: openImages });
  if (shownIds().length > 1) {
    entries.push({ icon: "code-ungroup", label: t("cmd.removeFromGroup"), hint: keyLabel("close-pane"), disabled: !hostCan("session_groups"), run: () => removeFromGroup(id) });
  }
  entries.push(endEntry(s));
  ctxMenu.open(x, y, sessionLabel(s), entries);
}

/**
 * The Skiff agent's row and pane menu: no splits, groups or rename. Its name is in Settings.
 * With no session, the row shows only its folder entries.
 */
export function skiffAgentMenu(s: SessionInfo | null, x: number, y: number) {
  const folder = skiffAgentFolder();
  // A remote Host's folder and log are that Host's: neither opens here.
  const local = !isRemote();
  const entries: MenuEntry[] = folder && local
    ? [{ icon: "indicators-square-arrow-out-up-right", label: t("menu.showInFileManager"), run: () => void openPath(folder).catch(showError) }]
    : [];
  if (s) {
    // Only while the row's icon is red: no watcher runs, or its last line failed.
    const watch = skiffWatch(s).state;
    // A remote Host's skiff-host starts it there.
    if (watch === "missing" && (local || hostCan("skipper_run"))) entries.push({ icon: "schedule-refresh-cw", label: t("menu.startWatcher"), run: startSkiffWatch });
    if ((watch === "missing" || watch === "failed") && local) entries.push({ icon: "indicators-square-arrow-out-up-right", label: t("menu.openWatchLog"), run: openWatchLog });
    entries.push(
      { icon: "view-focus", label: t(S.focusMode ? "cmd.leaveFocusMode" : "cmd.focusMode"), hint: keyLabel("focus-mode"), run: toggleFocusMode },
    );
    if (ledgerShown()) entries.push({ icon: "layouts-panel-left:flip", label: t(ledgerFolded() ? "cmd.showLedger" : "cmd.hideLedger"), hint: keyLabel("ledger"), run: toggleLedger });
    entries.push(
      themeEntry(s),
      endEntry(s),
    );
  }
  if (entries.length) ctxMenu.open(x, y, s ? sessionLabel(s) : skiffAgentName(), entries);
}

/** Second step of Split: a new shell or agent in this pane's worktree. */
export function splitMenu(id: string, dir: SplitDir, x: number, y: number) {
  const s = sessions.get(id);
  if (!s) return;
  const at = place(s);
  const cwd = at ? at.worktree.path : s.cwd;
  const where = at ? branchName(at.worktree) : basename(s.cwd);
  const entries: MenuEntry[] = [{ head: upper(t("menu.newIn", { where })) }];
  launchChoices().forEach((a, i) => {
    entries.push({
      glyph: launchIcon(a),
      label: a?.id ?? t("menu.shell"),
      hint: launchDigit(i),
      run: () => void newSession(cwd, a?.command ?? null, a?.id ?? "", a ? "agent" : "shell", { target: id, dir }).catch(showError),
    });
  });
  ctxMenu.open(x, y, t(dir === "row" ? "menu.addPaneRightTitle" : "menu.addPaneBelowTitle"), entries);
}

export function groupMenu(g: Group, x: number, y: number) {
  const resume = resumeAllEntry(sessionsOf(g.layout));
  ctxMenu.open(x, y, t("menu.group", { name: g.name }), [
    ...(resume ? [resume] : []),
    ...groupStartEntries(g),
    { head: upper(t("menu.headGroup")) },
    { icon: "tools-pencil", label: t("menu.rename"), hint: keyLabel("rename"), disabled: !hostCan("session_groups"), run: () => startRename(g) },
    { icon: "tools-sparkles", label: t("menu.groupTheme"), hint: keyLabel("theme"), disabled: !hostCan("session_lifecycle"), run: () => void groupThemeMenu(g) },
    ...groupCloseEntries(g),
  ]);
}

/** Shell, then agents: a new session in the group's first empty pane, else a new pane on the right. */
function groupStartEntries(g: Group): MenuEntry[] {
  const cwd = cwdOf(g);
  const at = cwd ? locate(S.projects, cwd) : null;
  const where = at ? branchName(at.worktree) : cwd ? basename(cwd) : t("menu.home");
  const empty = slotsOf(g.layout)[0];
  const filled = filledOf(g.layout).filter((id) => sessions.has(id));
  const full = !empty && filled.length >= MAX_PANES;
  const entries: MenuEntry[] = [{ head: upper(t("menu.newIn", { where })) }];
  launchChoices().forEach((a, i) => {
    entries.push({
      glyph: launchIcon(a),
      label: a?.id ?? t("menu.shell"),
      hint: launchDigit(i),
      disabled: full || !hostCan("session_lifecycle"),
      run: () => {
        if (g.id !== S.activeGroup) showGroup(g.id);
        const target = g.focus && filled.includes(g.focus) ? g.focus : filled[0];
        const place = empty ? { slot: empty } : { target, dir: "row" as const };
        void newSession(cwd, a?.command ?? null, a?.id ?? "", a ? "agent" : "shell", place).catch(showError);
      },
    });
  });
  return entries;
}

/** Drops the group. Its sessions keep running and go back under their worktrees. */
export function ungroup(g: Group) {
  if (g.id === S.activeGroup) unsplit(g);
  else {
    deleteGroup(g);
    render();
  }
}

/** Kill all sessions in the group. It asks first when one runs. The group goes with its last session. */
export async function killGroup(id: string) {
  // Group reloads replace objects while a menu or confirmation is open.
  const group = S.groups.find((x) => x.id === id);
  if (!group) return;
  const ids = sessionsOf(group.layout).filter((s) => sessions.has(s));
  const running = ids.filter((s) => sessions.get(s)?.state !== "done");
  if (running.length) {
    const ok = await confirmAction({
      // Not the group's name: an unnamed group is called "1 over 2", which reads as noise here.
      title: t("killGroup.title"),
      body: tn("killGroup.body", running.length),
      action: t("killGroup.action"),
    });
    if (!ok) return refocusTerminal();
  }
  clearSelection();
  const results = await Promise.allSettled(ids.filter((id) => sessions.has(id)).map((id) => hostInvoke("session_lifecycle", "kill_session", { session: id })));
  for (const result of results) {
    if (result.status === "rejected") {
      showError(result.reason);
    }
  }
  render();
}

/** The same group actions serve context menus, the menu bar, and the palette. */
export function groupCloseEntries(g: Group | null): Exclude<MenuEntry, { head: string } | { sep: true }>[] {
  const current = () => S.groups.find((x) => x.id === g?.id);
  const idsOf = (group: Group) => sessionsOf(group.layout).filter((id) => sessions.has(id));
  const hasSessions = !!g && idsOf(g).length > 0;
  const drop = () => {
    // Group reloads replace objects while a menu or confirmation is open.
    const group = current();
    if (group) ungroup(group);
  };
  const entries: Exclude<MenuEntry, { head: string } | { sep: true }>[] = [
    { icon: "code-ungroup", label: t("menu.ungroup"), hint: keyLabel("close-pane"), disabled: !g || !hostCan("session_groups"), run: drop },
  ];
  if (hasSessions) entries.push(
    { icon: "indicators-square-stop", label: t("menu.killGroup"), hint: keyLabel("end-session"), danger: true, disabled: !hostCan("session_lifecycle"), run: () => void killGroup(g!.id) },
  );
  return entries;
}

export async function removeWorktree(p: Project, w: Worktree) {
  if (removing.has(w.path)) return;
  const ok = await confirmAction({
    title: t("removeWorktree.title", { name: branchName(w) }),
    body: t("removeWorktree.body", { path: w.path }),
    action: t("removeWorktree.action"),
  });
  if (!ok) return refocusTerminal();
  removing.add(w.path);
  removeErrors.delete(w.path);
  render();
  try {
    try {
      await hostInvoke("worktrees", "remove_worktree", { project: p.name, path: w.path, force: false });
    } catch (e) {
      // The Host refuses uncommitted changes. Ask again, and force only on a yes:
      // a new request the person chose, never a retry of the first.
      if (!String(e).includes("uncommitted changes")) throw e;
      const force = await confirmAction({
        title: t("dirtyWorktree.title", { name: branchName(w) }),
        body: t("dirtyWorktree.body"),
        action: t("dirtyWorktree.action"),
      });
      if (!force) return refocusTerminal();
      await hostInvoke("worktrees", "remove_worktree", { project: p.name, path: w.path, force: true });
    }
    if (selectedWorktree.get(p.name) === w.path) selectedWorktree.delete(p.name);
    await loadProjects();
  } catch (e) {
    removeErrors.set(w.path, String(e));
  } finally {
    removing.delete(w.path);
    render();
  }
}
