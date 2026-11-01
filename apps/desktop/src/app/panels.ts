/** Settings, the launch menu, and the Add project dialog. */
import { startCanvas } from "../workspace/canvas";
import { PRESETS } from "../workspace/layoutSlots";
import { glyph } from "../workspace/layoutIcon";
import { createAddProject } from "../workspace/addProject";
import { createLaunchMenu } from "../ui/launchMenu";
import { createSettings } from "../appearance/settings";
import { invoke } from "@tauri-apps/api/core";
import { hostInvoke } from "./hostCaps";
import { loadProjects, newSession } from "./daemon";
import { host } from "../terminal/terminalHost";
import { showError } from "../ui/alerts";
import { render, scheduleRender } from "./render";
import { collapsed, S, selectedWorktree } from "./state";
import { launchChoices } from "./stateQueries";

import { loadThemes, themeOverrides } from "../appearance/themes";
import { refocusTerminal, selectProject } from "../workspace/view";
import { includeAdded } from "../workspace/profile";
import { skipperStart } from "../workspace/menus";

export const settings = createSettings(
  (a) => {
    S.agents = a;
    render();
  },
  () => refocusTerminal(),
  {
    themes: () => S.themes,
    current: () => S.appTheme,
    overrides: themeOverrides,
    load: loadThemes,
    set: (id) => {
      S.appTheme = id;
      render();
      invoke("set_appearance", { theme: id }).catch(showError);
    },
  },
  // Edit › Copy and Paste work in its text fields while it shows. The app wears the app theme while it shows.
  () => {
    S.settingsShown = settings.isOpen;
    scheduleRender();
  },
);

export const launchMenu = createLaunchMenu({
  choices: launchChoices,
  skipper: (p) => skipperStart(p),
  start: (p, w, agent) => {
    S.selectedProject = p.name;
    selectedWorktree.set(p.name, w.path);
    collapsed.delete(w.path);
    S.justAdded = null;
    newSession(w.path, agent?.command ?? null, agent?.id ?? "", agent ? "agent" : "shell").catch(showError);
  },
  openSettings: () => void settings.open(),
  splits: () => PRESETS.map((s) => ({ name: s.name, glyph: glyph(s.make(), 18, 13) })),
  split: (p, w, i) => startCanvas(p, w, i),
  onClose: () => refocusTerminal(),
});

export const addProject = createAddProject(
  async (p, fleet) => {
    S.justAdded = p.name;
    // From a fleet's +: the project goes in after the fleet's last project.
    if (fleet) {
      const order = S.projects.map((x) => x.name).filter((n) => n !== p.name);
      const last = S.projects.filter((x) => x.fleet === fleet).pop();
      order.splice(last ? order.indexOf(last.name) + 1 : order.length, 0, p.name);
      await hostInvoke("project_config", "reorder_projects", { order, fleets: [[p.name, fleet]] }).catch(showError);
    }
    await loadProjects();
    const added = S.projects.find((x) => x.name === p.name);
    if (added) includeAdded(added);
    selectProject(p.name);
    host.querySelector<HTMLButtonElement>(".welcome button")?.focus();
  },
  () => refocusTerminal(),
);
