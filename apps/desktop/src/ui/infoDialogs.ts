import { dialogFrame, dialogHeader } from "./dialogParts";
import { DESCRIBE, describeText, isMac, keyLabel } from "../app/keys";

import type { DaemonStatus } from "../platform/types";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";

import { h } from "./dom";

/** A plain modal for the shortcut sheet and About. Esc or a click outside closes it. */
function infoDialog(title: string, body: HTMLElement, initialFocus?: HTMLElement) {
  const back = document.activeElement as HTMLElement | null;
  const { overlay, panel } = dialogFrame("info-panel", "dialog", title);
  const head = dialogHeader(title, () => close());
  const scroll = h("div", "info-body");
  scroll.appendChild(body);
  panel.append(head, scroll);
  overlay.appendChild(panel);
  document.body.appendChild(overlay);
  const close = () => {
    overlay.remove();
    back?.focus?.();
  };
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });
  overlay.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Escape" || e.key === "F1") {
      e.preventDefault();
      close();
    }
  });
  if (initialFocus) initialFocus.focus();
  else {
    panel.tabIndex = -1;
    panel.focus();
  }
}

type Shortcut = [section: string, what: string, key: string];

/** Keyboard controls that do not come from the configurable action keymap. */
const FIXED_SHORTCUTS: Shortcut[] = [
  ["Projects", "Project 1 through 9, counted down the list: its Skipper, else its first pane. Folded fleets do not count. In Mission control and the Skippers list, Skipper 1 through 9, counted down the Skippers list", isMac ? "⌘+Option+1…9" : "Ctrl+Shift+1…9"],
  ["Session list", "Open or fold the focused project or fleet", "Enter / Space / ← / →"],
  ["Session list", "Close or remove the project from the project row", "Delete / Backspace"],
  ["Session list", "Open or close the focused worktree", "Enter / Space"],
  ["Session list", "Open the focused session or group", "Enter"],
  ["Session list", "Collapse or expand the focused worktree, Changes, Files or folder", "← / →"],
  ["Session list", "Go into the preview of the focused change, file, folder, Changes or Files", "Enter"],
  ["Session list", "Open or close the focused Changes, Files or folder", "Space"],
  ["Session list", "Give the keys to the preview", "Tab"],
  ["Session list", "Close the preview. A tool that took the terminal keeps Esc", "Esc"],
  ["Session list", "Open the file of the focused change", "Shift+Enter"],
  ["Session list", "Extend the session selection", "Shift+↑ / ↓"],
  ["Session list", "Make a group from the selected sessions", "Enter"],
  ["Session list", "Clear the session selection", "Esc"],
  ["Session list", "Rename the focused session or group", "F2"],
  ["Session list", "End a session or remove a group", "Delete / Backspace"],
  ["Session list", "Open the row menu", "Menu / Shift+F10"],
  ["Ledger", "Next or previous row", "↑ / ↓"],
  ["Ledger", "Open the row's pane or result", "Enter"],
  ["Ledger", "Back to {skiff-agent}'s pane", "Esc"],
  ["Mission control", "The parent, the first child, the card to the left or right", "↑ / ↓ / ← / →"],
  ["Mission control", "Go to the lit session. A cluster: its first session", "Enter"],
  ["Mission control", "Fold or open the lit cluster", "Space"],
  ["Mission control", "Fit the graph, zoom in, zoom out", "Home / = / -"],
  ["Mission control", "Pan, or zoom at the pointer. Drag a card to reorder it in its cluster, or a cluster head to move the cluster inside its fleet or on the canvas", isMac ? "Drag / wheel / ⌘+wheel" : "Drag / wheel / Ctrl+wheel"],
  ["Mission control", "Close. With text in the search, the first Esc clears it", "Esc"],
  ["Images", "Search images", "Type"],
  ["Images", "Go from the search to the list, and back", "↓ / ↑"],
  ["Images", "Next or previous image", "↑ / ↓"],
  ["Images", "Open, Insert, Pin, Copy path, Delete (in the list)", "Enter / I / P / C / Delete"],
  ["Images", "Close Images", "Esc"],
  ["Settings › Images", "Select one more image, or a range", isMac ? "⌘+click / Shift+click" : "Ctrl+click / Shift+click"],
  ["Settings › Images", "Move, or extend the selection", "Arrows / Shift+Arrows"],
  ["Settings › Images", "Add or remove the current image", "Space"],
  ["Settings › Images", "Select all matches", isMac ? "⌘+A" : "Ctrl+A"],
  ["Settings › Images", "Insert, Pin, Copy path, Delete the selection", "I / P / C / Delete"],
  ["Settings", "Change the section from its tab", "↑ / ↓"],
  ["Settings", "Go into the section from its tab", "→"],
  ["Settings", "Move between controls in a section", "↑ / ↓ / ← / →"],
  ["Settings", "Back to the tab from the start of a row", "←"],
  ["Settings", "Cut or select all in a text field", isMac ? "⌘+X / ⌘+A" : "Ctrl+X / Ctrl+A"],
  ["Panes", "Focus mode, or Maximize in a split, and back", "Pane header buttons"],
  ["Panes", "Maximize in a split, focus mode alone, and back", "Double-click the pane header"],
  ["Panes", "Focus mode, Maximize, Add pane, Rename", "Right-click the pane"],
  ["Command palette", "Move through matches", "↑ / ↓"],
  ["Command palette", "Run the selected match", "Enter"],
  ["Command palette", "Close the palette", "Esc"],
  ["Find in terminal", "Find the next or previous match", "Enter / Shift+Enter"],
  ["Find in terminal", "Close the find bar", "Esc"],
  ["Start-session menu", "Move through choices", "↑ / ↓"],
  ["Start-session menu", "Start or open the Skipper", "1"],
  ["Start-session menu", "Start the numbered choice", "2…9"],
  ["Start-session menu", "Close the menu", "Esc"],
  ["Empty pane", "Move through session choices", "↑ / ↓"],
  ["Empty pane", "Start the numbered choice", "1…9"],
  ["Empty pane", "Move to the next empty pane", "Tab / Shift+Tab"],
  ["Empty pane", "Remove the pane", "Delete"],
  ["Menus", "Move through menu items", "↑ / ↓"],
  ["Menus", "Open a submenu", "→ / Enter / Space"],
  ["Menus", "In Add pane and group menus, start the numbered choice", "1…9"],
  ["Menus", "Close a menu or submenu", "Esc / ←"],
  ["Dialogs", "Close the dialog", "Esc"],
  ["Dialogs", "Run the selected dialog action", "Enter"],
];

