/**
 * Install Skiff… and Update Skiff…: one dialog that asks, then shows the
 * run as it happens. The explanation goes once the run starts; the six
 * steps show from the start, dim until they run, with a 2px bar and "3 / 6
 * · 9 s". The upload line counts its bytes in place. Cancel works until
 * the switch to the new version (step 5). On success the dialog closes and
 * the caller puts the result on the check list row. On failure it stays:
 * title, bar and the failing line turn red, every line stays, one plain
 * line gives the cause, and Close, Copy output and Try again follow.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { dialogFrame } from "../ui/dialogParts";
import { button, h } from "../ui/dom";
import { rune } from "../ui/runes";
import { copyText } from "../platform/clipboard";
import { t } from "../i18n";

/** One event of the run, as Rust sends it (`install::Progress`). */
type Progress =
  | { id: string; kind: "step"; n: number; text: string }
  | { id: string; kind: "line"; text: string }
  | { id: string; kind: "sent"; sent: number; total: number };

/** The step that switches to the new version: no Cancel from it on. */
const SWITCH = 5;

export interface RunDialog<T> {
  id: string;
  /** Before the run: "Install Skiff on devbox?". */
  title: string;
  body: string;
  /** The button that starts it: "Install Skiff". */
  action: string;
  /** While it runs: "Installing Skiff on devbox", and the button "Installing". */
  runningTitle: string;
  working: string;
  /** "Install failed on devbox". */
  failTitle: string;
  /** The six steps' names, shown dim before each runs. */
  steps: string[];
  tone: "go" | "danger";
  run: () => Promise<T>;
  /** It worked: the dialog has closed. `secs`: how long it took. */
  done: (result: T, secs: number, output: string) => void;
  /** It failed and the person pressed Close. */
  failed?: (cause: string, output: string) => void;
}

type Entry = { at: number; text: string; step?: number; bad?: boolean };

