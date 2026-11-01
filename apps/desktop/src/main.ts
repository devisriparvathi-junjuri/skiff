import { configureRender } from "./app/renderRequest";
import { configurePaneActions } from "./terminal/paneActions";
import { configureActionDispatch } from "./app/actionDispatch";
import { configureCommandUi } from "./app/commandUi";
import { runAction } from "./app/actions";
import { focusPane, closePane, dragSessions } from "./workspace/view";
import "@xterm/xterm/css/xterm.css";
import "./styles/fonts.css";
import "./styles.css";

import { keyLabel, setKeymap } from "./app/keys";
import { t } from "./i18n";
import { bySessionPriority } from "./workspace/model";
import type { Appearance, DaemonStatus } from "./platform/types";
import { invoke } from "@tauri-apps/api/core";
import { switcher } from "./app/commands";
import { failureLine, followDataPlane, hostOnline, loadProjects, reconnectRemote, refreshSessions, reloadDaemon, setDaemon, subscribeEvents, untilConnected } from "./app/daemon";
import { connectAttempt, connectDone, connectFailed } from "./app/connectPanel";
import { isRemote, loadHosts, onHostsChanged, onManageHosts, setHostConnected, watchHostLink, watchHosts } from "./app/hosts";
import { muteInput } from "./terminal/terminalRuntime";
import { loadHostCaps } from "./app/hostCaps";
import { $ } from "./ui/dom";
import { showError } from "./ui/alerts";
import { loadGroups } from "./app/groups";
import { initTrace } from "./diagnostics/latency";
import { settings } from "./app/panels";
import { render, scheduleRender } from "./app/render";
import { FONT_DEFAULT, S, sessions } from "./app/state";
import { groupOf } from "./app/stateQueries";

import { setFontSize } from "./terminal/terminalFont";
import { warmPanes } from "./terminal/terminalRuntime";
import { loadThemes } from "./appearance/themes";
import { loadPlace, trackPlace } from "./app/stored";
import { startMemory } from "./diagnostics/memory";
import { watchStatusbar } from "./workspace/status";
import { toggleFocusMode, toggleMaximize } from "./app/modes";
import { nextUnread, nextWaiting, selectProject, showGroup, showSingle } from "./workspace/view";
import { closeMission, goToSession } from "./workspace/missionMotion";
import { missionOpen, restoreRunning } from "./workspace/missionControl";
import { startLedger } from "./workspace/ledger";
import "./terminal/fileDrop";
import { startWorkspaceQuitGuard, startWorkspaces } from "./workspace/profile";
import { startUpdates } from "./app/updater";

configureRender(render);
configurePaneActions({ focusPane, closePane, dragSessions });
configureActionDispatch(runAction);
configureCommandUi(switcher);
// The Host menu's Add SSH Host… and Manage Hosts… open Settings › Hosts. Here, not in
// panels.ts or settings.ts: hosts.ts imports panels.ts through the context menu, so a
// call there runs before hosts.ts has its `manage` and `changed`.
onManageHosts((page) => void settings.open(page));
onHostsChanged(() => settings.hostsChanged());

// Skiff owns right-click. The webview's own menu (Back, Reload, Inspect)
// never shows; text fields keep theirs for cut, copy and paste.
document.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement | null;
  const field = t?.closest("input, textarea:not(.xterm-helper-textarea), [contenteditable=true]");
  if (!field) e.preventDefault();
});

/** The page's static words, in the current language. index.html keeps the English for the first paint. */
function staticText() {
  const set = (sel: string, text: string) => {
    const el = document.querySelector(sel);
    if (el) el.textContent = text;
  };
  // The palette's real hint replaces the search line once skiffd answers.
  set("#open-switcher .spacer", t("switcher.search"));
  set("#next-waiting .label", t("topbar.waiting"));
  set("#next-unread .label", t("topbar.unread"));
  $("next-waiting").title = t("topbar.nextWaiting");
  $("next-unread").title = t("topbar.nextUnread");
  $("open-settings").title = t("cmd.settings");
  $("open-settings").setAttribute("aria-label", t("cmd.settings"));
  // header.ts and daemon.ts write these again later.
  set("#keys-to", t("topbar.keysGoTo"));
  set("#daemon-label", t("status.connecting"));
  $("ledger").setAttribute("aria-label", t("section.ledger"));
}

