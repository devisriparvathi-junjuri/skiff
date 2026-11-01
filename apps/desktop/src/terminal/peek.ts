/**
 * The peek: what the main area shows over the panes for a file, folder or
 * change row. It is not a session to the user: no sidebar row, no group, no
 * split. skiffd owns its PTY, flagged `peek`, so it is not saved or listed.
 *
 * The current row owns the preview. `select()` in keyboard.ts starts it. A
 * render only ends it, when the current row is no longer its row. One
 * generation number guards every change to what shows, so a late reply from
 * a row the user left cannot show anything.
 *
 * A preview is a card (folder, an app that opens the file, or a placeholder),
 * a command in a terminal (a diff, a terminal tool), or a card with a command
 * over it. A remote Host's diff is a card too: its git's text, drawn by
 * diffView.ts, with no command on this Mac. A command starts hidden, and what it does decides what shows:
 * - It takes the terminal (raw input or the alternate screen), as `micro`,
 *   `less` or a pager does: a terminal tool. The peek shows. When the tool
 *   exits, the peek closes.
 * - It prints and exits: the peek shows the output until a key.
 * - It prints nothing and exits, or keeps running without the terminal: an
 *   app with a window of its own. The peek never shows, and the app runs on.
 */
import { Channel, invoke } from "@tauri-apps/api/core";
import { handleOsc52 } from "./osc52";
import { copyText, pasteText } from "../platform/clipboard";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import { $, button, h } from "../ui/dom";
import { icon } from "../ui/icons";
import { showError } from "../ui/alerts";
import { appKey, currentRow, itemKey } from "../app/keyboard";
import { render } from "../app/renderRequest";
import { keyLabel, type Action } from "../app/keys";
import { S, sessions } from "../app/state";
import type { SessionInfo } from "../platform/types";
import { toBytes } from "../platform/ipcBytes";
import { openUrl } from "../platform/os";
import type { Preview } from "../workspace/rowActs";
import { rowActs } from "../workspace/rowActs";

import { panes } from "./terminalState";
import { fitShown } from "./terminal";
import { FONT, TERM_THEME } from "./terminalRuntime";
import { t, type MessageKey } from "../i18n";
import { hostInvoke } from "../app/hostCaps";
import { dataEpoch } from "../app/dataPlane";
import { hostName } from "../app/hosts";
import type { HostDiff, HostFile } from "../platform/types";
import { diffBody, diffState } from "./diffView";
import { fileBody } from "./fileView";

/** Where to find tools for a row that has none. */
export const TOOLS_URL = "https://github.com/solutionscay/skiff/wiki/Open-with";

/** A command that has not taken the terminal by now is an app with its own window. */
const APP_AFTER_MS = 3000;

type Opened =
  | { kind: "run"; script: string; cwd: string }
  | { kind: "default" }
  | { kind: "no_app"; message: string };

type FilePreview = { script: string | null; cwd: string; app: string; peek: boolean };

interface Peek {
  /** The preview this command belongs to. */
  gen: number;
  kind: string;
  request: string;
  el: HTMLElement;
  term: Terminal;
  fit: FitAddon;
  resize: ResizeObserver;
  /** The command's session, once skiffd started it. */
  session: string | null;
  /** The program, for prompts and messages. */
  program: string;
  /** On screen. Until then the command runs hidden. */
  shown: boolean;
  exited: boolean;
  /** The tool took the terminal: it closes the peek when it exits. */
  interactive: boolean;
  /** Output came. The terminal may not have parsed it yet. */
  got: boolean;
  timer: number;
}

/** The preview of the current row. Closed: the user closed it, the row stays current. */
interface Active {
  key: string;
  gen: number;
  /** The preview takes the keys when it is ready. */
  keys: boolean;
  closed: boolean;
  /** A ledger result shown over the row: its own preview, read again on a new data connection. */
  result?: { uid: string; title: string };
}

let active: Active | null = null;
let gen = 0;
/** Runs once when the peek of {@link peekText} leaves the screen. */
let afterPeek: (() => void) | null = null;

