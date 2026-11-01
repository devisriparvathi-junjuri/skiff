/**
 * The keymap. The terminal owns Ctrl+letter; Skiff uses Ctrl+Shift+letter
 * (the Command key is its macOS equivalent). `[keys]` overrides any
 * action: `palette = "ctrl+shift+p"`.
 *
 * Primary-modifier shortcuts also use Shift. This leaves bare Ctrl sequences
 * for the terminal. Pane focus also uses Alt to keep Ctrl+Shift+Arrow for
 * terminal selection. `[keys]` can override a default when a platform needs it.
 *
 * Quit is the exception on macOS: Cmd+Shift+Q is the system's Log Out, so
 * Quit takes Cmd+Q, as every Mac app does.
 */
import { type MessageKey, t } from "../i18n";

export type Action =
  | "palette" | "new-session" | "new-worktree" | "add-project" | "end-session"
  | "split-right" | "split-down" | "close-pane" | "next-waiting" | "back" | "rename" | "theme"
  | "copy" | "paste" | "find" | "font-bigger" | "font-smaller" | "font-reset"
  | "app-font-bigger" | "app-font-smaller" | "app-font-reset" | "settings" | "quit"
  | "focus-left" | "focus-right" | "focus-up" | "focus-down"
  | "project-menu" | "project-folder" | "project-copy-path" | "project-color" | "project-changes" | "project-files"
  | "region-next" | "region-prev" | "session-next" | "session-prev" | "list-project" | "list-back" | "list-main" | "shortcuts"
  | "focus-mode" | "maximize" | "mission-control" | "skiff-agent" | "ledger" | "sidebar-view" | "images" | "images-gallery";

export const isMac = navigator.userAgent.includes("Macintosh");

export const DEFAULTS: Record<Action, string> = {
  palette: "ctrl+shift+p",
  "new-session": "ctrl+shift+t",
  "new-worktree": "ctrl+shift+n",
  "add-project": "ctrl+shift+o",
  "end-session": "ctrl+shift+k",
  "split-right": "ctrl+shift+d",
  "split-down": "ctrl+shift+s",
  "close-pane": "ctrl+shift+w",
  "next-waiting": "ctrl+shift+j",
  back: "ctrl+shift+b",
  rename: "ctrl+shift+r",
  theme: "ctrl+shift+x",
  copy: isMac ? "ctrl+c" : "ctrl+shift+c",
  paste: isMac ? "ctrl+v" : "ctrl+shift+v",
  find: "ctrl+shift+f",
  "font-bigger": "ctrl+shift+=",
  "font-smaller": "ctrl+shift+-",
  "font-reset": "ctrl+shift+0",
  "app-font-bigger": "ctrl+shift+alt+=",
  "app-font-smaller": "ctrl+shift+alt+-",
  "app-font-reset": "ctrl+shift+alt+0",
  settings: "ctrl+shift+,",
  quit: isMac ? "ctrl+q" : "ctrl+shift+q",
  "focus-left": "ctrl+shift+alt+left",
  "focus-right": "ctrl+shift+alt+right",
  "focus-up": "ctrl+shift+alt+up",
  "focus-down": "ctrl+shift+alt+down",
  "project-menu": "ctrl+shift+m",
  "project-folder": "ctrl+shift+z",
  "project-copy-path": "ctrl+shift+y",
  "project-color": "ctrl+shift+i",
  "project-changes": "ctrl+shift+g",
  "project-files": "ctrl+shift+h",
  "region-next": "f6",
  "region-prev": "shift+f6",
  "session-next": "ctrl+shift+down",
  "session-prev": "ctrl+shift+up",
  "list-project": "ctrl+shift+left",
  "list-back": "ctrl+shift+right",
  "list-main": "ctrl+shift+home",
  shortcuts: "f1",
  "focus-mode": "ctrl+shift+e",
  maximize: "ctrl+shift+enter",
  "mission-control": "ctrl+shift+a",
  "skiff-agent": "ctrl+shift+l",
  ledger: "ctrl+shift+alt+l",
  "sidebar-view": "ctrl+shift+alt+s",
  // No default key: the palette and the pane menu have it. `[keys]` can set one.
  images: "",
  "images-gallery": "",
};

interface Combo { ctrl: boolean; shift: boolean; alt: boolean; key: string }

const ALIASES: Record<string, string> = {
  left: "arrowleft", right: "arrowright", up: "arrowup", down: "arrowdown",
  plus: "=", "+": "=", minus: "-", comma: ",", esc: "escape", space: " ",
};

function parse(spec: string): Combo | null {
  const parts = spec.toLowerCase().split("+").map((p) => p.trim());
  // "ctrl++" splits into an empty last part: the key is "+".
  if (parts.at(-1) === "" && parts.length > 1) parts.splice(-2, 2, "+");
  const key = parts.pop();
  if (!key) return null;
  return {
    ctrl: parts.includes("ctrl"),
    shift: parts.includes("shift"),
    alt: parts.includes("alt"),
    key: ALIASES[key] ?? key,
  };
}

/** The key a keydown means, independent of Shift and keyboard layout for letters and digits. */
function eventKey(e: KeyboardEvent): string {
  const m = /^(Key|Digit)(.)$/.exec(e.code);
  if (m) return m[2].toLowerCase();
  if (e.code === "Equal") return "=";
  if (e.code === "Minus") return "-";
  if (e.code === "Comma") return ",";
  return e.key.toLowerCase();
}

