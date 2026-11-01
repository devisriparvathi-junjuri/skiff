/** Shared app state. Query functions live in stateQueries.ts. */

import type { AgentInfo, Group, Project, SessionInfo, TerminalTheme } from "../platform/types";
import type { SkiffAgentSettings } from "./skiffAgent";
import { t } from "../i18n";

/** Accent for sessions outside every project, and projects without `color`. */
export const DEFAULT_ACCENT = "var(--project-default)";

export const sessions = new Map<string, SessionInfo>();

/** Selected worktree path per project name. */
export const selectedWorktree = new Map<string, string>();
export const collapsed = new Set<string>();

export const removeErrors = new Map<string, string>();

/** Worktrees with a remove in flight. */
export const removing = new Set<string>();

/** Merge a daemon copy into the known session, so menus and closures holding it see the change. */
export function upsert(info: SessionInfo) {
  const known = sessions.get(info.id);
  // skiffd leaves `watch` out when no watcher runs: that clears it.
  if (known) Object.assign(known, { watch: null }, info);
  else sessions.set(info.id, info);
}

/** The tree's "Other" entry: sessions outside every project (a removed worktree, a folder not added). */
export const OTHER = "\u0000other";

/** A group holds at most this many panes. */
export const MAX_PANES = 4;

export const fullHint = () => t("view.fullHint", { max: MAX_PANES });

/** Sessions the daemon removed. A list taken before the removal must not bring them back. */
export const gone = new Set<string>();

/** One set per list_sessions in flight: sessions created meanwhile, which that list cannot hold. */
export const born = new Set<Set<string>>();

export const FONT_DEFAULT = 13;

function savedFontSize(): number {
  try {
    return Number(localStorage.getItem("skiff.fontSize")) || FONT_DEFAULT;
  } catch {
    return FONT_DEFAULT;
  }
}

/** State that several modules reassign. */
export const S = {
  /** The pane that gets the keys. Always one of the shown sessions, or null. */
  focused: null as string | null,
  /** The group whose row is the sidebar selection. Its terminals are shown but none is picked. */
  groupPicked: null as string | null,
  // What the terminal area shows: the active group's layout, else one session.
  groups: [] as Group[],
  activeGroup: null as string | null,
  single: null as string | null,
  /** The group whose name is an input, and what is typed in it. */
  renaming: null as { id: string; name: string } | null,
  /** The session whose title is an input for renaming. */
  renamingSession: null as string | null,

  /** Open projects, in saved order. */
  projects: [] as Project[],
  /** Closed projects: kept with their settings, out of the tree. */
  closedProjects: [] as Project[],
  projectsError: null as string | null,
  /** The selected Host's data plane (skiff-host), which serves projects and
   *  Git: still loading, ready, or out of reach with `dataNote` as the cause. */
  dataState: "loading" as "loading" | "ready" | "unavailable",
  dataNote: null as string | null,
  selectedProject: null as string | null,
  /** Known agents with installed and enabled state, from list_agents. */
  agents: [] as AgentInfo[],
  /** `[skiff_agent]`. With a folder, its row shows above the projects. */
  skiffAgent: null as SkiffAgentSettings | null,
  /** The project just added: the terminal area offers to start its first agent. */
  justAdded: null as string | null,

  /** Sessions picked with Ctrl or Shift+click, in click order. */
  selection: [] as string[],
  /** The list item the session list's single Tab stop lands on. */
  roveKey: null as string | null,
  /**
   * The group of the current row's line. A Skipper in a group has two rows
   * with one key: its own row and its line in the group. Null: the row in no group.
   */
  roveAt: null as { key: string; group: string | null } | null,
  /** A pane asked for the keys before its terminal opened. It takes them when it opens. */
  grab: null as string | null,
  /** Focus mode: the terminal area alone, no tree, top bar or status bar. */
  focusMode: false,
  /** The pane that fills the terminal area, or null. The window chrome hides too. */
  maximized: null as string | null,
  /** The session focused before the current one, for Back. */
  previous: null as string | null,

  /** Numbers the groups not saved yet. */
  tmpSeq: 0,
  themes: [] as TerminalTheme[],
  /** `[appearance] theme`: a theme id, or null for Harbor. It colors the app and every terminal without its own. */
  appTheme: null as string | null,
  /** Settings shows: the app wears the app theme, not the selected project's. */
  settingsShown: false,
  fontSize: savedFontSize(),
};