/** The peek left the screen: Settings opens again when it sent the file. */
function peekEnded() {
  const f = afterPeek;
  afterPeek = null;
  f?.();
}
/** The card over the panes. Under a shown command it is hidden. */
let card: HTMLElement | null = null;
/** The card's own action: the Enter keycap. Its Esc keycap is not one. */
const cardAction = () => card?.querySelector<HTMLButtonElement>("button.file-preview-key, button.peek-open") ?? null;
/** The blob URL of a Host image on the card, revoked when the card goes. */
let cardUrl: string | null = null;
/** The command of the active preview. */
let open: Peek | null = null;
/** File tools kept running off screen while another row is current. */
const parked = new Map<string, Peek>();
/** Commands that ended before their start call returned. */
const early = new Map<string, number | null>();
/** Commands that took the terminal before their start call returned. Tools such as micro do it within milliseconds. */
const earlyInteractive = new Set<string>();
/** Apps let go from the peek. Their session goes when they exit. */
const released = new Set<string>();
/**
 * The last command let go as an app, still hidden and subscribed, with the
 * row it opened on. A terminal tool can be quiet for a while before it takes
 * the terminal: it shows then, if nothing opened on that row since.
 */
let held: { p: Peek; key: string } | null = null;
/** One number per subscription, as panes count theirs. */
let stream = 0;

/** The current row's key; "" with no row. */
function currentKey(): string {
  const row = currentRow();
  return row ? itemKey(row) : "";
}

const live = (g: number) => !!active && active.gen === g && !active.closed;

/** A preview covers the panes. No pane takes the keys, and none counts as seen. */
export function previewOnScreen(): boolean {
  return !!active && !active.closed;
}

/** Which preview is on screen, 0 for none. A new preview or Open changes it. */
export function previewToken(): number {
  return active && !active.closed ? active.gen : 0;
}

/** A peek session that is not the open peek's: left behind, or an app let go. */
export function strayPeek(s: SessionInfo) {
  if (s.id === open?.session || s.id === held?.p.session || [...parked.values()].some((p) => p.session === s.id)) return;
  // An app still running keeps its window. Its session goes when it exits.
  // A terminal tool out of the peek has no screen left: it ends.
  if (s.state === "done" || s.interactive) hostInvoke("session_lifecycle", "kill_session", { session: s.id }).catch(console.error);
  else released.add(s.id);
}

/** Navigation can leave a tool. Other app keys stay out of its terminal. */
export function peekBlocksAction(action: Action | number | null): boolean {
  if (!open?.shown || open.exited || !live(open.gen)) return false;
  return typeof action !== "number" && action !== "close-pane" && action !== "session-next" && action !== "session-prev"
    && action !== "list-project" && action !== "list-back" && action !== "list-main"
    && action !== "region-next" && action !== "region-prev";
}

// ── The controller ──────────────────────────────────────────────────────────

/**
 * The current row became `key`. Its preview shows; any other ends. The same
 * row again keeps what shows, even an Open over it, and only moves the keys.
 * `fresh` (Show diff, Review all changes) shows the row's own preview again.
 * A closed preview starts again.
 */
export function startPreview(key: string | null, keys: boolean, fresh = false) {
  if (key && active?.key === key && !active.closed && !fresh) {
    if (keys) giveKeys();
    else active.keys = false;
    return;
  }
  endPreview();
  const t = rowActs(key)?.preview;
  if (!key || !t) return;
  active = { key, gen: ++gen, keys, closed: false };
  begin(active.gen, t, fresh);
  if (keys) giveKeys();
}

/** After a render: a preview whose row is no longer current ends. Nothing starts here. */
export function syncPreview() {
  if (!active) return;
  if (currentKey() !== active.key) endPreview();
}

function begin(mine: number, preview: Preview, fresh = false) {
  if (preview.kind === "folder") {
    if (preview.local === false) showCard(mine, "folder", preview.path);
    else showCard(mine, "folder", preview.path, t("menu.showInFileManager"), () => openPath(preview.path));
  } else if (preview.kind === "host-diff" && preview.wait && !fresh) {
    const key = active!.key;
    showCard(mine, "diff", `${preview.wt}/${preview.file ?? allChanges(preview.base)}`, t("menu.reviewChanges"), () => startPreview(key, true, true));
  } else if (preview.kind === "host-diff") {
    showHostDiff(mine, preview);
  } else if (preview.kind === "host-file") {
    showHostFile(mine, preview);
  } else if (preview.kind === "diff" && preview.wait && !fresh) {
    // Landing on the row shows the card. Enter, or Review all changes, runs the diff.
    const key = active!.key;
    showCard(mine, "diff", `${preview.wt}/${preview.file ?? allChanges(preview.base)}`, t("menu.reviewChanges"), () => startPreview(key, true, true));
  } else if (preview.kind === "diff") {
    showCard(mine, "diff", `${preview.wt}/${preview.file ?? allChanges(preview.base)}`);
    void invoke<Opened>("open_diff", { path: preview.wt, file: preview.file ?? null })
      .then((o) => { if (live(mine)) handle(o, "diff", preview.file ?? allChanges(preview.base), preview.branch); })
      .catch(failed(mine));
  } else {
    const { path } = preview;
    const name = path.split("/").pop() ?? path;
    const dir = path.slice(0, path.length - name.length - 1);
    // Cover the panes before the setting is known, without an action yet.
    showCard(mine, "file", path);
    void invoke<FilePreview>("preview_file", { path }).then((p) => {
      if (!live(mine)) return;
      if (p.peek && p.script) return void run("file", name, dir, p.script, p.cwd);
      // Selection starts terminal tools only. An app waits for an explicit open.
      showCard(mine, "file", path, t("peek.openIn", { app: p.app }), () => openPath(path));
    }).catch(failed(mine));
  }
}

