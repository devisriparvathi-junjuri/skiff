import { dialogFrame, dialogHeader } from "./dialogParts";
import { DESCRIBE, describe, describeText, isMac, keyLabel } from "../app/keys";
import { type MessageKey, t, upper } from "../i18n";

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

/**
 * A fixed key's F1 line: catalog keys for the section and the words. The key
 * column is a key label, or `{ words }` when it names a mouse act or a word.
 */
type FixedShortcut = [section: MessageKey, what: MessageKey, key: string | { words: MessageKey }];

const keyWords = (key: FixedShortcut[2]) => (typeof key === "string" ? key : t(key.words));

/** Keyboard controls that do not come from the configurable action keymap. */
const FIXED_SHORTCUTS: FixedShortcut[] = [
  ["section.projects", "fixed.projects.select", isMac ? "⌘+Option+1…9" : "Ctrl+Shift+1…9"],
  ["section.sessionList", "fixed.list.openProject", "Enter / Space / ← / →"],
  ["section.sessionList", "fixed.list.closeProject", "Delete / Backspace"],
  ["section.sessionList", "fixed.list.openWorktree", "Enter / Space"],
  ["section.sessionList", "fixed.list.openSession", "Enter"],
  ["section.sessionList", "fixed.list.foldWorktree", "← / →"],
  ["section.sessionList", "fixed.list.intoPreview", "Enter"],
  ["section.sessionList", "fixed.list.openFolder", "Space"],
  ["section.sessionList", "fixed.list.previewKeys", "Tab"],
  ["section.sessionList", "fixed.list.closePreview", "Esc"],
  ["section.sessionList", "fixed.list.openChangeFile", "Shift+Enter"],
  ["section.sessionList", "fixed.list.extendSelection", "Shift+↑ / ↓"],
  ["section.sessionList", "fixed.list.makeGroup", "Enter"],
  ["section.sessionList", "fixed.list.clearSelection", "Esc"],
  ["section.sessionList", "fixed.list.rename", "F2"],
  ["section.sessionList", "fixed.list.end", "Delete / Backspace"],
  ["section.sessionList", "fixed.list.menu", "Menu / Shift+F10"],
  ["section.ledger", "fixed.ledger.step", "↑ / ↓"],
  ["section.ledger", "fixed.ledger.open", "Enter"],
  ["section.ledger", "fixed.ledger.back", "Esc"],
  ["section.missionControl", "fixed.mission.move", "↑ / ↓ / ← / →"],
  ["section.missionControl", "fixed.mission.go", "Enter"],
  ["section.missionControl", "fixed.mission.fold", "Space"],
  ["section.missionControl", "fixed.mission.zoom", "Home / = / -"],
  ["section.missionControl", "fixed.mission.pan", { words: isMac ? "fixed.key.dragWheelMac" : "fixed.key.dragWheel" }],
  ["section.missionControl", "fixed.mission.close", "Esc"],
  ["section.images", "fixed.images.search", { words: "fixed.key.type" }],
  ["section.images", "fixed.images.toList", "↓ / ↑"],
  ["section.images", "fixed.images.step", "↑ / ↓"],
  ["section.images", "fixed.images.actions", "Enter / I / P / C / Delete"],
  ["section.images", "fixed.images.close", "Esc"],
  ["section.imagesGallery", "fixed.gallery.select", isMac ? "⌘+click / Shift+click" : "Ctrl+click / Shift+click"],
  ["section.imagesGallery", "fixed.gallery.move", { words: "fixed.key.arrows" }],
  ["section.imagesGallery", "fixed.gallery.toggle", "Space"],
  ["section.imagesGallery", "fixed.gallery.selectAll", isMac ? "⌘+A" : "Ctrl+A"],
  ["section.imagesGallery", "fixed.gallery.actions", "I / P / C / Delete"],
  ["section.settings", "fixed.settings.section", "↑ / ↓"],
  ["section.settings", "fixed.settings.into", "→"],
  ["section.settings", "fixed.settings.move", "↑ / ↓ / ← / →"],
  ["section.settings", "fixed.settings.back", "←"],
  ["section.settings", "fixed.settings.cut", isMac ? "⌘+X / ⌘+A" : "Ctrl+X / Ctrl+A"],
  ["section.panes", "fixed.panes.buttons", { words: "fixed.key.headerButtons" }],
  ["section.panes", "fixed.panes.doubleClick", { words: "fixed.key.doubleClick" }],
  ["section.panes", "fixed.panes.menu", { words: "fixed.key.rightClick" }],
  ["section.palette", "fixed.palette.move", "↑ / ↓"],
  ["section.palette", "fixed.palette.run", "Enter"],
  ["section.palette", "fixed.palette.close", "Esc"],
  ["section.find", "fixed.find.step", "Enter / Shift+Enter"],
  ["section.find", "fixed.find.close", "Esc"],
  ["section.startMenu", "fixed.startMenu.move", "↑ / ↓"],
  ["section.startMenu", "fixed.startMenu.skipper", "1"],
  ["section.startMenu", "fixed.startMenu.start", "2…9"],
  ["section.startMenu", "fixed.startMenu.close", "Esc"],
  ["section.emptyPane", "fixed.emptyPane.move", "↑ / ↓"],
  ["section.emptyPane", "fixed.emptyPane.start", "1…9"],
  ["section.emptyPane", "fixed.emptyPane.next", "Tab / Shift+Tab"],
  ["section.emptyPane", "fixed.emptyPane.remove", "Delete"],
  ["section.menus", "fixed.menus.move", "↑ / ↓"],
  ["section.menus", "fixed.menus.submenu", "→ / Enter / Space"],
  ["section.menus", "fixed.menus.start", "1…9"],
  ["section.menus", "fixed.menus.close", "Esc / ←"],
  ["section.dialogs", "fixed.dialogs.close", "Esc"],
  ["section.dialogs", "fixed.dialogs.run", "Enter"],
];

/** F1: all app controls. Action keys come from the live keymap. */
export function showShortcuts() {
  const body = h("div", "keys-dialog");
  const search = h("input", "keys-search") as HTMLInputElement;
  search.type = "search";
  search.autocomplete = "off";
  search.spellcheck = false;
  search.placeholder = t("keys.search");
  search.setAttribute("aria-label", t("keys.search"));
  const table = h("div", "keys-table");
  const shortcuts: Shortcut[] = [
    ...DESCRIBE.map(([section, action]): Shortcut => [t(`section.${section}`), describe(action), keyLabel(action)]),
    ...FIXED_SHORTCUTS.map(([section, what, key]): Shortcut => [t(section), describeText(what), keyWords(key)]),
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
        table.appendChild(h("div", "keys-head", upper(sec)));
        section = sec;
      }
      const row = h("div", "keys-row");
      row.append(h("span", "keys-what", what), h("kbd", "", key));
      table.appendChild(row);
    }
    if (!shown.length) table.appendChild(h("div", "keys-empty", t("keys.none")));
    if (!query) table.appendChild(h("div", "keys-note", t("keys.note")));
  };
  search.addEventListener("input", render);
  render();
  body.append(search, table);
  infoDialog(t("keys.title"), body, search);
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
  row(t("about.app"), app);
  row(t("about.daemon"), status?.version ?? t("about.unreachable"));
  row(t("about.socket"), status?.socket ?? "?");
  if (status?.pid) row(t("about.pid"), String(status.pid));
  row(t("about.config"), config);
  table.appendChild(h("div", "keys-note", t("about.note")));
  infoDialog(t("about.title"), table);
}
