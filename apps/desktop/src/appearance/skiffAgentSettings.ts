import { h } from "../ui/dom";
import type { AgentInfo } from "../platform/types";
import type { InstructionFile, SkiffAgentSettings, TaskType } from "../app/skiffAgent";
import { DEFAULT_NAME, mainName, skiffAgentChoice } from "../app/skiffAgent";
import { continueLine } from "../terminal/resume";
import { rowTable, sectionHead, tableState, type RowTableState } from "./rowTable";
import { BAD_TASK_TYPE_NAME, fileSize, modelError, modelFlag, TASK_TYPE_NAME } from "./taskTypes";
import { reveal } from "../platform/fileActions";
import { copyText } from "../platform/clipboard";
import { showError } from "../ui/alerts";
import { isRemote } from "../app/hosts";
import { avatarScopes, setAvatarDialog, skipperFace } from "../workspace/skipperFace";
import { scopeName } from "../workspace/model";

/** A task type row as `set_skiff_agent` takes it. Null model: the agent's default. */
export interface TaskEdit {
  type: string;
  agent: string;
  model: string | null;
}

/** One edit, as `set_skiff_agent` takes it. */
export type SkiffAgentEdit =
  | { key: "name" | "skipper_name" | "agent" | "model" | "project_agent" | "project_model"; value: string | null }
  | { key: "continue_on_restore"; value: boolean }
  | { key: "add_task" | "task"; value: TaskEdit }
  | { key: "rename_task"; value: { from: string; to: string } }
  | { key: "remove_task"; value: string }
  | { key: "cache_days"; value: number };

/** What the Instructions table does with a row. */
export interface InstructionActs {
  files: InstructionFile[];
  /** One error line under a row, by its file. */
  errors: Map<string, string>;
  /** False on a Host whose Skiff cannot send its files: the rows show, with no buttons. */
  editable: boolean;
  edit: (f: InstructionFile) => void;
  reset: (f: InstructionFile) => void;
}

/**
 * Settings › Skippers: the folder, the name, the agent and model, Continue
 * on restore, the agent and model of project and fleet Skippers, and the
 * instruction files. `changeFolder` opens the Change… dialog: a change
 * copies, checks and switches, so the ledger and results go with the folder.
 */
export function skiffAgentSection(
  set: SkiffAgentSettings,
  agents: AgentInfo[],
  save: (e: SkiffAgentEdit) => void,
  changeFolder: () => void,
): HTMLElement[] {
  // Enabled agents and the user's own. A custom agent is on when the user added it.
  const choices = agents.filter((a) => a.enabled || a.custom);
  const chosen = skiffAgentChoice(agents, set.agent);

  const name = textRow("Main", set.name ?? "", DEFAULT_NAME, (v) => save({ key: "name", value: v || null }));
  const skipperName = textRow("Project/Fleet", set.skipper_name ?? "", "Skipper", (v) => save({ key: "skipper_name", value: v || null }));

  const main = mainName(set.name);
  const pick = agentPick(`${main}'s agent`, choices, null);
  if (chosen) pick.value = chosen.id;
  pick.addEventListener("change", () => save({ key: "agent", value: pick.value }));
  const agentRow = agentModelRow(pick, chosen ?? undefined, set.model, `${main}'s model`, (v) => save({ key: "model", value: v }));

  // A custom agent has no continue line: the switch shows off.
  const line = chosen ? continueLine(chosen.id, chosen.command) : null;
  const on = !!line && set.continue_on_restore;
  const cont = h("div", "set-row set-open set-agent" + (on ? " on" : "") + (line ? "" : " missing"));
  const sw = h("button", "c-mode set-switch");
  sw.type = "button";
  sw.disabled = !line;
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-checked", String(on));
  sw.setAttribute("aria-label", `Continue the last conversation on restore ${on ? "on" : "off"}`);
  sw.appendChild(h("span", "switch"));
  sw.addEventListener("click", () => save({ key: "continue_on_restore", value: !on }));
  const what = line
    ? h("span", "c-cmd mono", line)
    : h("span", "c-cmd", chosen ? `${chosen.id} has no continue command.` : "No agent is on.");
  cont.append(h("span", "c-name", "Continue on restore"), what, sw);
  return [...folderRows(set, changeFolder), name, skipperName, agentRow, cont];
}

/**
 * Settings › Skippers › Project and fleet Skippers: one agent and model for
 * both levels. "Same as Admiral" takes the main Skipper's agent. A project's
 * `skipper_agent` in projects.toml still wins for its project Skipper.
 */