export function runDialog<T>(o: RunDialog<T>) {
  const back = document.activeElement as HTMLElement | null;
  const { overlay, panel } = dialogFrame("confirm-panel", o.tone === "danger" ? "alertdialog" : "dialog", o.title);
  const titleText = h("span", "", o.title);
  const count = h("span", "rp-count");
  const title = h("div", "confirm-title rp-title");
  title.append(titleText, count);
  const bar = h("div", "rp-bar");
  const fill = h("i");
  bar.append(fill);
  bar.hidden = true;
  const body = h("div", "confirm-body", o.body);
  const log = h("div", "rp-panel");
  log.hidden = true;
  const lines = h("div", "cp-lines");
  log.append(lines);
  const cause = h("div", "rp-cause");
  cause.setAttribute("role", "alert");
  cause.hidden = true;
  const foot = h("div", "confirm-foot");
  panel.append(title, bar, body, log, cause, foot);
  overlay.append(panel);
  document.body.append(overlay);

  let state: "ask" | "run" | "failed" = "ask";
  let entries: Entry[] = [];
  let step = 0;
  let start = 0;
  let tick = 0;
  let off: (() => void) | null = null;
  let cancelled = false;
  let open = true;

  const close = () => {
    if (!open) return;
    open = false;
    clearInterval(tick);
    off?.();
    overlay.remove();
    back?.focus?.();
  };
  const output = () => entries.map((e) => `${(e.at / 1000).toFixed(1)}  ${e.text}`).concat(cause.hidden ? [] : [cause.textContent ?? ""]).join("\n");

  function feet() {
    foot.replaceChildren();
    if (state === "ask") {
      const cancel = button("confirm-cancel", t("daemon.cancel"), close);
      const act = button(o.tone === "go" ? "confirm-act go" : "confirm-act", o.action, () => void go());
      foot.append(cancel, act);
      act.focus();
    } else if (state === "run") {
      const cancel = button("confirm-cancel", t("daemon.cancel"), () => {
        cancelled = true;
        void invoke("cancel_install", { id: o.id });
      });
      cancel.disabled = step >= SWITCH || cancelled;
      const act = h("button", "confirm-act go busy");
      act.type = "button";
      act.setAttribute("aria-disabled", "true");
      act.append(h("span", "sq"), o.working);
      foot.append(cancel, act);
    } else {
      const shut = button("confirm-cancel", t("update.close"), () => {
        close();
        o.failed?.(cause.textContent ?? "", output());
      });
      foot.append(shut, button("confirm-cancel", t("connect.copyOutput"), () => void copyText(output())), button("confirm-act go", t("connect.tryAgain"), () => void go()));
      shut.focus();
    }
  }

  function draw() {
    const secs = Math.floor((Date.now() - start) / 1000);
    count.textContent = `${Math.min(step, o.steps.length)} / ${o.steps.length} · ${secs} s`;
    fill.style.width = `${(Math.min(step, o.steps.length) / o.steps.length) * 100}%`;
    lines.replaceChildren();
    const last = entries.length - 1;
    entries.forEach((e, i) => {
      const now = i === last && state === "run";
      const row = h("div", "cp-line" + (e.bad ? " bad" : now ? " now" : ""));
      const m = h("span", "cp-mark");
      if (e.bad) m.append(rune("indicators-x", 12));
      else if (now) m.append(h("span", "sq"));
      row.append(m, h("span", "cp-at", (e.at / 1000).toFixed(1)), h("span", "cp-text", e.text));
      lines.append(row);
    });
    // The steps still to run, dim.
    for (let n = step + 1; n <= o.steps.length && state !== "failed"; n++) {
      const row = h("div", "cp-line dim");
      row.append(h("span", "cp-mark"), h("span", "cp-at", ""), h("span", "cp-text", o.steps[n - 1]));
      lines.append(row);
    }
    if (state === "run") log.scrollTop = log.scrollHeight;
  }

  function onProgress(p: Progress) {
    const at = Date.now() - start;
    if (p.kind === "step") {
      step = Math.max(step, p.n);
      entries.push({ at, text: p.text, step: p.n });
      if (p.n >= SWITCH) feet();
    } else if (p.kind === "line") {
      entries.push({ at, text: p.text });
    } else {
      // The upload's line counts its bytes in place.
      const up = [...entries].reverse().find((e) => e.step === 3);
      const mb = (b: number) => (b / 1048576).toFixed(1);
      if (up) up.text = `${up.text.split("  ")[0]}  ${mb(p.sent)} / ${mb(p.total)} MB`;
    }
    draw();
  }

  async function go() {
    state = "run";
    cancelled = false;
    entries = [];
    step = 0;
    start = Date.now();
    body.hidden = true;
    cause.hidden = true;
    log.hidden = bar.hidden = false;
    overlay.classList.remove("rp-failed");
    titleText.textContent = o.runningTitle;
    feet();
    draw();
    clearInterval(tick);
    tick = window.setInterval(draw, 1000);
    off?.();
    off = await listen<Progress>("skiff:host-step", (e) => {
      if (e.payload.id === o.id && state === "run") onProgress(e.payload);
    });
    try {
      const r = await o.run();
      const secs = Math.round((Date.now() - start) / 1000);
      const out = output();
      close();
      o.done(r, secs, out);
    } catch (e) {
      clearInterval(tick);
      off?.();
      off = null;
      if (!open) return;
      if (cancelled) return close();
      state = "failed";
      const last = entries.at(-1);
      if (last) last.bad = true;
      overlay.classList.add("rp-failed");
      titleText.textContent = o.failTitle;
      cause.textContent = String(e);
      cause.hidden = false;
      log.scrollTop = log.scrollHeight;
      draw();
      feet();
    }
  }

  overlay.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Escape" && state !== "run") close();
    else if (e.key === "Tab") {
      const stops = [...foot.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
      const i = stops.indexOf(document.activeElement as HTMLButtonElement);
      stops[(i + (e.shiftKey ? -1 : 1) + stops.length) % stops.length]?.focus();
    } else return;
    e.preventDefault();
  });
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay && state === "ask") close();
  });
  feet();
}