let table: [Action, Combo][] = [];
let labels: Record<string, string> = {};
let combos: Record<string, Combo> = {};

/** Ctrl in configuration means the platform's primary application modifier. */
function primaryHeld(e: KeyboardEvent): boolean {
  return isMac ? e.metaKey : e.ctrlKey;
}

/** Builds the keymap from the defaults and `[keys]` overrides. */
export function setKeymap(overrides: Record<string, string>) {
  const specs: Record<string, string> = { ...DEFAULTS };
  for (const [k, v] of Object.entries(overrides)) if (k in DEFAULTS) specs[k] = v;
  table = [];
  labels = {};
  combos = {};
  for (const [action, spec] of Object.entries(specs) as [Action, string][]) {
    const c = parse(spec);
    if (!c) continue;
    table.push([action, c]);
    labels[action] = label(c);
    combos[action] = c;
  }
}
setKeymap({});

/** The action a keydown triggers, if any. */
export function actionFor(e: KeyboardEvent): Action | null {
  const key = eventKey(e);
  for (const [action, c] of table) {
    if (c.ctrl === primaryHeld(e) && c.shift === e.shiftKey && c.alt === e.altKey && c.key === key) return action;
  }
  return null;
}

function label(c: Combo): string {
  const k = c.key.startsWith("arrow") ? c.key.slice(5)[0].toUpperCase() + c.key.slice(6) : c.key.length === 1 || /^f\d+$/.test(c.key) ? c.key.toUpperCase() : c.key[0].toUpperCase() + c.key.slice(1);
  return [c.ctrl && (isMac ? "⌘" : "Ctrl"), c.alt && "Alt", c.shift && "Shift", k].filter(Boolean).join("+");
}

/** A keydown spelled as keyLabel spells a key, so a menu can match its hints. */
export function eventLabel(e: KeyboardEvent): string {
  return label({ ctrl: primaryHeld(e), shift: e.shiftKey, alt: e.altKey, key: eventKey(e) });
}

/** "Ctrl+Shift+T" (or "⌘+Shift+T"), for menus and the palette. */
export function keyLabel(action: Action): string {
  return labels[action] ?? "";
}

const MENU_KEYS: Record<string, string> = { "=": "Equal", "-": "Minus", ",": "Comma", " ": "Space", escape: "Escape", enter: "Enter", home: "Home", end: "End" };

/** The action's key as the menu bar spells it ("Ctrl+Shift+T"), or "" for none. */
export function menuAccel(action: Action): string {
  const c = combos[action];
  if (!c) return "";
  const k = c.key.startsWith("arrow") ? "Arrow" + c.key[5].toUpperCase() + c.key.slice(6) : MENU_KEYS[c.key] ?? c.key.toUpperCase();
  return [c.ctrl && "CmdOrCtrl", c.alt && "Alt", c.shift && "Shift", k].filter(Boolean).join("+");
}

/** The Skiff agent's name from Settings. Its F1 line reads it. */
let skiffAgentName = "Admiral";
export const setSkiffAgentName = (name: string) => void (skiffAgentName = name);

/** Words with the names Settings sets filled in. */
export const describeText = (key: MessageKey) => t(key, { "skiff-agent": skiffAgentName });

/** What the action does, for the shortcut sheet. Each action has a `key.<action>` line in the catalog. */
export const describe = (action: Action) => describeText(`key.${action}`);

export type KeySection = "file" | "edit" | "project" | "view" | "help";

/** The shortcut sheet's sections for the actions. Grouped as the menu bar is. */
export const DESCRIBE: [KeySection, Action][] = [
  ["file", "new-session"],
  ["file", "new-worktree"],
  ["file", "add-project"],
  ["file", "settings"],
  ["file", "quit"],
  ["edit", "copy"],
  ["edit", "paste"],
  ["edit", "images"],
  ["edit", "images-gallery"],
  ["edit", "find"],
  ["edit", "rename"],
  ["edit", "theme"],
  ["edit", "end-session"],
  ["project", "project-menu"],
  ["project", "project-folder"],
  ["project", "project-copy-path"],
  ["project", "project-color"],
  ["project", "project-changes"],
  ["project", "project-files"],
  ["view", "palette"],
  ["view", "split-right"],
  ["view", "split-down"],
  ["view", "close-pane"],
  ["view", "next-waiting"],
  ["view", "skiff-agent"],
  ["view", "ledger"],
  ["view", "sidebar-view"],
  ["view", "back"],
  ["view", "session-next"],
  ["view", "session-prev"],
  ["view", "list-project"],
  ["view", "list-back"],
  ["view", "list-main"],
  ["view", "focus-left"],
  ["view", "focus-right"],
  ["view", "focus-up"],
  ["view", "focus-down"],
  ["view", "focus-mode"],
  ["view", "maximize"],
  ["view", "mission-control"],
  ["view", "font-bigger"],
  ["view", "font-smaller"],
  ["view", "font-reset"],
  ["view", "app-font-bigger"],
  ["view", "app-font-smaller"],
  ["view", "app-font-reset"],
  ["help", "region-next"],
  ["help", "region-prev"],
  ["help", "shortcuts"],
];