/** The preview leaves the screen: a file tool parks, other commands end, apps run on. */
function endPreview() {
  if (!active) return;
  active = null;
  clearCard();
  const p = open;
  open = null;
  if (p) retire(p);
  repaintPanes();
  peekEnded();
}

/** Ctrl+Shift+Up/Down, Tab or F6 into the preview: the tool, the diff, or the card's button. */
export function giveKeys(): boolean {
  if (!active || active.closed) return false;
  active.keys = true;
  if (open?.shown) open.term.focus();
  else if (card) (card.querySelector<HTMLElement>(".diff-view-body") ?? cardAction() ?? card).focus();
  // A command still starting takes the keys when it shows.
  return !!(open || card);
}

// The keys left the preview (F6, a click, a menu or dialog): a late show does not take them back.
document.addEventListener("focusin", (e) => {
  if (!active?.keys) return;
  const t = e.target as Node;
  if (!card?.contains(t) && !open?.el.contains(t)) active.keys = false;
});

/** Enter on the row or the card: the card's action runs; a tool or diff takes the keys. */
export function enterPreview() {
  if (!active || active.closed) return;
  const b = !open?.shown ? cardAction() : null;
  if (b) b.click();
  else giveKeys();
}

/**
 * Settings › Skippers › Edit with Open with › Text files in the peek: runs
 * `script` over the current row. `ended` runs once when the peek leaves the
 * screen.
 */
export function peekText(script: string, cwd: string, path: string, ended: () => void) {
  if (!active || active.closed) {
    endPreview();
    active = { key: currentKey(), gen: 0, keys: true, closed: false };
  }
  active.gen = ++gen;
  active.keys = true;
  if (open) open.gen = active.gen;
  afterPeek = ended;
  const name = path.split("/").pop() ?? path;
  handle({ kind: "run", script, cwd }, "file", name, path.slice(0, path.length - name.length - 1));
}

/** Shift+Enter, double-click, the card's button, a menu's Open: opens the path over the current row. */
export function openPath(path: string) {
  // An Open from a row with no preview, or a closed one, still shows its tool there.
  if (!active || active.closed) {
    endPreview();
    active = { key: currentKey(), gen: 0, keys: true, closed: false };
  }
  // Its own generation: a reply still due for the row's preview, or an older Open, shows nothing.
  active.gen = ++gen;
  active.keys = true;
  active.result = undefined;
  const mine = active.gen;
  // The command on screen, or still starting, belongs to the Open now: it may be the same tool.
  if (open) open.gen = mine;
  const name = path.split("/").pop() ?? path;
  const dir = path.slice(0, path.length - name.length - 1);
  void invoke<Opened>("open_file", { path })
    .then((o) => {
      if (!live(mine)) return;
      handle(o, "file", name, dir);
      if (o.kind !== "run") settle(mine);
    })
    .catch((e) => {
      if (!live(mine)) return console.error(e);
      settle(mine);
      showError(e);
    });
}

/** An Open that shows nothing here (an app, an error) leaves no empty preview behind: only a card with an action stays. */
function settle(mine: number) {
  if (!live(mine) || open || cardAction()) return;
  active!.closed = true;
  clearCard();
  repaintPanes();
}

/** A failed request for a preview the user left says nothing. */
const failed = (mine: number) => (e: unknown) => (live(mine) ? showError(e) : console.error(e));

/**
 * Closes the peek and ends its tool. A card under it stays; without one, the
 * preview closes. `hold`: the command runs on hidden, as an app may.
 */