async function boot() {
  staticText();
  await startWorkspaceQuitGuard();
  // Which Host this window shows decides everything below.
  await loadHosts().catch(showError);
  watchHosts();
  // `tauri dev` build: a badge in the status bar tells it apart from an installed Skiff.
  if (import.meta.env.DEV) {
    const badge = document.createElement("span");
    badge.className = "status-item dev-badge";
    badge.textContent = "DEV";
    badge.title = "Development build (tauri dev)";
    $("statusbar").prepend(badge);
  }
  $("next-waiting").addEventListener("click", () => goToSession(nextWaiting()));
  $("next-unread").addEventListener("click", () => goToSession(nextUnread()));
  $("open-switcher").addEventListener("click", () => switcher.open());
  $("open-settings").addEventListener("click", () => {
    if (missionOpen()) closeMission();
    if (settings.isOpen) settings.close();
    else void settings.open();
  });
  render();

  // The GNOME header bar has its own search button.
  void invoke<{ current: string }>("menu_layout")
    .then((m) => document.body.classList.toggle("header-bar", m.current === "header-bar"))
    .catch(() => null);
  void initTrace();
  startMemory();
  watchStatusbar();
  if (isRemote()) connectAttempt();
  let status = await invoke<DaemonStatus>("daemon_status");
  setDaemon(status);
  if (isRemote() && status.connected) connectDone();
  // An older skiffd that can reload moves onto the app's own first. Its sessions keep running.
  if (status.reload) {
    status = await reloadDaemon();
    // A slow adoption answers late. Keep asking, so the page loads without a restart.
    while (!status.connected) {
      await new Promise((r) => setTimeout(r, 2000));
      status = await invoke<DaemonStatus>("daemon_status").catch(() => status);
    }
    setDaemon(status);
  }
  if (!status.connected) {
    if (!isRemote()) return;
    // A remote Host that is down at launch: wait for it, input off.
    setHostConnected(false);
    muteInput(true);
    connectFailed(failureLine(status), 1000);
    status = await untilConnected(1000);
    muteInput(false);
    setHostConnected(true);
  }
  watchHostLink(reconnectRemote, hostOnline);

  // What the selected Host offers decides which session surfaces it gets.
  await loadHostCaps().catch(console.error);
  // The selected Host's ledger (M6), This Mac's or a remote one's.
  startLedger();
  await subscribeEvents();

  await refreshSessions();
  await Promise.all([
    loadProjects(),
    loadThemes(),
    invoke<Appearance>("get_appearance")
      .then((a) => {
        S.appTheme = a.theme;
        restoreRunning(a.mission_filter);
        // No size in the daemon yet: keep this machine's and hand it over.
        if (a.font_size) setFontSize(a.font_size, false);
        else if (S.fontSize !== FONT_DEFAULT) setFontSize(S.fontSize);
      })
      .catch(() => null),
    invoke<Record<string, string>>("get_keys").then(setKeymap).catch(() => null),
  ]);
  // The top bar's search button is the palette; show its real key.
  const kbd = document.querySelector("#open-switcher kbd");
  if (kbd) kbd.textContent = keyLabel("palette").replace(/\+/g, " ");
  const hint = document.querySelector("#open-switcher .spacer");
  if (hint) hint.textContent = t("switcher.hint");
  // Projects and Git come back on their own when skiff-host starts late or restarts.
  followDataPlane();
  await loadGroups();
  restorePlace();
  await startWorkspaces();
  trackPlace();
  warmPanes();
  startUpdates();

  let refreshing = false;
  setInterval(() => {
    if (refreshing) return;
    refreshing = true;
    refreshSessions()
      .then((changed) => changed && scheduleRender("poll"), console.error)
      .finally(() => (refreshing = false));
  }, 10_000);
}

/** Back to the project, session and group open when the app last ran. Without
 *  one, the first session by priority. */
function restorePlace() {
  const last = loadPlace();
  const s = last.session ? sessions.get(last.session) : undefined;
  const lastGroup = last.group ? S.groups.find((x) => x.id === last.group) : undefined;
  const hasProject = !!last.project && S.projects.some((p) => p.name === last.project);
  if (lastGroup && (last.picked || !s)) showGroup(lastGroup.id, !!last.picked, s?.id);
  else if (s) {
    const g = groupOf(s.id);
    if (g) showGroup(g.id, false, s.id);
    else showSingle(s.id);
  } else if (hasProject) selectProject(last.project!);
  else {
    const first = [...sessions.values()].sort(bySessionPriority)[0];
    const g = first && groupOf(first.id);
    if (first) g ? showGroup(g.id, false, first.id) : showSingle(first.id);
  }
  // The project on screen may not be the open session's.
  if (hasProject && S.selectedProject !== last.project) {
    S.selectedProject = last.project!;
    render();
  }
  // Focus mode or maximize, on the same session.
  if (s && S.focused === s.id) {
    if (last.mode === "maximize") toggleMaximize();
    else if (last.mode === "focus") toggleFocusMode();
  }
}

// Show the app once the first data is in. A slow daemon does not hold it past 2 s.
const reveal = () => document.body.classList.add("ready");
setTimeout(reveal, 2000);
boot().catch(showError).finally(reveal);
