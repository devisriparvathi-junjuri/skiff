import { DEFAULT_THEME, isLight, mix, readable } from "./colors";
/** Terminal and app themes. */
import { applyAppTheme } from "./appTheme";
import { sessionsOf } from "../workspace/layout";

import { scopeFleet, taskTitle } from "../workspace/model";
import { pickTheme } from "./themeCards";
import type { Group, Project, SessionInfo, TerminalTheme } from "../platform/types";
import { invoke } from "@tauri-apps/api/core";
import { showError } from "../ui/alerts";
import { render } from "../app/renderRequest";
import { S, sessions } from "../app/state";
import { currentProject, place } from "../app/stateQueries";
import { panes } from "../terminal/terminalState";
import { skiffAgentSession } from "../app/skiffAgent";
import { t } from "../i18n";
import { hostInvoke } from "../app/hostCaps";

const knownTheme = (id: string | null) => (S.themes.some((t) => t.id === id) ? id! : DEFAULT_THEME);

export async function loadThemes() {
  const list = await invoke<TerminalTheme[]>("list_themes").catch(() => S.themes);
  S.themes = list.map((t) => ({ ...t, source: t.source === "file" ? "file" : "built-in" }));
  // Skipper faces drawn before the themes came in take their colors now.
  render("themes");
}

/** Projects with a `theme` in projects.toml: name to theme id. */
const projectThemes = new Map<string, string>();

/** Each project's own theme, from the Host's rows. Called with every project read. */
export function projectThemesFrom(list: Project[]) {
  projectThemes.clear();
  for (const p of list) if (p.theme) projectThemes.set(p.name, p.theme);
}

/** The project's own theme id, or null when it follows the app. */
export const projectTheme = (p: Project) => projectThemes.get(p.name) ?? null;

/** A project's theme, else the app theme; Ultraviolet if neither is set. */
function themeFor(p: Project | null | undefined): string {
  return knownTheme((p && projectThemes.get(p.name)) || S.appTheme);
}

/** The theme a project's part of the app takes: its own, else the app's. `null`: the app's. */
export function appThemeOf(p: Project | null): TerminalTheme | null {
  const id = themeFor(p);
  return S.themes.find((t) => t.id === id) ?? null;
}

/** A terminal's theme: its own, else its project's, else the app's. */
export function themeIdFor(s: SessionInfo | undefined): string {
  return s?.theme ? knownTheme(s.theme) : themeFor(s && place(s)?.project);
}

let appliedApp: string | undefined;

/** The one pane on screen when it is the Skiff agent's or a fleet Skipper's: a session in no project. */
function noProject(): SessionInfo | undefined {
  if (S.activeGroup || !S.single) return undefined;
  const s = sessions.get(S.single);
  return s && (s.id === skiffAgentSession()?.id || scopeFleet(s.skipper) !== null) ? s : undefined;
}

/**
 * The app's own colors: sidebar, top bar, panels. The selected project's theme
 * wins. The Skiff agent and a fleet Skipper have no project row to select:
 * while one's pane is on screen, the app takes its terminal's theme, its
 * session theme else the app theme. Settings is app scope: while it shows,
 * the app wears the app theme, so a pick there shows at once.
 */
export function applyApp() {
  const want = S.settingsShown ? knownTheme(S.appTheme)
    : noProject() ? themeIdFor(noProject()!) : themeFor(currentProject());
  if (want === appliedApp) return;
  appliedApp = want;
  applyAppTheme(S.themes.find((t) => t.id === want) ?? null);
}

/**
 * The themes that replace the app theme on screen when Settings closes: the
 * selected project's own, and the focused session's own. Each names its scope.
 */
export function themeOverrides(): { id: string; label: string }[] {
  const out: { id: string; label: string }[] = [];
  const app = knownTheme(S.appTheme);
  const p = currentProject();
  const own = p && projectThemes.get(p.name);
  if (own && knownTheme(own) !== app) out.push({ id: knownTheme(own), label: t("theme.scopeProject", { name: p!.name }) });
  const s = S.focused ? sessions.get(S.focused) : undefined;
  if (s?.theme && knownTheme(s.theme) !== app) out.push({ id: knownTheme(s.theme), label: t("theme.scopeSession", { name: taskTitle(s) }) });
  return out;
}

/** The theme id the app colors come from. Icons are inked against it. */
export const appliedTheme = () => appliedApp;

