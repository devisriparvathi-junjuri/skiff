import { launchIcon } from "../appearance/agentIcon";

import { branchName, isFolder } from "./model";

import { newSession } from "../app/daemon";

import { button, h } from "../ui/dom";
import { branchIcon } from "../ui/icons";
import { host } from "../terminal/terminalHost";

import { showError } from "../ui/alerts";
import { newWorktree } from "./menus";

import { addProject } from "../app/panels";
import { openSkiffAgent, SKIFF_AGENT_KEY, skiffAgentChoice, skiffAgentFolder, skiffAgentName } from "../app/skiffAgent";
import { keepRow } from "./selection";
import { openClosedProject } from "./projectClose";

import { S } from "../app/state";
import { checkShownHost, hostName, isRemote, updateShownHost } from "../app/hosts";
import { daemonWarning } from "../app/daemonRestart";
import { hostCan } from "../app/hostCaps";
import { openRemoteNewSession } from "./remoteLaunch";
import { accent, currentProject, launchChoices, shownIds } from "../app/stateQueries";
import { t } from "../i18n";

/** Puts `node` where `{name}` stands in `text`, so a language can move it. */
function around(el: HTMLElement, text: string, name: string, node: Node) {
  const at = text.indexOf(`{${name}}`);
  if (at < 0) return el.append(text);
  el.append(text.slice(0, at), node, text.slice(at + name.length + 2));
}

let welcomeSig = "";
let welcomeBox: HTMLElement | null = null;

/** The Skiff agent's start button, while it is on. It runs in its own folder, so it needs no project. */
function skiffAgentButton(): HTMLButtonElement | null {
  if (!skiffAgentFolder()) return null;
  const b = button("", "", () => {
    keepRow(SKIFF_AGENT_KEY);
    openSkiffAgent();
  });
  b.append(launchIcon(skiffAgentChoice()), skiffAgentName());
  return b;
}

/** Add project and the Skiff agent's button side by side. */
function first(add: HTMLElement, agent: HTMLElement): HTMLElement {
  const row = h("div", "welcome-first");
  const actions = h("div", "welcome-actions");
  actions.appendChild(agent);
  row.append(add, actions);
  return row;
}

/** First run: add a project, or start the Skiff agent. Just added, or nothing focused: start an agent. */
export function renderWelcome() {
  const p = currentProject();
  const w = p?.worktrees.find((x) => x.is_main) ?? p?.worktrees[0];
  // A pane on screen with no project is the Skiff agent's: the welcome does not cover it.
  const none = S.projects.length === 0 && !S.projectsError && !shownIds().length;
  const agent = skiffAgentFolder() ? [skiffAgentName(), skiffAgentChoice()?.id ?? null] : null;
  // Same content: keep the element, and the focus and hover on its buttons.
  const gap = daemonWarning();
  const sig = JSON.stringify([agent, gap?.kind, gap?.message, S.projects.length, S.closedProjects.map((x) => x.name), !!S.projectsError, p?.name, p?.color, w?.path, S.justAdded === p?.name, !!S.focused, shownIds().length, launchChoices().map((a) => a?.id), hostCan("session_lifecycle")]);
  if (sig === welcomeSig && (!welcomeBox || welcomeBox.isConnected)) return;
  welcomeSig = sig;
  host.querySelector(".empty")?.remove();
  welcomeBox?.remove();
  welcomeBox = null;
  const box = h("div", "welcome");
  if (isRemote()) {
    // A remote Host: its sessions, and new ones when its Skiff offers them.
    if (S.focused || shownIds().length) return;
    const can = hostCan("session_lifecycle");
    const text = can
      ? t("welcome.remotePickOrStart", { host: hostName() })
      : t("welcome.remotePick");
    box.append(h("div", "welcome-title", t("welcome.remoteTitle", { host: hostName() })), h("div", "welcome-text", text));
    if (can) {
      const start = button("welcome-primary", t("cmd.newSession"), () => {
        const b = start.getBoundingClientRect();
        openRemoteNewSession(b.left, b.bottom);
      });
      box.append(start);
    } else {
      box.append(...versionGap());
    }
  } else if (none && S.closedProjects.length) {
    // Every project is closed: open one again, or add another.
    const actions = h("div", "welcome-actions");
    for (const c of S.closedProjects) {
      const b = button("", c.name, () => void openClosedProject(c));
      b.style.color = accent(c);
      actions.appendChild(b);
    }
    const add = button("", t("welcome.addProjectLower"), () => void addProject.open());
    actions.appendChild(add);
    const sa = skiffAgentButton();
    if (sa) actions.appendChild(sa);
    box.append(
      h("div", "welcome-title", t("welcome.noOpenTitle")),
      h("div", "welcome-text", t("welcome.noOpenText")),
      actions,
    );
  } else if (none) {
    const add = button("welcome-primary", t("welcome.addProject"), () => void addProject.open());
    const hint = h("div", "welcome-hint", t("welcome.orEdit"));
    hint.appendChild(h("span", "mono", "~/.config/skiff/projects.toml"));
    const sa = skiffAgentButton();
    box.append(
      h("div", "welcome-title", t("welcome.firstTitle")),
      h("div", "welcome-text", t("welcome.firstText")),
      sa ? first(add, sa) : add,
      hint,
    );
  } else if (S.projects.length === 0 && !S.projectsError) {
    // The Skiff agent's pane shows.
    return;
  } else if (p && w && !shownIds().length && (S.justAdded === p.name || !S.focused)) {
    const title = h("div", "welcome-title");
    const name = h("span", "", p.name);
    name.style.color = accent(p);
    if (S.justAdded === p.name) around(title, t("welcome.ready"), "name", name);
    else title.append(name);
    const text = h("div", "welcome-text");
    around(text, t(isFolder(p) ? "welcome.startIn" : "welcome.startInRepo"), "branch", h("span", "mono", branchName(w)));
    const actions = h("div", "welcome-actions");
    for (const a of launchChoices()) {
      const b = button("", "", () => {
        S.justAdded = null;
        newSession(w.path, a?.command ?? null, a?.id ?? "", a ? "agent" : "shell").catch(showError);
      });
      b.append(launchIcon(a), a ? a.id : t("welcome.shell"));
      actions.appendChild(b);
    }
    if (!isFolder(p)) {
      const wt = button("", "", () => newWorktree(p));
      wt.append(branchIcon(), t("welcome.worktree"));
      actions.appendChild(wt);
    }
    box.append(title, text, actions);
  } else if (!S.focused && !shownIds().length) {
    box.append(h("div", "welcome-text", t("welcome.noSession")));
  } else {
    return;
  }
  host.appendChild(box);
  welcomeBox = box;
}

/** Why a remote Host offers no new sessions, and the one step that fixes it
 *  from here. A Skiff that Skiff installed: Update Skiff…, the same update
 *  as in Settings › Hosts. One from its own install, or a newer one: the
 *  message only, as the badge says it. Neither: the check list. */
function versionGap(): HTMLElement[] {
  const w = daemonWarning();
  const name = hostName();
  if (w?.kind === "update") {
    return [
      h("div", "welcome-hint", t("welcome.hostOlder", { name })),
      button("welcome-primary", t("skiffAgent.updateSkiff"), () => updateShownHost()),
    ];
  }
  if (w?.kind === "remote") return [h("div", "welcome-hint", w.message)];
  return [
    h("div", "welcome-hint", t("welcome.noSessions", { name })),
    button("welcome-primary", t("hosts.checkHost", { name }), () => checkShownHost()),
  ];
}
