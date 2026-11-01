/**
 * The connecting panel: while a remote Host connects, the top of the main
 * area shows what happens, as it happens. ssh's own allowlisted lines (Rust
 * filters them, `shown_line`), and the skiffd and skiff-host replies. It
 * shows after 1 s, so a fast connect shows nothing. The status bar repeats
 * the newest line and the elapsed seconds, so it stays visible from
 * Settings. On success it goes in the same frame the tree renders. On
 * failure it stays with every line, the cause, and Check…, Try again and
 * Copy output, until the next try starts.
 */
import { listen } from "@tauri-apps/api/event";
import { $, button, h } from "../ui/dom";
import { rune } from "../ui/runes";
import { copyText } from "../platform/clipboard";
import { checkShownHost, hostName } from "./hosts";
import { t } from "../i18n";

/** Lines the panel shows: the newest eight. */
const SHOWN = 8;
/** A connect shorter than this shows no panel. */
const QUIET_MS = 1000;

type Line = { at: number; text: string };

let active = false;
let failed: { cause: string; next: number } | null = null;
let start = 0;
let lines: Line[] = [];
let tick = 0;
let wake: (() => void) | null = null;
let listening = false;

const secs = (ms: number) => (ms / 1000).toFixed(1);

function ensureListener() {
  if (listening) return;
  listening = true;
  void listen<string>("skiff:connect-line", (e) => {
    if (!active || failed) return;
    lines.push({ at: Date.now() - start, text: e.payload });
    if (lines.length > 200) lines = lines.slice(-200);
    render();
  });
}

/** One try starts. The clock and the lines start again. */
export function connectAttempt() {
  ensureListener();
  active = true;
  failed = null;
  start = Date.now();
  lines = [];
  clearInterval(tick);
  tick = window.setInterval(render, 1000);
  render();
}

/** The try worked: the panel goes now, with no fade. */
export function connectDone() {
  active = false;
  failed = null;
  clearInterval(tick);
  document.getElementById("connect-panel")?.remove();
}

/** The try failed. `cause`: one plain line from the error copy. `retryMs`:
 *  when Skiff tries again on its own. */
export function connectFailed(cause: string, retryMs: number) {
  if (!active) connectAttempt();
  failed = { cause, next: Date.now() + retryMs };
  render();
}

/** Waits `ms` before the next try, or less after Try again. */
export function connectWait(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const t = window.setTimeout(done, ms);
    function done() {
      clearTimeout(t);
      wake = null;
      resolve();
    }
    wake = done;
  });
}

/** The raw lines, for a bug report. */
export function connectOutput(): string {
  const out = lines.map((l) => `${secs(l.at)}  ${l.text}`);
  if (failed) out.push(failed.cause);
  return out.join("\n");
}

function render() {
  if (!active) return;
  const host = hostName();
  const elapsed = Date.now() - start;
  const label = $("daemon-label");
  const newest = lines.at(-1)?.text;
  if (failed) {
    if (label) label.textContent = t("connect.attention", { host });
  } else if (label) {
    label.textContent = `${newest ?? t("connect.connecting", { host })} · ${Math.floor(elapsed / 1000)} s`;
  }
  if (!failed && elapsed < QUIET_MS) return;
  const main = document.getElementById("main");
  if (!main) return;
  let panel = document.getElementById("connect-panel");
  if (!panel) {
    panel = h("section", "connect-panel");
    panel.id = "connect-panel";
    panel.setAttribute("aria-live", "polite");
    main.insertBefore(panel, document.getElementById("terminal-host"));
  }
  const time = panel.querySelector(".cp-time");
  const when = failed
    ? t("connect.nextTry", { n: Math.max(0, Math.ceil((failed.next - Date.now()) / 1000)) })
    : `${Math.floor(elapsed / 1000)} s`;
  // A failed panel keeps its buttons, and the keys on them: only the clock moves.
  if (failed && panel.classList.contains("failed") && time && panel.querySelector(".cp-acts")) {
    time.textContent = when;
    return;
  }
  panel.classList.toggle("failed", !!failed);
  const head = h("div", "cp-head");
  const mark = h("span", "cp-mark");
  mark.append(failed ? rune("indicators-x", 12) : h("span", "sq"));
  head.append(mark, h("span", "cp-title", t(failed ? "connect.cannot" : "connect.connecting", { host })), h("span", "cp-time", when));
  const list = h("div", "cp-lines");
  const shown = failed ? lines : lines.slice(-SHOWN);
  shown.forEach((l, i) => {
    const last = i === shown.length - 1;
    const row = h("div", "cp-line" + (last ? (failed ? " bad" : " now") : ""));
    const m = h("span", "cp-mark");
    if (last && !failed) m.append(h("span", "sq"));
    if (last && failed) m.append(rune("indicators-x", 12));
    row.append(m, h("span", "cp-at", secs(l.at)), h("span", "cp-text", l.text));
    list.append(row);
  });
  panel.replaceChildren(head, list);
  if (failed) {
    const cause = h("div", "cp-cause", failed.cause);
    const acts = h("div", "cp-acts");
    acts.append(
      button("cp-btn", t("connect.check"), () => checkShownHost()),
      button("cp-btn", t("connect.tryAgain"), () => wake?.()),
      button("cp-btn", t("connect.copyOutput"), () => void copyText(connectOutput())),
    );
    panel.append(cause, acts);
  }
}
