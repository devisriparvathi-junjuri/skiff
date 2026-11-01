/**
 * App updates: a quiet check at the launch, Help › Check for updates…, a
 * top-bar tag while an update exists, and one panel under the tag for every
 * step. The panel stays until the person picks an action, presses Escape or
 * clicks ×. Only the person's click downloads and restarts the app. The new
 * app hands skiffd over at its launch, so sessions keep running.
 */
import { invoke } from "@tauri-apps/api/core";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "../platform/os";
import { $, button, h } from "../ui/dom";
import { leave } from "../workspace/profile";
import { failLine, notesLines, progressText, releasePage } from "./updateNotice";
import { t } from "../i18n";

type State = "idle" | "checking" | "latest" | "off" | "available" | "downloading" | "ready" | "error";

let state: State = "idle";
let update: Update | null = null;
/** The running version, for the panel titles. */
let current = "";
/** Why updates are off, or the error line. */
let line = "";
/** The step a Retry runs again. */
let failed: "check" | "download" | "install" = "check";
/** The update is downloaded, so Relaunch only installs. */
let downloaded = false;
let got = 0;
let total: number | undefined;
/** The panel is open, and what had the keys before it opened. */
let open = false;
let back: HTMLElement | null = null;

/** Called once the launch is done. Shows the tag only when an update exists. */
export function startUpdates() {
  void find(false);
}

/** Help › Check for updates… and the palette entry. A check that runs already shows in the panel. */
export function checkForUpdates() {
  openPop();
  if (state === "idle" || state === "latest" || state === "off" || (state === "error" && failed === "check")) void find(true);
}

/** `asked`: the person chose Check for updates…, so the panel shows each step. The quiet check shows only a found update. */
async function find(asked: boolean) {
  current ||= await getVersion().catch(() => "");
  const off = await invoke<string | null>("update_off").catch((e) => String(e));
  if (off) {
    line = off;
    return set(asked ? "off" : "idle");
  }
  set("checking");
  let found: Update | null;
  try {
    found = await check();
  } catch (e) {
    console.error(e);
    if (!open) return set("idle");
    failed = "check";
    line = failLine("check", e);
    return set("error");
  }
  if (!found) return set(open ? "latest" : "idle");
  update = found;
  set("available");
}

/** Install and relaunch: the download first, with its progress in the tag and the panel. */
async function download() {
  const u = update;
  if (!u) return;
  got = 0;
  total = undefined;
  set("downloading");
  try {
    // The updater checks the signature here, before anything is installed.
    await u.download((e) => {
      if (e.event === "Started") total = e.data.contentLength;
      else if (e.event === "Progress") got += e.data.chunkLength;
      progress();
    });
  } catch (e) {
    console.error(e);
    failed = "download";
    line = failLine("download", e);
    return set("error");
  }
  downloaded = true;
  set("ready");
}

/**
 * Relaunch: the workspace save guard first, then the install, then the
 * restart. Cancel at the guard installs nothing. A failed install, such as a
 * cancelled password prompt for a .deb or .rpm, keeps this app running.
 */
function relaunch() {
  const u = update;
  if (!u) return;
  closePop();
  void leave(t("update.restartsLeave"), async () => {
    try {
      await u.install();
    } catch (e) {
      console.error(e);
      failed = "install";
      line = failLine("install", e);
      openPop();
      set("error");
      return false;
    }
    try {
      await invoke("relaunch_app");
      return true;
    } catch (e) {
      console.error(e);
      failed = "install";
      line = String(e instanceof Error ? e.message : e);
      openPop();
      set("error");
      return false;
    }
  });
}

function retry() {
  if (failed === "check") void find(true);
  else if (failed === "download") void download();
  else relaunch();
}

function set(next: State) {
  state = next;
  badge();
  if (open) fill();
}

/** The tag shows while an update exists, and while the panel is open so the panel has its anchor. */
function badge() {
  const shows = open || state === "available" || state === "downloading" || state === "ready" || (state === "error" && !!update);
  let b = document.getElementById("update-badge") as HTMLButtonElement | null;
  if (!shows) return b?.remove();
  if (!b) {
    b = button("tb-cell update-badge", "", () => (open ? closePop(true) : openPop()));
    b.id = "update-badge";
    b.setAttribute("aria-haspopup", "dialog");
    $("open-settings").before(b);
  }
  b.setAttribute("aria-expanded", String(open));
  b.classList.toggle("err", state === "error");
  const v = update?.version ?? current;
  const parts: [string, string][] = {
    idle: [["", t("update.tagIdle")]],
    checking: [["", t("update.tagChecking")]],
    latest: [["", t("update.tagLatest")]],
    off: [["", t("update.tagOff")]],
    available: [["dot", ""], ["", t("update.tagUpdate")], ["v", v]],
    downloading: [["", t("update.tagUpdate")], ["v", progressText(got, total).pct]],
    ready: [["dot", ""], ["", t("update.relaunch")], ["v", v]],
    error: [["dot", ""], ["", t("update.failed")]],
  }[state] as [string, string][];
  b.replaceChildren(...parts.map(([cls, text]) => h("span", cls, text)));
  if (state === "downloading") b.append(bar("update-badge-bar"));
}