export function xtermTheme(t: TerminalTheme) {
  const [black, red, green, yellow, blue, magenta, cyan, white, brightBlack, brightRed, brightGreen, brightYellow, brightBlue, brightMagenta, brightCyan, brightWhite] = t.palette;
  return {
    foreground: isLight(t.background) ? readable(t.foreground, [t.background], 7) : t.foreground,
    background: t.background,
    cursor: t.cursor ?? t.foreground,
    cursorAccent: t.cursor_foreground,
    selectionBackground: t.selection ?? mix(t.background, t.foreground, 0.25),
    selectionForeground: t.selection_foreground ?? (isLight(t.background) ? readable(t.foreground, [t.selection ?? mix(t.background, t.foreground, 0.25)], 4.5) : undefined),
    black, red, green, yellow, blue, magenta, cyan, white,
    brightBlack, brightRed, brightGreen, brightYellow, brightBlue, brightMagenta, brightCyan, brightWhite,
  };
}

/** Give every open terminal its project's theme. Only changed panes repaint. */
export function applyThemes() {
  for (const [id, pane] of panes) {
    const want = themeIdFor(sessions.get(id));
    if (pane.theme === want) continue;
    const t = S.themes.find((x) => x.id === want) ?? S.themes.find((x) => x.id === DEFAULT_THEME);
    if (!t) continue;
    pane.theme = want;
    pane.term.options.theme = xtermTheme(t);
    // xterm halves this target for ANSI dim text, which light themes use often.
    pane.term.options.minimumContrastRatio = isLight(t.background) ? 7 : 1;
    pane.el.style.background = t.background;
    pane.el.style.colorScheme = isLight(t.background) ? "light" : "dark";
  }
}

/** Right-click a terminal, Session theme: this terminal only. */
export async function sessionThemeMenu(s: SessionInfo) {
  await loadThemes();
  const app = S.themes.find((t) => t.id === themeFor(place(s)?.project));
  pickTheme(t("theme.sessionTitle", { name: taskTitle(s) }), t("theme.sessionScope"), S.themes, {
    active: s.theme,
    reload: async () => { await loadThemes(); return S.themes; },
    none: { label: t("theme.sameAsProject"), theme: app },
    pick: (id) => {
      const live = sessions.get(s.id);
      if (!live) return;
      live.theme = id;
      applyThemes();
      applyApp();
      render();
      hostInvoke<SessionInfo>("session_lifecycle", "set_session_theme", { session: s.id, theme: id }).catch(showError);
    },
  });
}

/** Right-click a group, Group theme: every terminal in it. */
export async function groupThemeMenu(g: Group) {
  await loadThemes();
  const ids = sessionsOf(g.layout);
  const members = ids.map((id) => sessions.get(id)).filter((s): s is SessionInfo => !!s);
  const first = members[0]?.theme ?? null;
  const shared = members.every((s) => s.theme === first) ? first : null;
  const app = S.themes.find((t) => t.id === themeFor(members[0] && place(members[0])?.project));
  pickTheme(t("theme.groupTitle", { name: g.name }), t("theme.groupScope"), S.themes, {
    active: shared,
    reload: async () => { await loadThemes(); return S.themes; },
    none: { label: t("theme.sameAsProject"), theme: app },
    pick: (id) => {
      // Look sessions up now: one may have ended while the picker was open.
      for (const sid of ids) {
        const s = sessions.get(sid);
        if (!s) continue;
        s.theme = id;
        hostInvoke<SessionInfo>("session_lifecycle", "set_session_theme", { session: sid, theme: id }).catch(showError);
      }
      applyThemes();
      applyApp();
      render();
    },
  });
}

/** The main color supplied by the theme, else its foreground. */
export function signatureColor(t: TerminalTheme): string {
  return t.main_color ?? t.foreground;
}

/** The theme this terminal was given by hand, or null when it follows the app. */
export function ownTheme(s: SessionInfo): TerminalTheme | null {
  const id = s.theme;
  return id ? S.themes.find((t) => t.id === id) ?? null : null;
}

/** Right-click a project, Project theme: its chrome and every terminal without its own. */
export async function projectThemeMenu(p: Project) {
  await loadThemes();
  const app = S.themes.find((t) => t.id === (knownTheme(S.appTheme)));
  pickTheme(t("theme.projectTitle", { name: p.name }), t("theme.projectScope"), S.themes, {
    active: projectTheme(p),
    reload: async () => { await loadThemes(); return S.themes; },
    none: { label: t("theme.sameAsApp"), theme: app },
    pick: (id) => {
      if (id) projectThemes.set(p.name, id);
      else projectThemes.delete(p.name);
      applyThemes();
      applyApp();
      render();
      hostInvoke("project_config", "set_project_theme", { project: p.name, theme: id }).catch(showError);
    },
  });
}