export function closePeek(hold = false) {
  const p = open;
  if (!p) return;
  const hadKeys = keysIn(p.el) || !!card?.contains(document.activeElement);
  open = null;
  if (hold) {
    if (held) drop(held.p, false);
    held = { p, key: active?.key ?? "" };
  } else drop(p, p.shown);
  // A diff leaves its card when it closes, so its row still owns the main area. Enter shows it again.
  const preview = active && !active.closed ? rowActs(active.key)?.preview : undefined;
  if (active && preview?.kind === "diff" && !cardAction()) {
    const key = active.key;
    showCard(active.gen, "diff", `${preview.wt}/${preview.file ?? allChanges(preview.base)}`, preview.file ? t("menu.showDiff") : t("menu.reviewChanges"), () => startPreview(key, true, true));
  }
  // A card with an action stays: the app opened from it runs on.
  if (card && cardAction()) {
    card.hidden = false;
    if (hadKeys || active?.keys) giveKeys();
    return peekEnded();
  }
  if (active) active.closed = true;
  clearCard();
  if (p.shown) repaintPanes();
  peekEnded();
  if (!hadKeys) return;
  // The keys go back to the row the user left.
  const row = currentRow();
  if (row) row.focus();
  else if (S.focused) panes.get(S.focused)?.term.focus();
}

/**
 * The Esc button, and Esc on a card or on a command that did not take the
 * terminal: the preview closes, its command ends, the row stays current.
 * Enter or a click on the row shows it again.
 */
export function closePreview() {
  if (!active || active.closed) return;
  const hadKeys = !!card?.contains(document.activeElement) || (!!open && keysIn(open.el));
  active.closed = true;
  clearCard();
  const p = open;
  open = null;
  if (p) drop(p, p.shown);
  repaintPanes();
  render();
  peekEnded();
  if (!hadKeys) return;
  const row = currentRow();
  if (row) row.focus();
  else if (S.focused) panes.get(S.focused)?.term.focus();
}

/** The Esc keycap at the right of a preview's header. */
function escButton(): HTMLButtonElement {
  const b = button("peek-esc", "Esc", closePreview);
  b.title = t("peek.closeKey", { key: "Esc" });
  b.setAttribute("aria-label", t("peek.close"));
  return b;
}

/**
 * The command took the terminal: Esc is the tool's now. The keycap names the
 * close key, which Skiff takes before the tool does. A click still closes.
 */
function tookTerminal(p: Peek) {
  p.interactive = true;
  const b = p.el.querySelector<HTMLButtonElement>(".peek-esc");
  if (!b) return;
  const key = keyLabel("close-pane");
  b.textContent = key;
  b.title = t("peek.closeKey", { key });
}

/** The word in a preview's header for its kind: file, folder or diff. */
const KIND_NAMES: Record<string, MessageKey> = { file: "peek.kindFile", folder: "peek.kindFolder", diff: "peek.kindDiff" };
const kindName = (kind: string) => (KIND_NAMES[kind] ? t(KIND_NAMES[kind]) : kind);

const keysIn = (el: HTMLElement) => el.contains(document.activeElement) || document.activeElement === document.body;

function repaintPanes() {
  // Refit and repaint the live panes after the overlay leaves the screen.
  fitShown();
  for (const pane of panes.values()) {
    if (!pane.parked && pane.el.isConnected) pane.term.refresh(0, pane.term.rows - 1);
  }
}

// ── The card ────────────────────────────────────────────────────────────────

function clearCard() {
  card?.remove();
  card = null;
  if (cardUrl) URL.revokeObjectURL(cardUrl);
  cardUrl = null;
}

/** A card's frame: its head, with the kind, the name, its folder and Esc. */
function cardFrame(kind: string, path: string, cls = "peek file-preview"): HTMLElement {
  const name = path.split("/").pop() ?? path;
  const dir = path.slice(0, path.length - name.length - 1);
  const el = h("div", cls);
  el.tabIndex = -1;
  el.setAttribute("aria-label", name);
  const head = h("div", "peek-head");
  head.append(h("span", "peek-kind", kindName(kind)), h("span", "peek-title mono", name), h("span", "peek-where mono", dir), escButton());
  el.append(head);
  return el;
}

