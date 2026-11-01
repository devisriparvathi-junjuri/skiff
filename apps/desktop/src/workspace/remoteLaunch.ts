/**
 * New session on a remote Host. The folder is one the Host knows: a
 * worktree of its projects, a folder it saw in use (Recent), its home
 * folder, or one picked from its disk in the folder picker. Nothing is
 * typed. The choices are the Host's own agents. A folder here is a path on
 * that Host: it goes only into the session spec, never to an opener on
 * this Mac.
 */
import type { FolderPlaces, Group, Project, SessionInfo } from "../platform/types";
import type { MenuEntry } from "../ui/menu";
import { ctxMenu } from "../ui/contextMenu";
import { showError } from "../ui/alerts";
import { newSession } from "../app/daemon";
import { hostCan, hostInvoke } from "../app/hostCaps";
import { hostConnected, hostName } from "../app/hosts";
import { S, sessions } from "../app/state";
import { launchChoices, launchDigit } from "../app/stateQueries";
import { launchIcon } from "../appearance/agentIcon";
import { openFolderPicker } from "./folderPicker";
import { basename } from "./model";
import { refocusTerminal } from "./view";
import { t, upper } from "../i18n";

/** The folders the Host reported: its sessions' and groups' folders, once each, `first` before the rest. */
export function hostFolders(list: Iterable<SessionInfo>, groups: Group[], first?: string | null): string[] {
  const seen = new Set<string>();
  for (const s of list) if (s.cwd) seen.add(s.cwd);
  for (const g of groups) if (g.cwd) seen.add(g.cwd);
  const rest = [...seen].filter((f) => f !== first).sort();
  return first ? [first, ...rest] : rest;
}

/** The Host's project worktrees, as menu labels: the project, then the branch for a linked one. */
export function hostWorktrees(projects: Project[]): { label: string; path: string }[] {
  return projects.flatMap((p) =>
    p.worktrees.map((w) => ({ label: w.is_main ? p.name : `${p.name} › ${w.branch ?? basename(w.path)}`, path: w.path })),
  );
}

/** Recent: the Host's own list first, then its sessions' and groups'
 *  folders, once each, without the folders listed above them. */
export function recentFolders(hostRecent: string[], reported: string[], skip: Set<string>): string[] {
  const out: string[] = [];
  for (const f of [...hostRecent, ...reported]) if (!skip.has(f) && !out.includes(f)) out.push(f);
  return out.slice(0, 20);
}

/** The Host's Recent list from its last answer. The menu opens at once
 *  with it, and asks the Host again for the next time. */
let recent: { host: string; list: string[] } = { host: "", list: [] };
function refreshRecent(host: string) {
  try {
    if (!hostCan("folders")) return;
    void hostInvoke<FolderPlaces>("folders", "folder_places")
      .then((p) => void (recent = { host, list: p.recent }))
      .catch(() => {});
  } catch {
    // No Host call here: Recent stays as it was.
  }
}

/** Shell, then the Host's agents, as the other start menus list them. */
function startEntries(cwd: string | null): MenuEntry[] {
  return launchChoices().map((a, i) => ({
    glyph: launchIcon(a),
    label: a?.id ?? t("menu.shell"),
    hint: launchDigit(i),
    run: () => start(cwd, a?.id ?? null),
  }));
}

/** A menu can stay open across a lost link, and the Host may list other
 *  agents once it is back. So the pick is read again when it runs: the
 *  agent's command as the Host lists it now, or nothing. */
function start(cwd: string | null, id: string | null) {
  const host = hostName();
  if (!hostConnected()) return showError(t("remote.notConnected", { host }));
  const a = id === null ? null : launchChoices().find((c) => c?.id === id);
  if (a === undefined) return showError(t("remote.noAgent", { host, agent: id ?? "" }));
  void newSession(cwd, a?.command ?? null, a?.id ?? "", a ? "agent" : "shell").catch(showError);
}

/** The folder menu. `near`: the highlighted or focused session's folder, listed first. */
export function openRemoteNewSession(x: number, y: number, near?: string | null) {
  const host = hostName();
  const folder = (label: string, path: string | null): MenuEntry => ({ icon: "documents-folder-open", label, sub: startEntries(path) });
  const worktrees = hostWorktrees(S.projects ?? []).filter((w) => w.path !== near);
  const skip = new Set([...(near ? [near] : []), ...worktrees.map((w) => w.path)]);
  const hostRecent = recent.host === host ? recent.list : [];
  const others = recentFolders(hostRecent, hostFolders(sessions.values(), S.groups), skip);
  const entries: MenuEntry[] = [
    { head: upper(t("remote.folderOn", { host })) },
    ...(near ? [folder(near, near)] : []),
    ...(worktrees.length ? [{ head: upper(t("remote.worktrees")) }, ...worktrees.map((w) => folder(w.label, w.path))] : []),
    ...(others.length ? [{ head: upper(t("remote.recent")) }, ...others.map((f) => folder(f, f))] : []),
    { sep: true },
    folder(t("remote.homeFolder"), null),
    { icon: "documents-folder-open", label: t("remote.chooseFolder"), run: () => chooseFolder(x, y) },
  ];
  ctxMenu.open(x, y, t("menu.newSessionIn"), entries);
  refreshRecent(host);
}

/** The folder picker, then the start menu for the picked folder. */
function chooseFolder(x: number, y: number) {
  let picked: string | null = null;
  openFolderPicker({
    action: t("remote.startHere"),
    pick: async (path) => void (picked = path),
    onClose: () => {
      if (picked === null) return refocusTerminal();
      ctxMenu.open(x, y, t("menu.newSessionIn"), [{ head: upper(t("remote.newIn", { name: basename(picked) })) }, ...startEntries(picked)]);
    },
  });
}