/** F1: all app controls. Action keys come from the live keymap. */
export function showShortcuts() {
  const body = h("div", "keys-dialog");
  const search = h("input", "keys-search") as HTMLInputElement;
  search.type = "search";
  search.autocomplete = "off";
  search.spellcheck = false;
  search.placeholder = "Find a shortcut";
  search.setAttribute("aria-label", "Find a shortcut");
  const table = h("div", "keys-table");
  const shortcuts: Shortcut[] = [
    ...DESCRIBE.map(([section, action, what]): Shortcut => [section, describeText(what), keyLabel(action)]),
    ...FIXED_SHORTCUTS.map(([section, what, key]): Shortcut => [section, describeText(what), key]),
  ];
  const render = () => {
    const query = search.value.trim().toLocaleLowerCase();
    const shown = shortcuts.filter(([section, what, key]) =>
      `${section} ${what} ${key}`.toLocaleLowerCase().includes(query),
    );
    table.replaceChildren();
    let section = "";
    for (const [sec, what, key] of shown) {
      if (sec !== section) {
        table.appendChild(h("div", "keys-head", sec.toUpperCase()));
        section = sec;
      }
      const row = h("div", "keys-row");
      row.append(h("span", "keys-what", what), h("kbd", "", key));
      table.appendChild(row);
    }
    if (!shown.length) table.appendChild(h("div", "keys-empty", "No shortcuts match."));
    if (!query) table.appendChild(h("div", "keys-note", "Change an action key in projects.toml. For example: [keys] palette = \"ctrl+shift+k\"."));
  };
  search.addEventListener("input", render);
  render();
  body.append(search, table);
  infoDialog("Keyboard shortcuts", body, search);
}

export async function showAbout() {
  const [app, status, config] = await Promise.all([
    getVersion().catch(() => "?"),
    invoke<DaemonStatus>("daemon_status").catch(() => null),
    invoke<string>("config_path").catch(() => "?"),
  ]);
  const table = h("div", "keys-table");
  const row = (what: string, value: string) => {
    const r = h("div", "keys-row");
    r.append(h("span", "keys-what", what), h("span", "about-value mono", value));
    table.appendChild(r);
  };
  row("App", app);
  row("skiffd", status?.version ?? "not reachable");
  row("Socket", status?.socket ?? "?");
  if (status?.pid) row("Pid", String(status.pid));
  row("Config", config);
  table.appendChild(h("div", "keys-note", "Skiff runs coding agents side by side. Sessions live in skiffd and keep running when the window closes."));
  infoDialog("About Skiff", table);
}