function showCard(mine: number, kind: string, path: string, label?: string, act?: () => void) {
  if (!live(mine)) return;
  const el = cardFrame(kind, path);
  const body = h("div", "file-preview-body");
  if (label && act) {
    // A keycap you can click, with the action under it: the hint and the button in one.
    const cap = button("file-preview-key", "", act);
    cap.title = t("peek.actKey", { label });
    cap.setAttribute("aria-label", label);
    const arrow = icon(`<path d="M19 5v8H6m0 0 4-4m-4 4 4 4"></path>`);
    arrow.classList.add("file-preview-key-arrow");
    cap.append(h("span", "", "Enter"), arrow);
    const space = label.indexOf(" ");
    const text = h("div", "file-preview-label");
    text.append(h("b", "", space < 0 ? label : label.slice(0, space)), space < 0 ? "" : label.slice(space));
    body.append(cap, text);
  }
  el.append(body);
  mountCard(el);
}

/** Puts `el` up as the card, over the panes. */
function mountCard(el: HTMLElement) {
  // Tab does nothing here. Space opens or closes the row, as on the row. Enter runs the button.
  el.addEventListener("keydown", (e) => {
    if (e.key === "Tab") e.preventDefault();
    else if (e.key === "Escape" && !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey) {
      e.preventDefault();
      closePreview();
    } else if (e.key === " " && !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey) {
      e.preventDefault();
      if (active) rowActs(active.key)?.fold?.();
    }
  });
  const hadKeys = !!card && card.contains(document.activeElement);
  clearCard();
  card = el;
  $("main").appendChild(el);
  if (open?.shown) el.hidden = true;
  else if (hadKeys || active?.keys) giveKeys();
}

/** The whole-worktree diff's title: what it counts from, when not HEAD. */
const allChanges = (base?: string) => (base ? t("peek.allChangesSince", { base }) : t("peek.allChanges"));

type HostDiffPreview = Extract<Preview, { kind: "host-diff" }>;

/**
 * A remote Host's diff: the viewer shows at once, says it loads, then shows
 * the Host's text or why there is none. A reply for a row the user left, or
 * from an older data connection, shows nothing; the latter is asked again.
 */
function showHostDiff(mine: number, p: HostDiffPreview) {
  const el = cardFrame("diff", `${p.wt}/${p.file ?? allChanges(p.base)}`, "peek diff-view");
  showHostView(
    mine,
    el,
    t("peek.loadingDiff", { host: hostName() }),
    () => hostInvoke<HostDiff>("git_diff", "git_diff", { path: p.wt, file: p.file ?? null }),
    (d) => diffBody(d, p.file, p.base),
    t("peek.noDiff"),
  );
}

/**
 * A Host's read in the card `el`: it shows at once, says it loads, then
 * shows `render` of what `read` gave, or why there is none. A reply for a
 * row the user left shows nothing; one from an older data connection is
 * asked again.
 */
function showHostView<T>(mine: number, el: HTMLElement, loading: string, read: () => Promise<T>, render: (value: T) => HTMLElement, none: string) {
  if (!live(mine)) return;
  el.append(diffState(loading));
  mountCard(el);
  const at = dataEpoch();
  const shows = () => live(mine) && card === el;
  const fill = (body: HTMLElement) => {
    const hadKeys = el.contains(document.activeElement);
    el.querySelector(".diff-view-body")?.replaceWith(body);
    if (hadKeys || active?.keys) body.focus();
  };
  const again = () => showHostView(mine, el, loading, read, render, none);
  read()
    .then((value) => {
      if (!shows()) return;
      if (at !== dataEpoch()) return again();
      fill(render(value));
    })
    .catch((e) => {
      if (!shows()) return console.error(e);
      if (at !== dataEpoch()) return again();
      fill(diffState(t("peek.noneReason", { none, error: String(e) }), true));
    });
}

type HostFilePreview = Extract<Preview, { kind: "host-file" }>;

/**
 * Opens a local copy of a Host's file as Settings › Open with says: a
 * configured command (a peek tool runs here, over the row), else the
 * default app. Only the copy's path reaches an opener, never the Host's.
 */
export function openHostCopy(wt: string, file: string) {
  // An Open from a row with no preview, or a closed one, still shows its tool there.
  if (!active || active.closed) {
    endPreview();
    active = { key: currentKey(), gen: 0, keys: true, closed: false };
  }
  active.gen = ++gen;
  active.keys = true;
  active.result = undefined;
  const mine = active.gen;
  if (open) open.gen = mine;
  const name = file.split("/").pop() ?? file;
  hostInvoke<Opened>("files", "open_file_copy", { worktree: wt, file })
    .then((o) => {
      if (!live(mine)) return;
      handle(o, "file", name, wt);
    })
    .catch((e) => (live(mine) ? showError(e) : console.error(e)));
}

