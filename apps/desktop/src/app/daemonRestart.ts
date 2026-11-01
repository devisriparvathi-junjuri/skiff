/** A daemon that differs from the app: the top-bar badge, and the restart it offers. */
import { invoke } from "@tauri-apps/api/core";
import type { DaemonWarning, SessionInfo } from "../platform/types";
import { muteInput } from "../terminal/terminalRuntime";
import { showError } from "../ui/alerts";
import { dialogFrame } from "../ui/dialogParts";
import { $, button, h } from "../ui/dom";
import { opensAtOnce } from "./daemonNotice";
import { acceptHostIdentity, updateShownHost } from "./hosts";
import { branchName, taskTitle } from "../workspace/model";
import { sessions } from "./state";
import { place } from "./stateQueries";
import { sentences, t, tn, type MessageKey } from "../i18n";

/** Badge text and panel title per warning. */
const WARNINGS: Record<DaemonWarning["kind"], [MessageKey, MessageKey]> = {
  // The app hands an older skiffd over on its own. This shows only when a
  // handover cannot work, and the message says why.
  outdated: ["daemon.badgeOutdated", "daemon.titleOutdated"],
  protocol: ["daemon.badgeProtocol", "daemon.titleProtocol"],
  newer: ["daemon.badgeNewer", "daemon.titleNewer"],
  hung: ["daemon.badgeHung", "daemon.titleHung"],
  failed: ["daemon.badgeFailed", "daemon.titleFailed"],
  remote: ["daemon.badgeRemote", "daemon.titleRemote"],
  identity: ["daemon.badgeIdentity", "daemon.titleIdentity"],
  update: ["daemon.badgeUpdate", "daemon.titleUpdate"],
};

let shown: DaemonWarning | null = null;

/** The warning the badge shows now. `null`: none. */
export const daemonWarning = () => shown;
/** Set by "Restart when idle": the restart waits until no agent works. */
let whenIdle = false;

const live = () => [...sessions.values()].filter((s) => s.state !== "done");
const working = () => live().filter((s) => s.state === "working");

async function restartNow() {
  whenIdle = false;
  muteInput(true);
  try {
    await invoke("restart_daemon");
    location.reload();
  } catch (e) {
    muteInput(false);
    showError(e);
    // The user asked for this restart: a daemon it left hung or lost opens its panel.
    daemonBadge(shown, true);
  }
}

/** Called on every state change. Restarts once the last agent stops working. */
export function restartIfIdle() {
  if (whenIdle && !working().length) void restartNow();
}

/**
 * A cell in the top bar, beside Settings. It opens a panel that says what a
 * restart costs, so the restart stays the user's call. The panel opens on
 * its own only when `requested`: after a step the user asked for. A warning
 * from the launch's own checks waits in the badge.
 */
