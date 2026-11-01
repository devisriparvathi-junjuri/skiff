/** Inline rename of sessions and groups. */
import { agentIcon } from "../appearance/agentIcon";
import { agentName, taskTitle, isProjectSkipper } from "./model";
import type { Group, SessionInfo } from "../platform/types";
import { $, h } from "../ui/dom";
import { showError } from "../ui/alerts";
import { saveGroup } from "../app/groups";
import { findRow, itemKey, listItems } from "../app/keyboard";
import { render } from "../app/render";
import { S, sessions, upsert } from "../app/state";

import { refocusTerminal } from "./view";
import { t } from "../i18n";
import { hostInvoke } from "../app/hostCaps";

/** What the operator has typed so far; a re-render rebuilds the input from it. */
let renameDraft: string | null = null;

/**
 * The list item that held the keys when a rename began; Esc and Enter give them back to it.
 * `group`: the group of its line, for a session with two rows (a Skipper also in a group).
 */
let renameReturn: { key: string; group: string | null } | null = null;

/** The key of the row in rename: a session's id or a group's id. */
export function renamingKey(): string | null {
  return S.renamingSession ?? S.renaming?.id ?? null;
}

function rememberRow() {
  const a = document.activeElement as HTMLElement | null;
  const key = a && $("sidebar-scroll").contains(a) ? itemKey(a) : "";
  renameReturn = key ? { key, group: a!.dataset.inGroup ?? null } : null;
}

/** After a rename ends by key: back to the row it began on, else the terminal. */
export function leaveRename() {
  const back = renameReturn;
  renameReturn = null;
  if (back) S.roveAt = { key: back.key, group: back.group };
  const row = back ? findRow(listItems(), back.key) : undefined;
  if (row) {
    S.roveKey = back!.key;
    row.focus();
  } else refocusTerminal();
}

export function startSessionRename(id: string) {
  // A project Skipper's name is derived: the Skiff agent's name and its project. Nothing to type.
  const s = sessions.get(id);
  if (s && isProjectSkipper(s)) return;
  rememberRow();
  S.renamingSession = id;
  renameDraft = null;
  render();
  requestAnimationFrame(() => {
    const input = document.querySelector<HTMLInputElement>("#sidebar-scroll input.rename-session");
    input?.focus();
    input?.select();
  });
}

async function finishRename(id: string, name: string | null, byKey = false) {
  if (S.renamingSession !== id) return;
  S.renamingSession = null;
  renameDraft = null;
  if (name !== null) {
    try {
      const info = await hostInvoke<SessionInfo>("session_lifecycle", "rename_session", { session: id, name });
      // The session may have ended during the call; do not bring it back.
      if (sessions.has(id)) upsert(info);
    } catch (e) {
      showError(e);
    }
  }
  render();
  if (byKey) leaveRename();
  else refocusTerminal();
}

export function renameRow(s: SessionInfo, color: string): HTMLElement {
  const row = h("div", "session-row renaming");
  row.style.setProperty("--pc", color);
  const input = h("input", "rename-session");
  input.type = "text";
  input.value = renameDraft ?? s.name ?? taskTitle(s);
  input.placeholder = s.title || s.label || agentName(s);
  input.spellcheck = false;
  input.dataset.key = "session-rename";
  input.addEventListener("input", () => (renameDraft = input.value));
  input.setAttribute("aria-label", t("rename.session"));
  let done = false;
  const finish = (name: string | null, byKey = false) => {
    if (done) return;
    done = true;
    void finishRename(s.id, name, byKey);
  };
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.ctrlKey && e.key.toLowerCase() === "a") input.select();
    else if (e.key === "Enter") finish(input.value, true);
    else if (e.key === "Escape") finish(null, true);
    else return;
    e.preventDefault();
  });
  // A re-render replaces the input; only a real blur ends the rename.
  input.addEventListener("blur", () => {
    if (input.isConnected) finish(input.value);
  });
  row.append(agentIcon(s), input);
  return row;
}

export async function renameGroup(id: string, name: string, byKey = false) {
  S.renaming = null;
  const g = S.groups.find((x) => x.id === id);
  const n = name.trim();
  if (g && n && n !== g.name) {
    g.name = n;
    saveGroup(g);
  }
  render();
  if (byKey) leaveRename();
  else refocusTerminal();
}

/** Renames the list item that holds the keys: a session or a group. False for anything else. */
export function renameListItem(el: HTMLElement | null): boolean {
  if (!el || !listItems().includes(el)) return false;
  const g = el.dataset.group ? S.groups.find((x) => x.id === el.dataset.group) : undefined;
  if (el.dataset.session) startSessionRename(el.dataset.session);
  else if (g) startRename(g);
  else return false;
  return true;
}

export function startRename(g: Group) {
  rememberRow();
  S.renaming = { id: g.id, name: g.name };
  render();
  requestAnimationFrame(() => {
    const input = $<HTMLElement>("sidebar-scroll").querySelector<HTMLInputElement>('input[data-key="group-rename"]');
    input?.focus();
    input?.select();
  });
}

export function groupRenameInput(g: Group): HTMLInputElement {
    const r = S.renaming!;
    const input = h("input", "form-input");
    input.type = "text";
    input.value = r.name;
    input.spellcheck = false;
    input.dataset.key = "group-rename";
    input.setAttribute("aria-label", t("rename.group"));
    input.addEventListener("input", () => (r.name = input.value));
    input.addEventListener("blur", () => {
      if (S.renaming === r && input.isConnected) void renameGroup(g.id, r.name);
    });
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.ctrlKey && e.key.toLowerCase() === "a") input.select();
      else if (e.key === "Enter") void renameGroup(g.id, r.name, true);
      else if (e.key === "Escape") {
        S.renaming = null;
        render();
        leaveRename();
      } else return;
      e.preventDefault();
    });
    return input;
}