/**
 * A Host's file: the viewer shows at once, says it loads, then shows the
 * Host's bounded preview or why there is none. Enter opens a local copy as Open with says.
 * A reply for a row the user left shows nothing; one from an older data
 * connection is asked again.
 */
function showHostFile(mine: number, p: HostFilePreview) {
  const copy = p.copy ? () => openHostCopy(p.wt, p.file) : null;
  showHostRead(mine, p.path, () => hostInvoke<HostFile>("files", "read_file", { worktree: p.wt, file: p.file }), copy);
}

/** The Host's bounded read `read` in the page's viewer, under `title`. `copy`: the Open a copy button. */
function showHostRead(mine: number, title: string, read: () => Promise<HostFile>, copy: (() => void) | null) {
  const el = cardFrame("file", title, "peek diff-view file-view");
  if (copy) {
    const open = button("peek-open", "", copy);
    open.title = t("peek.openCopyTitle");
    open.setAttribute("aria-label", t("menu.openCopy"));
    open.append(h("span", "peek-open-key", "Enter"), " " + t("menu.openCopy"));
    el.querySelector(".peek-esc")?.before(open);
  }
  showHostView(mine, el, t("peek.loadingFile", { host: hostName() }), read, (f) => fileBody(f, (b) => (cardUrl = URL.createObjectURL(b))), t("peek.noPreview"));
}

/**
 * A ledger row's result over the current row, in the page's own viewer: the
 * selected Host reads the path its row records (`ledger@1`). No path is
 * named or opened on this Mac. `title` names the work.
 */
export function openLedgerResult(uid: string, title: string) {
  if (!active || active.closed) {
    endPreview();
    active = { key: currentKey(), gen: 0, keys: true, closed: false };
  }
  active.gen = ++gen;
  active.keys = true;
  active.result = { uid, title };
  // A tool on screen gives way to the result.
  const p = open;
  open = null;
  if (p) retire(p);
  showLedgerResult(active.gen, active.result);
}

function showLedgerResult(mine: number, r: { uid: string; title: string }) {
  const { uid } = r;
  showHostRead(mine, t("peek.result", { title: r.title }), () => hostInvoke<HostFile>("ledger", "ledger_result", { uid }), null);
}

/** The data plane is back: a Host diff, file or ledger result on screen is read again from it. */
export function refreshHostDiff() {
  if (!active || active.closed || !card?.classList.contains("diff-view")) return;
  // A result keeps its own uid and title: the row's preview does not replace it.
  if (active.result) return showLedgerResult(active.gen, active.result);
  const p = rowActs(active.key)?.preview;
  if (p?.kind === "host-diff") showHostDiff(active.gen, p);
  if (p?.kind === "host-file") showHostFile(active.gen, p);
}

// ── Commands ────────────────────────────────────────────────────────────────

/** Takes the peek down. Its command ends, unless it is an app that runs on. */
function drop(p: Peek, end: boolean) {
  if (open === p) open = null;
  if (held?.p === p) held = null;
  if (parked.get(p.request) === p) parked.delete(p.request);
  clearTimeout(p.timer);
  p.resize.disconnect();
  p.term.dispose();
  p.el.remove();
  if (!p.session) return;
  if (end || p.exited) hostInvoke("session_lifecycle", "kill_session", { session: p.session }).catch(console.error);
  else released.add(p.session);
}

/** A command whose preview is gone: an editor parks, a shown command ends, one still starting runs on as an app. */
function retire(p: Peek) {
  if (p.kind === "file" && p.interactive && !p.exited) {
    p.el.hidden = true;
    parked.set(p.request, p);
  } else drop(p, p.shown);
}