/** A 2px bar; with no size known it sweeps. */
function bar(cls: string) {
  const { frac } = progressText(got, total);
  const el = h("div", `rp-bar ${cls}${frac === undefined ? " sweep" : ""}`);
  const i = h("i");
  if (frac !== undefined) i.style.width = `${Math.round(frac * 100)}%`;
  el.append(i);
  return el;
}

/** A download step: only the counts move, so the keys stay where they are. */
function progress() {
  badge();
  const pop = document.getElementById("update-pop");
  if (!pop || state !== "downloading") return;
  const p = progressText(got, total);
  pop.querySelector(".rp-count")!.textContent = p.size;
  pop.querySelector(".rp-bar")!.replaceWith(bar(""));
  pop.querySelector(".confirm-act")!.textContent = p.pct;
}

function openPop() {
  if (open) return;
  back = document.activeElement as HTMLElement | null;
  open = true;
  badge();
  const pop = h("div", "");
  pop.id = "update-pop";
  pop.setAttribute("role", "dialog");
  pop.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    closePop(true);
  });
  document.body.appendChild(pop);
  fill();
  place();
  window.addEventListener("resize", place);
}

function place() {
  const pop = document.getElementById("update-pop");
  const b = document.getElementById("update-badge");
  if (!pop || !b) return;
  const r = b.getBoundingClientRect();
  pop.style.top = `${r.bottom}px`;
  pop.style.right = `${Math.max(0, window.innerWidth - r.right)}px`;
}

/** The panel for the current step. The first button takes the keys. */
function fill() {
  const pop = document.getElementById("update-pop");
  if (!pop) return;
  const v = update?.version ?? "";
  const close = () => closePop(true);
  const title = (text: string, count = "") => {
    const el = h("div", "confirm-title rp-title update-title");
    const x = button("update-x", "×", close);
    x.setAttribute("aria-label", t("update.close"));
    el.append(h("span", "", text), h("span", "rp-count", count), x);
    pop.setAttribute("aria-label", text);
    return el;
  };
  const body = (text: string) => h("div", "confirm-body", text);
  const foot = (...bs: HTMLButtonElement[]) => {
    const f = h("div", "confirm-foot");
    f.append(...bs);
    return f;
  };
  const go = (label: string, run: () => void) => button("confirm-act go", label, run);
  const plain = (label: string, run: () => void) => button("confirm-cancel", label, run);
  const restarts = t("update.restarts");
  pop.classList.toggle("rp-failed", state === "error");
  let parts: HTMLElement[];
  switch (state) {
    case "idle":
    case "checking":
      parts = [title(t("update.checkingTitle"), current), h("div", "rp-bar sweep", ""), body(t("update.checkingBody", { version: current })), foot(plain(t("update.close"), close))];
      parts[1].append(h("i"));
      break;
    case "latest":
      parts = [title(t("update.latestTitle"), current), body(t("update.latest", { version: current })), foot(plain(t("update.close"), close))];
      break;
    case "off":
      parts = [title(t("update.offTitle"), current), body(line), foot(plain(t("update.close"), close))];
      break;
    case "available": {
      const notes = notesLines(update?.body);
      const list = h("ul", "update-notes");
      for (const n of notes) list.append(h("li", "", n));
      const page = button("update-page", t("update.releaseNotes"), () => void openUrl(releasePage(v)));
      parts = [title(t("update.title", { version: v }), `${current} → ${v}`), ...(notes.length ? [list] : []), page, body(restarts), foot(plain(t("update.later"), close), go(t("update.install"), () => void download()))];
      break;
    }
    case "downloading": {
      const p = progressText(got, total);
      const busy = go(p.pct, () => {});
      busy.disabled = true;
      parts = [title(t("update.downloadingTitle", { version: v }), p.size), bar(""), body(t("update.downloadingBody")), foot(plain(t("update.hide"), close), busy)];
      break;
    }
    case "ready":
      parts = [title(t("update.readyTitle", { version: v }), progressText(got, total).size), body(restarts), foot(plain(t("update.later"), close), go(t("update.relaunch"), relaunch))];
      break;
    case "error": {
      const bs = [plain(t("update.close"), close)];
      if (v) bs.push(plain(t("update.releasePage"), () => void openUrl(releasePage(v))));
      bs.push(go(t("update.retry"), retry));
      parts = [title(t("update.failed"), v), h("div", "rp-cause", line), foot(...bs)];
      break;
    }
  }
  pop.replaceChildren(...parts);
  pop.querySelector<HTMLButtonElement>(".confirm-foot button:not(:disabled)")?.focus();
}

/**
 * Closes the panel. A finished check or a fault leaves no tag behind; a
 * failed download goes back to the found update. `restore`: the keys go back
 * to what had them before the panel opened.
 */
function closePop(restore = false) {
  document.getElementById("update-pop")?.remove();
  window.removeEventListener("resize", place);
  open = false;
  if (state === "latest" || state === "off") state = "idle";
  if (state === "error") state = downloaded ? "ready" : update ? "available" : "idle";
  badge();
  if (restore && back?.isConnected) back.focus();
  back = null;
}