export function projectSkippersSection(set: SkiffAgentSettings, agents: AgentInfo[], save: (e: SkiffAgentEdit) => void): HTMLElement[] {
  const head = sectionHead("Project and fleet Skippers");
  const choices = agents.filter((a) => a.enabled || a.custom);
  const pick = agentPick("Project and fleet Skippers' agent", choices, `Same as ${mainName(set.name)}`);
  const own = choices.find((a) => a.id === set.project_agent);
  pick.value = own ? own.id : "";
  pick.addEventListener("change", () => save({ key: "project_agent", value: pick.value || null }));
  const agent = own ?? skiffAgentChoice(agents, set.agent) ?? undefined;
  const row = agentModelRow(pick, agent, set.project_model, "Project and fleet Skippers' model", (v) => save({ key: "project_model", value: v }));
  return [head, row];
}

/**
 * Settings › Skippers › Avatars: each fleet and project
 * Skipper with a folder on this Mac, its face, and Change… for the picker.
 * The faces are kept in those folders: a remote Host shows none here.
 */
export function avatarsSection(set: SkiffAgentSettings, redraw: () => void): HTMLElement[] {
  // The Admiral wears the mascot's head: nothing to change.
  const scopes = isRemote() ? [] : avatarScopes().filter((s) => s);
  if (!scopes.length) return [];
  const word = set.skipper_name || "Skipper";
  const rows = scopes.map((scope) => {
    const label = scope ? `${scopeName(scope)} ${scope.startsWith("fleet:") ? "fleet " : ""}${word}` : mainName(set.name);
    const row = h("div", "set-row set-open set-agent set-avatar on");
    const face = h("span", "c-cmd");
    face.appendChild(skipperFace(scope || null, 26));
    const change = h("button", "set-rm", "Change…");
    change.type = "button";
    change.addEventListener("click", () => setAvatarDialog(scope || null, `Avatar: ${label}`, redraw));
    const acts = h("span", "c-acts");
    acts.append(change);
    row.append(h("span", "c-name", label), face, acts);
    return row;
  });
  return [sectionHead("Avatars", undefined, String(scopes.length)), ...rows];
}

/**
 * Settings › Skippers › Instructions: AGENTS.md, then each fleet's and each
 * open project's SKIPPER.md, in tree order. Edit opens the file, and writes
 * it from the template first when there is none. Reset… shows on an edited
 * file only.
 */
export function instructionsSection(set: SkiffAgentSettings, acts: InstructionActs): HTMLElement[] {
  const edited = acts.files.filter((f) => f.status.startsWith("edited")).length;
  const head = sectionHead("Instructions", "set-h-instructions", `${acts.files.length} ${acts.files.length === 1 ? "file" : "files"} · ${edited} edited`);
  const skipper = mainName(set.name);
  const table = rowTable<InstructionFile>(
    {
      grid: "g-instr",
      noun: "file",
      article: "A file",
      dupHint: "",
      labelledBy: "set-h-instructions",
      rows: acts.files,
      id: (f) => f.file,
      cols: [
        {
          key: "scope",
          head: "SCOPE",
          name: "scope",
          kind: "lead",
          draw: (f) => {
            const cell = h("span", "c-scope", f.scope.kind === "skipper" ? skipper : f.scope.name);
            if (f.scope.kind === "skipper" || f.scope.kind === "fleet") cell.append(h("span", "c-kind", f.scope.kind === "skipper" ? "every Skipper" : "fleet"));
            return cell;
          },
        },
        { key: "file", head: "FILE", name: "file", kind: "preview", get: (f) => f.file },
        { key: "status", head: "STATUS", name: "status", kind: "lead", draw: fileState },
      ],
      // The buttons go straight into the end cell, so they line up with every other row's.
      status: (f) => {
        const box = document.createDocumentFragment();
        if (!acts.editable) return box;
        const edit = h("button", "set-rm", "Edit");
        edit.type = "button";
        edit.setAttribute("aria-label", `Edit ${f.file}`);
        edit.addEventListener("click", () => acts.edit(f));
        box.append(edit);
        if (f.status.startsWith("edited")) {
          const reset = h("button", "set-rm", "Reset…");
          reset.type = "button";
          reset.setAttribute("aria-label", `Reset ${f.file}`);
          reset.addEventListener("click", () => acts.reset(f));
          box.append(reset);
        }
        return box;
      },
    },
    tableState(),
    () => {},
  );
  // One 40px error row under the row it is about.
  for (const [file, why] of acts.errors) {
    const row = [...table.querySelectorAll<HTMLElement>(".set-row")].find((r) => r.querySelector(".c-preview span")?.textContent === file);
    const meta = h("div", "set-error", why);
    meta.setAttribute("role", "alert");
    row?.after(meta);
  }
  return [head, table];
}