export function daemonBadge(warning: DaemonWarning | null, requested = false) {
  shown = warning;
  document.getElementById("daemon-badge")?.remove();
  document.getElementById("daemon-pop")?.remove();
  if (!warning) return;
  const badge = button("tb-cell daemon-badge", "", () => (pop.isConnected ? close() : open()));
  badge.id = "daemon-badge";
  const [label, title] = WARNINGS[warning.kind].map((key) => t(key));
  badge.append(h("span", "dot " + (warning.kind === "newer" || warning.kind === "update" ? "idle" : "waiting")), whenIdle ? t("daemon.restartsIdle") : label);
  badge.setAttribute("aria-haspopup", "dialog");
  $("open-settings").before(badge);

  const pop = h("div", "");
  pop.id = "daemon-pop";
  pop.setAttribute("role", "dialog");
  pop.setAttribute("aria-label", title);
  const later = button("confirm-cancel", whenIdle ? t("daemon.keepWaiting") : t("daemon.notNow"), () => close());
  const restart = whenIdle
    ? button("confirm-act", t("daemon.cancelRestart"), () => {
        whenIdle = false;
        daemonBadge(shown);
      })
    : button("confirm-act", t("daemon.restartDots"), () => {
        close();
        // A hung or lost daemon lists nothing: there is nothing to choose from.
        if (warning.kind === "hung" || warning.kind === "failed" || !live().length) void restartNow();
        else restartDialog();
      });
  const foot = h("div", "confirm-foot");
  // This app never restarts or updates skiffd on a remote Host.
  if (warning.kind === "remote") foot.append(later);
  else if (warning.kind === "update")
    foot.append(
      later,
      button("confirm-act go", t("skiffAgent.updateSkiff"), () => {
        close();
        updateShownHost();
      }),
    );
  else if (warning.kind === "identity")
    foot.append(
      later,
      button("confirm-act", t("daemon.acceptIdentity"), () => {
        close();
        void acceptHostIdentity();
      }),
    );
  else foot.append(later, restart);
  const body = whenIdle
    ? tn("daemon.idleWait", working().length)
    : warning.message;
  pop.append(h("div", "confirm-title", title), h("div", "confirm-body", body), foot);

  const outside = (e: MouseEvent) => {
    if (!pop.contains(e.target as Node) && !badge.contains(e.target as Node)) close();
  };
  function open() {
    const r = badge.getBoundingClientRect();
    pop.style.top = `${r.bottom + 4}px`;
    pop.style.right = `${Math.max(8, window.innerWidth - r.right)}px`;
    document.body.appendChild(pop);
    badge.setAttribute("aria-expanded", "true");
    window.addEventListener("mousedown", outside, true);
    later.focus();
  }
  function close() {
    pop.remove();
    badge.setAttribute("aria-expanded", "false");
    window.removeEventListener("mousedown", outside, true);
  }
  pop.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    close();
    badge.focus();
  });
  if (!whenIdle && opensAtOnce(warning.kind, requested)) requestAnimationFrame(open);
}

/** The sessions a restart stops, by project and worktree, with what each one does. */
function sessionList(list: SessionInfo[]): HTMLElement {
  const byPlace = new Map<string, SessionInfo[]>();
  for (const s of list) {
    const at = place(s);
    const key = at ? `${at.project.name} / ${branchName(at.worktree)}` : t("daemon.other");
    byPlace.set(key, [...(byPlace.get(key) ?? []), s]);
  }
  const box = h("div", "restart-list");
  for (const [key, group] of byPlace) {
    box.append(h("div", "restart-place", key));
    for (const s of group) {
      const row = h("div", "restart-row");
      row.append(h("span", "dot " + s.state), h("span", "restart-name", taskTitle(s)), h("span", "restart-state", t(`state.${s.state}`)));
      box.append(row);
    }
  }
  return box;
}

/** Restart now, when idle, or not at all. */
function restartDialog() {
  const back = document.activeElement as HTMLElement | null;
  const list = live();
  const busy = working().length;
  const waiting = list.filter((s) => s.state === "waiting").length;
  const { overlay, panel } = dialogFrame("confirm-panel", "alertdialog", t("daemon.restart"));
  const lines = [tn("daemon.stops", list.length)];
  if (busy) lines.push(tn("daemon.working", busy));
  if (waiting) lines.push(tn("daemon.waiting", waiting));
  const cancel = button("confirm-cancel", t("daemon.cancel"), () => done());
  const idle = button("confirm-cancel", t("daemon.whenIdle"), () => {
    whenIdle = true;
    done();
    restartIfIdle();
    daemonBadge(shown);
  });
  const now = button("confirm-act", t("daemon.restartNow"), () => {
    done();
    void restartNow();
  });
  idle.title = t("daemon.whenIdleTitle");
  const foot = h("div", "confirm-foot");
  foot.append(cancel, ...(busy ? [idle] : []), now);
  panel.append(h("div", "confirm-title", t("daemon.restart")), h("div", "confirm-body", sentences(lines)), sessionList(list), foot);
  overlay.appendChild(panel);
  document.body.appendChild(overlay);

  function done() {
    overlay.remove();
    back?.focus?.();
  }
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) done();
  });
  overlay.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key !== "Escape") return;
    e.preventDefault();
    done();
  });
  cancel.focus();
}
