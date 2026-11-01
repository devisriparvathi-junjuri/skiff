/**
 * Close and remove a project. Close keeps it in projects.toml with its
 * settings, out of the tree; Remove drops it. Both end its sessions and leave
 * the folder on disk.
 */
import { projectOrder } from "./fleets";
import { confirmAction } from "../ui/confirm";
import type { MenuEntry } from "../ui/menu";
import { ctxMenu } from "../ui/contextMenu";
import { agentName, branchName, taskTitle } from "./model";
import type { Project } from "../platform/types";
import { loadProjects } from "../app/daemon";
import { showError } from "../ui/alerts";
import { render } from "../app/render";
import { clearSelection } from "./selection";
import { collapsed, S, selectedWorktree, sessions } from "../app/state";
import { worktreeSessions } from "../app/stateQueries";

import { refocusTerminal, selectProject } from "./view";
import { t, tn } from "../i18n";
import { hostInvoke } from "../app/hostCaps";

/** The project's sessions, and one line per worktree that has any, for a confirmation. */
function projectSessions(p: Project) {
  const ids: string[] = [];
  const lines: string[] = [];
  for (const w of p.worktrees) {
    const list = worktreeSessions(w);
    if (!list.length) continue;
    lines.push(t("project.sessionsIn", { branch: branchName(w) }));
    for (const s of list) {
      ids.push(s.id);
      const vars = { agent: agentName(s), title: taskTitle(s) };
      if (s.state === "working" || s.state === "waiting") lines.push(t("project.sessionLineState", { ...vars, state: t(s.state === "working" ? "state.working" : "state.waiting") }));
      else lines.push(t("project.sessionLine", vars));
    }
  }
  const running = ids.filter((id) => sessions.get(id)?.state !== "done");
  return { ids, running, list: lines.join("\n") };
}

/** Ends the sessions. False when one would not end: the project stays as it was. */
async function endAll(ids: string[]): Promise<boolean> {
  clearSelection();
  const results = await Promise.allSettled(ids.filter((id) => sessions.has(id)).map((id) => hostInvoke("session_lifecycle", "kill_session", { session: id })));
  let ok = true;
  for (const r of results) {
    if (r.status === "rejected") {
      ok = false;
      showError(r.reason);
    }
  }
  return ok;
}

/** The project next to `p` in project order, else the one before it. */
function neighbor(p: Project): Project | null {
  const all = projectOrder();
  const i = all.findIndex((x) => x.name === p.name);
  return all[i + 1] ?? all[i - 1] ?? null;
}

/** After a close or remove: the tree loses the project, and a neighbor shows. */
async function leave(p: Project) {
  const next = S.selectedProject === p.name ? neighbor(p) : null;
  if (S.justAdded === p.name) S.justAdded = null;
  await loadProjects();
  if (next) selectProject(next.name);
  else render();
  refocusTerminal();
}

/** Closes the project. With running sessions, asks first: they end. */
export async function closeProject(p: Project) {
  const { ids, running, list } = projectSessions(p);
  if (running.length) {
    const ok = await confirmAction({
      title: t("project.closeTitle", { name: p.name }),
      body: tn("project.closeBody", running.length, { list }),
      action: tn("project.closeAction", running.length),
    });
    if (!ok) return refocusTerminal();
  }
  if (!(await endAll(ids))) return;
  try {
    await hostInvoke("project_config", "set_project_closed", { project: p.name, closed: true });
  } catch (e) {
    return showError(e);
  }
  await leave(p);
}

/** Removes the project and its settings, open or closed, after a confirmation. */
export async function removeProject(p: Project) {
  const { ids, running, list } = projectSessions(p);
  const ok = await confirmAction({
    title: t("project.removeTitle", { name: p.name }),
    body: running.length ? tn("project.removeBodyRunning", running.length, { path: p.path, list }) : t("project.removeBody", { path: p.path }),
    action: running.length ? tn("project.removeAction", running.length) : t("project.removeProject"),
  });
  if (!ok) return refocusTerminal();
  if (!(await endAll(ids))) return;
  try {
    await hostInvoke("project_config", "remove_project", { project: p.name });
  } catch (e) {
    return showError(e);
  }
  for (const w of p.worktrees) collapsed.delete(w.path);
  selectedWorktree.delete(p.name);
  await leave(p);
}

/** Opens a closed project again, with its settings, and shows it. */
export async function openClosedProject(p: Project) {
  try {
    await hostInvoke("project_config", "set_project_closed", { project: p.name, closed: false });
  } catch (e) {
    return showError(e);
  }
  await loadProjects();
  selectProject(p.name);
}

/** The project menu's last entries. */
export function closeEntries(p: Project): MenuEntry[] {
  return [
    { icon: "indicators-minus", label: t("cmd.closeProject"), run: () => void closeProject(p) },
    { sep: true },
    { icon: "tools-trash-2", label: t("cmd.removeProject"), danger: true, run: () => void removeProject(p) },
  ];
}

/** File › Open closed project: each closed project, to open or remove. */
export function closedProjectsMenu(x: number, y: number) {
  const entries: MenuEntry[] = S.closedProjects.map((p) => ({
    icon: "documents-folder-open",
    label: p.name,
    sub: [
      { icon: "documents-folder-open", label: t("menu.open"), run: () => void openClosedProject(p) },
      { icon: "tools-trash-2", label: t("menu.removeAsk"), danger: true, run: () => void removeProject(p) },
    ],
  }));
  if (!entries.length) entries.push({ label: t("closed.none"), disabled: true });
  ctxMenu.open(x, y, t("closed.title"), entries);
}