/** The STATUS cell: a square and the word. */
function fileState(f: InstructionFile): HTMLElement {
  const cls = f.status === "none" ? " none" : f.status === "template" ? " tpl" : "";
  const cell = h("span", "c-state" + cls);
  cell.append(h("span", "sq"), f.status === "edited-template-changed" ? "edited" : f.status);
  if (f.status === "edited-template-changed") cell.append(h("span", "dim", "· template changed"));
  return cell;
}

/** The agents to pick from, after `first` (value "") when given. */
function agentPick(label: string, choices: AgentInfo[], first: string | null): HTMLSelectElement {
  const pick = h("select", "c-cmd");
  pick.setAttribute("aria-label", label);
  if (first !== null) {
    const o = h("option", "", first);
    o.value = "";
    pick.append(o);
  } else if (!choices.length) pick.append(h("option", "", "No agent is on"));
  for (const a of choices) {
    const o = h("option", "", a.custom ? `${a.id} (custom)` : a.id);
    o.value = a.id;
    pick.append(o);
  }
  pick.disabled = !choices.length && first === null;
  return pick;
}

/**
 * An Agent row: the agent, then its model in the same cell, and "At next
 * start". An empty model is the agent's default. An agent with no model
 * flag shows "in the command" in place of the field.
 */
function agentModelRow(pick: HTMLSelectElement, agent: AgentInfo | undefined, model: string | null, label: string, save: (v: string | null) => void): HTMLElement {
  const row = h("div", "set-row set-open set-agent on");
  const cell = h("span", "c-split");
  cell.append(pick);
  if (modelFlag(agent)) {
    const old = model ?? "";
    const input = h("input", "c-cmd mono");
    input.value = old;
    input.placeholder = "agent's default";
    input.spellcheck = false;
    input.setAttribute("aria-label", label);
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        input.blur();
      } else if (e.key === "Escape") {
        input.value = old;
        input.blur();
      }
    });
    input.addEventListener("blur", () => {
      const v = input.value.trim();
      if (v === old) return;
      if (modelError(v)) {
        input.value = old;
        return void showError(modelError(v)!);
      }
      save(v || null);
    });
    cell.append(input);
  } else cell.append(h("span", "c-note", agent ? "in the command" : ""));
  row.append(h("span", "c-name", "Agent"), cell, h("span", "c-mode", "At next start"));
  return row;
}

/**
 * The Folder row: the path, Copy path, Show in file manager and Change…. A
 * 40px error row under it says why only when the folder is not usable; the counts and
 * safety checks show in the Change… preview. The path is a read-only text field, so it selects,
 * copies and selects all as any field does, with the keys and the field's
 * own right-click menu. Cut and paste do nothing in it: only Change… sets the
 * folder, after its preview.
 */
function folderRows(set: SkiffAgentSettings, change: () => void): HTMLElement[] {
  const row = h("div", "set-row set-open set-agent set-folder on");
  const path = h("input", "c-cmd mono set-path");
  path.type = "text";
  path.readOnly = true;
  path.spellcheck = false;
  path.value = set.configured;
  path.title = set.configured;
  path.setAttribute("aria-label", "Skiff agent folder");
  const acts = h("span", "c-acts");
  const copy = h("button", "set-rm", "Copy path");
  copy.type = "button";
  copy.addEventListener("click", () => void copyText(set.configured).catch(showError));
  acts.append(copy);
  // A remote Host's folder is on that Host: no file manager here opens it.
  if (set.folder_status === "ready" && !isRemote()) {
    const show = h("button", "set-rm", "Show in file manager");
    show.type = "button";
    show.addEventListener("click", () => reveal(set.configured));
    acts.append(show);
  }
  const edit = h("button", "set-rm", "Change…");
  edit.type = "button";
  edit.addEventListener("click", change);
  acts.append(edit);
  row.append(h("span", "c-name", "Folder"), path, acts);
  const why =
    set.folder_status === "other"
      ? "Not usable: it holds other files. Choose another folder."
      : set.folder_status === "refused" || set.folder_status === "missing"
        ? (set.folder_error ?? "Refused.")
        : null;
  if (!why) return [row];
  const meta = h("div", "set-error", why);
  meta.id = "set-folder-why";
  path.setAttribute("aria-invalid", "true");
  path.setAttribute("aria-describedby", meta.id);
  return [row, meta];
}

/** Task types › Edit: opens a row's file. `errors`: one line under a row, by type. */
export interface TypeFileActs {
  edit: (t: TaskType) => void;
  errors: Map<string, string>;
}

/**
 * Settings › Task types: the agent, model and instruction file Skipper
 * starts for each kind of task. `save` answers with the error to show, or null.
 */