function frame(mine: number, kind: string, title: string, where: string, program: string, request: string): Peek {
  if (open) retire(open);
  open = null;
  const el = h("div", "peek pending");
  el.setAttribute("role", "dialog");
  el.setAttribute("aria-label", title);
  const head = h("div", "peek-head");
  head.append(h("span", "peek-kind", kindName(kind)), h("span", "peek-title mono", title), h("span", "peek-where mono", where), escButton());
  const box = h("div", "peek-body");
  el.append(head, box);
  // Laid out but not visible, so the terminal has its real size from the start.
  $("main").appendChild(el);

  const term = new Terminal({
    theme: TERM_THEME,
    fontFamily: FONT,
    fontSize: S.fontSize,
    lineHeight: 1.2,
    cursorBlink: true,
    scrollback: 10000,
    allowProposedApi: true,
  });
  handleOsc52(term);
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(box);
  const p: Peek = {
    gen: mine, kind, request, el, term, fit, resize: new ResizeObserver(() => resized(p)),
    session: null, program, shown: false, exited: false, interactive: false, got: false, timer: 0,
  };
  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== "keydown") return true;
    // After the command ends, any key closes the peek.
    if (p.exited) {
      if (["Shift", "Control", "Alt", "Meta"].includes(e.key)) return false;
      e.preventDefault();
      if (open === p) closePeek();
      return false;
    }
    // Esc closes a diff or printed output. A tool that took the terminal keeps Esc: micro and vim need it.
    if (e.key === "Escape" && !p.interactive && !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey) {
      e.preventDefault();
      if (open === p) closePreview();
      return false;
    }
    // Navigation runs in the window handler. Copy and paste work in the tool.
    const key = appKey(e);
    if (key === "copy" || key === "paste") {
      e.preventDefault();
      if (key === "copy") void copyText(term.getSelection()).catch(showError);
      else void pasteText().then((text) => text && term.paste(text), showError);
      return false;
    }
    return key === null;
  });
  term.onData((data) => {
    if (p.session && !p.exited) hostInvoke("session_terminal", "pty_write", { session: p.session, data }).catch(console.error);
  });
  p.resize.observe(box);
  open = p;
  fit.fit();
  return p;
}

/** Puts the command on screen, if its preview is still the current one. */
function show(p: Peek) {
  if (open !== p || !live(p.gen)) return;
  if (p.shown && !p.el.hidden) return;
  p.el.hidden = false;
  p.shown = true;
  clearTimeout(p.timer);
  p.el.classList.remove("pending");
  if (!p.exited) p.el.classList.add("busy");
  // A placeholder card goes. A card with an action waits under the command.
  if (card && cardAction()) card.hidden = true;
  else clearCard();
  resized(p);
  if (active?.keys) p.term.focus();
}

function resized(p: Peek) {
  if (open !== p) return;
  p.fit.fit();
  if (p.session && !p.exited) hostInvoke("session_terminal", "pty_resize", { session: p.session, cols: p.term.cols, rows: p.term.rows }).catch(console.error);
}

/** Starts `script` for the active preview, hidden. What it does decides whether the peek shows. */
async function run(kind: string, title: string, where: string, script: string, cwd: string) {
  if (!active || active.closed) return;
  const mine = active.gen;
  const request = JSON.stringify([kind, title, where, script, cwd]);
  if (open?.request === request && !open.exited) return show(open);
  const saved = parked.get(request);
  if (saved) {
    if (open) retire(open);
    parked.delete(request);
    saved.gen = mine;
    open = saved;
    return show(saved);
  }
  const program = script.trim().split(/\s+/)[0] ?? kind;
  const p = frame(mine, kind, title, where, program, request);
  let info: SessionInfo;
  try {
    info = await hostInvoke<SessionInfo>("session_lifecycle", "create_session", {
      spec: { label: program, role: "task", cwd, command: "/bin/sh", args: ["-c", script], peek: true, cols: p.term.cols, rows: p.term.rows },
    });
  } catch (e) {
    if (open !== p) return console.error(e);
    closePeek();
    return showError(e);
  }
  // The row changed meanwhile, or a newer command came: this one runs on its own.
  if (open !== p) {
    earlyInteractive.delete(info.id);
    if (info.interactive) hostInvoke("session_lifecycle", "kill_session", { session: info.id }).catch(console.error);
    else released.add(info.id);
    return;
  }
  // A skiffd from before the peek ran it as a plain session, with a shell after it.
  if (!info.peek) {
    hostInvoke("session_lifecycle", "kill_session", { session: info.id }).catch(console.error);
    closePeek();
    return showError(t("peek.oldDaemon"));
  }
  p.session = info.id;
  // Still hidden and still running: an app with its own window. Let it go, but keep watching.
  p.timer = window.setTimeout(() => {
    if (open === p && !p.shown && !p.exited) closePeek(true);
  }, APP_AFTER_MS);
  const n = ++stream;
  const channel = new Channel<unknown>();
  channel.onmessage = (m) => {
    if (open !== p && held?.p !== p && parked.get(p.request) !== p) return;
    const bytes = toBytes(m);
    if (bytes.length) p.got = true;
    p.term.write(bytes, () => invoke("ack_output", { session: info.id, stream: n, bytes: bytes.length }).catch(console.error));
  };
  await hostInvoke("session_terminal", "subscribe_output", { session: info.id, stream: n, onOutput: channel }).catch(showError);
  if (info.interactive || earlyInteractive.delete(info.id)) peekUpdated({ ...info, interactive: true });
  if (early.has(info.id)) {
    const code = early.get(info.id) ?? null;
    early.delete(info.id);
    peekExited(info.id, code);
  }
}