export function taskTypesSection(
  set: SkiffAgentSettings,
  agents: AgentInfo[],
  state: RowTableState,
  save: (e: SkiffAgentEdit) => Promise<string | null>,
  redraw: () => void,
  files: TypeFileActs,
): HTMLElement[] {
  // As the Agent row above: enabled agents and the user's own.
  const choices = agents.filter((a) => a.enabled || a.custom);
  const byId = (id: string) => agents.find((a) => a.id === id);
  const label = (a: AgentInfo) => (a.custom ? `${a.id} (custom)` : a.id);
  const head = h("div", "set-head");
  const title = h("div", "set-h", "Task types");
  title.id = "set-h-tasks";
  head.append(title, h("div", "set-sub mono", set.types));
  const row = (t: TaskType, change: Partial<TaskEdit>): TaskEdit => ({ type: t.type, agent: t.agent, model: t.model, ...change });
  const table = rowTable<TaskType>(
    {
      grid: "g-tasks",
      noun: "task type",
      article: "A task type",
      dupHint: "Change it in its row.",
      labelledBy: title.id,
      rows: set.tasks ?? [],
      id: (t) => t.type,
      cols: [
        {
          key: "type",
          head: "TYPE",
          name: "type",
          kind: "key",
          required: true,
          get: (t) => t.type,
          check: (v) => (TASK_TYPE_NAME.test(v) ? null : BAD_TASK_TYPE_NAME),
        },
        {
          key: "agent",
          head: "AGENT",
          name: "agent",
          kind: "select",
          empty: "No agent is on",
          options: (t) => {
            const list: [string, string][] = choices.map((a) => [a.id, label(a)]);
            // A row keeps an agent that is off or gone, so the select shows the truth.
            if (t && !choices.some((a) => a.id === t.agent)) list.unshift([t.agent, `${t.agent} (${byId(t.agent) ? "off" : "gone"})`]);
            return list;
          },
          initial: () => skiffAgentChoice(agents, set.agent)?.id ?? choices[0]?.id ?? "",
          get: (t) => t.agent,
        },
        {
          key: "model",
          head: "MODEL",
          name: "model",
          kind: "text",
          mono: true,
          placeholder: () => "agent's default",
          get: (t) => t.model ?? "",
          // A custom agent sets its model itself. A gone agent has none.
          note: (t, d) => {
            const a = byId(t ? t.agent : (d.agent ?? ""));
            return modelFlag(a) ? null : a ? "set by agent" : "";
          },
          check: modelError,
        },
        {
          key: "instructions",
          head: "INSTRUCTIONS",
          name: "instructions",
          kind: "lead",
          draw: (t) => {
            const cell = h("span", "c-file");
            const none = t.instructions == null;
            const state = h("span", "c-state" + (none ? " none" : ""));
            state.append(h("span", "sq"), none ? "none" : fileSize(t.instructions!));
            const edit = h("button", "set-rm", "Edit");
            edit.type = "button";
            edit.setAttribute("aria-label", `Edit the instructions of ${t.type}`);
            edit.addEventListener("click", () => files.edit(t));
            cell.append(state, edit);
            return cell;
          },
          drawDraft: () => {
            const state = h("span", "c-state none");
            state.append(h("span", "sq"), "none");
            return state;
          },
        },
      ],
      blocked: () => (choices.length ? null : "Turn on an agent in Settings › Agents to add a task type."),
      add: (d) => save({ key: "add_task", value: { type: d.type, agent: d.agent, model: d.model || null } }),
      edit: (t, key, v) => {
        if (key === "type") return save({ key: "rename_task", value: { from: t.type, to: v } });
        // A new agent with no model flag drops the model: skiff-core saves none.
        return save({ key: "task", value: row(t, key === "agent" ? { agent: v } : { model: v || null }) });
      },
      remove: (t) => save({ key: "remove_task", value: t.type }),
    },
    state,
    redraw,
  );
  // One 40px error row under the row it is about.
  for (const [type, why] of files.errors) {
    const row = table.querySelector<HTMLElement>(`.set-row[data-id="${CSS.escape(type)}"]`);
    const meta = h("div", "set-error", why);
    meta.setAttribute("role", "alert");
    row?.after(meta);
  }
  return [head, table];
}

/** A label and a text field that saves on Enter or blur. Esc puts the old value back. */
function textRow(label: string, value: string, placeholder: string, save: (v: string) => void): HTMLElement {
  const row = h("div", "set-row set-open set-agent on");
  const input = h("input", "c-cmd");
  input.value = value;
  input.placeholder = placeholder;
  input.spellcheck = false;
  input.setAttribute("aria-label", `Skiff agent ${label.toLowerCase()}`);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      input.blur();
    } else if (e.key === "Escape") {
      input.value = value;
      input.blur();
    }
  });
  input.addEventListener("blur", () => {
    if (input.value.trim() !== value.trim()) save(input.value.trim());
  });
  row.append(h("span", "c-name", label), input, h("span", "c-mode", ""));
  return row;
}