/** skiffd changed a peek session: the command took the terminal. */
export function peekUpdated(info: SessionInfo) {
  if (!info.interactive) return;
  if (held?.p.session === info.id) return reclaim(held);
  // A command let go as an app turned out to be a terminal tool: nothing shows it, so it ends.
  if (released.delete(info.id)) return void hostInvoke("session_lifecycle", "kill_session", { session: info.id }).catch(console.error);
  const p = open;
  if (!p) return;
  // A start is in flight: the update may be its command's, ahead of the reply.
  if (!p.session) return void earlyInteractive.add(info.id);
  if (p.session !== info.id || p.interactive) return;
  tookTerminal(p);
  show(p);
}

/**
 * A held command took the terminal late: a slow terminal tool, not an app.
 * It shows when its row is still current and nothing opened since. Else
 * nothing can show it, and it ends.
 */
function reclaim({ p, key }: { p: Peek; key: string }) {
  held = null;
  if (open || p.gen !== gen || currentKey() !== key) return drop(p, true);
  if (!active || active.closed) active = { key, gen: p.gen, keys: true, closed: false };
  open = p;
  tookTerminal(p);
  show(p);
}

/** The program `sh` could not find, from its message: dash, bash and busybox word it apart. */
function notFound(term: Terminal): string | null {
  const b = term.buffer.active;
  for (let y = 0; y < b.length; y++) {
    const m = b.getLine(y)?.translateToString(true).match(/([^\s:]+): (?:command )?not found/);
    if (m) return m[1];
  }
  return null;
}

/** The terminal holds text: the command printed something. */
function printed(term: Terminal): boolean {
  const b = term.buffer.active;
  for (let y = 0; y < b.length; y++) if (b.getLine(y)?.translateToString(true).trim()) return true;
  return false;
}

/** skiffd says a peek command ended. True when the session was a peek's. */
export function peekExited(id: string, code: number | null): boolean {
  if (released.delete(id)) {
    hostInvoke("session_lifecycle", "kill_session", { session: id }).catch(console.error);
    return true;
  }
  // A held app ended. What it printed was for no one.
  if (held?.p.session === id) {
    held.p.exited = true;
    drop(held.p, true);
    return true;
  }
  const saved = [...parked.values()].find((p) => p.session === id);
  if (saved) {
    saved.exited = true;
    drop(saved, true);
    return true;
  }
  const p = open;
  if (!p) return false;
  // A start is in flight: the exit may be its command's, ahead of the reply.
  if (!p.session && !sessions.has(id)) {
    early.set(id, code);
    return true;
  }
  if (p.session !== id) return false;
  p.exited = true;
  p.el.classList.remove("busy");
  // The last output can still be on its way: it comes on another channel.
  window.setTimeout(() => {
    if (open !== p) return;
    // A tool that took the terminal is done with it, and it cleared up after itself.
    // A pager that quit at once because the text fit (less -F under delta) took the
    // terminal for a moment but left the text on screen: that text is the preview.
    if (p.interactive && code === 0 && !printed(p.term)) return closePeek();
    // Nothing printed, no error: an app took the file and runs on its own.
    if (!p.shown && !code && !p.got && !printed(p.term)) return closePeek();
    // `sh` could not find the program: name it, and where to find tools.
    if (code === 127) {
      const name = notFound(p.term) ?? p.program;
      closePeek();
      return showError(t("peek.notFound", { name }), { label: t("peek.findTool"), run: () => openUrl(TOOLS_URL) });
    }
    show(p);
    if (p.shown) p.term.write(`\r\n\x1b[2m${code ? t("peek.exited", { program: p.program, code }) : t("peek.pressKey")}\x1b[0m`);
  }, 150);
  return true;
}

function handle(o: Opened, kind: string, title: string, where: string) {
  if (o.kind === "run") {
    void run(kind, title, where, o.script, o.cwd);
    return;
  }
  if (o.kind === "no_app") {
    showError(t("peek.noApp", { title }), { label: t("peek.findTool"), run: () => openUrl(TOOLS_URL) });
  }
}
