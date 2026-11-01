import { h } from "../ui/dom";
import type { AgentInfo } from "../platform/types";
import type { InstructionFile, SkiffAgentSettings, TaskType } from "../app/skiffAgent";
import { DEFAULT_NAME, mainName, skiffAgentChoice } from "../app/skiffAgent";
import { continueLine } from "../terminal/resume";
import { rowTable, sectionHead, tableState, tableWords, type RowTableState } from "./rowTable";
import { t, tn, upper } from "../i18n";
import { badTaskTypeName, fileSize, modelError, modelFlag, TASK_TYPE_NAME } from "./taskTypes";
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

  const name = textRow(t("skipper.name"), t("skipper.nameAria"), set.name ?? "", DEFAULT_NAME, (v) => save({ key: "name", value: v || null }));
  const skipperName = textRow(t("skipper.skipperName"), t("skipper.skipperNameAria"), set.skipper_name ?? "", "Skipper", (v) => save({ key: "skipper_name", value: v || null }));

  const main = mainName(set.name);
  const pick = agentPick(t("skipper.agentOf", { name: main }), choices, null);
  if (chosen) pick.value = chosen.id;
  pick.addEventListener("change", () => save({ key: "agent", value: pick.value }));
  const agentRow = agentModelRow(pick, chosen ?? undefined, set.model, t("skipper.modelOf", { name: main }), (v) => save({ key: "model", value: v }));

  // A custom agent has no continue line: the switch shows off.
  const line = chosen ? continueLine(chosen.id, chosen.command) : null;
  const on = !!line && set.continue_on_restore;
  const cont = h("div", "set-row set-open set-agent" + (on ? " on" : "") + (line ? "" : " missing"));
  const sw = h("button", "c-mode set-switch");
  sw.type = "button";
  sw.disabled = !line;
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-checked", String(on));
  sw.setAttribute("aria-label", t(on ? "skipper.continueOn" : "skipper.continueOff"));
  sw.appendChild(h("span", "switch"));
  sw.addEventListener("click", () => save({ key: "continue_on_restore", value: !on }));
  const what = line
    ? h("span", "c-cmd mono", line)
    : h("span", "c-cmd", chosen ? t("skipper.noContinue", { id: chosen.id }) : t("skipper.noAgentOn"));
  cont.append(h("span", "c-name", t("skipper.continue")), what, sw);
  return [...folderRows(set, changeFolder), name, skipperName, agentRow, cont];
}

/**
 * Settings › Skippers › Project and fleet Skippers: one agent and model for
 * both levels. "Same as Admiral" takes the main Skipper's agent. A project's
 * `skipper_agent` in projects.toml still wins for its project Skipper.
 */
export function projectSkippersSection(set: SkiffAgentSettings, agents: AgentInfo[], save: (e: SkiffAgentEdit) => void): HTMLElement[] {
  const head = sectionHead(t("skipper.projectSkippers"));
  const choices = agents.filter((a) => a.enabled || a.custom);
  const pick = agentPick(t("skipper.projectAgent"), choices, t("skipper.sameAs", { name: mainName(set.name) }));
  const own = choices.find((a) => a.id === set.project_agent);
  pick.value = own ? own.id : "";
  pick.addEventListener("change", () => save({ key: "project_agent", value: pick.value || null }));
  const agent = own ?? skiffAgentChoice(agents, set.agent) ?? undefined;
  const row = agentModelRow(pick, agent, set.project_model, t("skipper.projectModel"), (v) => save({ key: "project_model", value: v }));
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
    const label = scope ? t(scope.startsWith("fleet:") ? "skipper.avatarFleet" : "skipper.avatarProject", { scope: scopeName(scope), skipper: word }) : mainName(set.name);
    const row = h("div", "set-row set-open set-agent set-avatar on");
    const face = h("span", "c-cmd");
    face.appendChild(skipperFace(scope || null, 26));
    const change = h("button", "set-rm", t("skipper.change"));
    change.type = "button";
    change.addEventListener("click", () => setAvatarDialog(scope || null, t("skipper.avatarTitle", { name: label }), redraw));
    const acts = h("span", "c-acts");
    acts.append(change);
    row.append(h("span", "c-name", label), face, acts);
    return row;
  });
  return [sectionHead(t("skipper.avatars"), undefined, String(scopes.length)), ...rows];
}

/**
 * Settings › Skippers › Instructions: AGENTS.md, then each fleet's and each
 * open project's SKIPPER.md, in tree order. Edit opens the file, and writes
 * it from the template first when there is none. Reset… shows on an edited
 * file only.
 */
export function instructionsSection(set: SkiffAgentSettings, acts: InstructionActs): HTMLElement[] {
  const edited = acts.files.filter((f) => f.status.startsWith("edited")).length;
  const head = sectionHead(t("skipper.instructions"), "set-h-instructions", t("skipper.instructionsCount", { files: tn("skipper.files", acts.files.length), edited }));
  const skipper = mainName(set.name);
  const table = rowTable<InstructionFile>(
    {
      grid: "g-instr",
      words: tableWords("file"),
      labelledBy: "set-h-instructions",
      rows: acts.files,
      id: (f) => f.file,
      cols: [
        {
          key: "scope",
          head: upper(t("skipper.colScope")),
          name: t("skipper.scope"),
          kind: "lead",
          draw: (f) => {
            const cell = h("span", "c-scope", f.scope.kind === "skipper" ? skipper : f.scope.name);
            if (f.scope.kind === "skipper" || f.scope.kind === "fleet") cell.append(h("span", "c-kind", t(f.scope.kind === "skipper" ? "skipper.everySkipper" : "skipper.fleet")));
            return cell;
          },
        },
        { key: "file", head: upper(t("skipper.colFile")), name: t("skipper.file"), kind: "preview", get: (f) => f.file },
        { key: "status", head: upper(t("settings.colStatus")), name: t("skipper.status"), kind: "lead", draw: fileState },
      ],
      // The buttons go straight into the end cell, so they line up with every other row's.
      status: (f) => {
        const box = document.createDocumentFragment();
        if (!acts.editable) return box;
        const edit = h("button", "set-rm", t("skipper.edit"));
        edit.type = "button";
        edit.setAttribute("aria-label", t("skipper.editFile", { file: f.file }));
        edit.addEventListener("click", () => acts.edit(f));
        box.append(edit);
        if (f.status.startsWith("edited")) {
          const reset = h("button", "set-rm", t("skipper.reset"));
          reset.type = "button";
          reset.setAttribute("aria-label", t("skipper.resetFile", { file: f.file }));
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
  cell.append(h("span", "sq"), t(f.status === "none" ? "skipper.status.none" : f.status === "template" ? "skipper.status.template" : "skipper.status.edited"));
  if (f.status === "edited-template-changed") cell.append(h("span", "dim", t("skipper.status.templateChanged")));
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
  } else if (!choices.length) pick.append(h("option", "", t("skipper.noAgentOnShort")));
  for (const a of choices) {
    const o = h("option", "", a.custom ? t("skipper.custom", { id: a.id }) : a.id);
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
    input.placeholder = t("skipper.agentDefault");
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
  } else cell.append(h("span", "c-note", agent ? t("skipper.inCommand") : ""));
  row.append(h("span", "c-name", t("skipper.agent")), cell, h("span", "c-mode", t("skipper.atNextStart")));
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
  path.setAttribute("aria-label", t("skipper.folderAria"));
  const acts = h("span", "c-acts");
  const copy = h("button", "set-rm", t("menu.copyPath"));
  copy.type = "button";
  copy.addEventListener("click", () => void copyText(set.configured).catch(showError));
  acts.append(copy);
  // A remote Host's folder is on that Host: no file manager here opens it.
  if (set.folder_status === "ready" && !isRemote()) {
    const show = h("button", "set-rm", t("menu.showInFileManager"));
    show.type = "button";
    show.addEventListener("click", () => reveal(set.configured));
    acts.append(show);
  }
  const edit = h("button", "set-rm", t("skipper.change"));
  edit.type = "button";
  edit.addEventListener("click", change);
  acts.append(edit);
  row.append(h("span", "c-name", t("skipper.folder")), path, acts);
  const why =
    set.folder_status === "other"
      ? t("skipper.folderOther")
      : set.folder_status === "refused" || set.folder_status === "missing"
        ? (set.folder_error ?? t("skipper.refused"))
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
  const label = (a: AgentInfo) => (a.custom ? t("skipper.custom", { id: a.id }) : a.id);
  const head = h("div", "set-head");
  const title = h("div", "set-h", t("settings.tab.taskTypes"));
  title.id = "set-h-tasks";
  head.append(title, h("div", "set-sub mono", set.types));
  const row = (task: TaskType, change: Partial<TaskEdit>): TaskEdit => ({ type: task.type, agent: task.agent, model: task.model, ...change });
  const table = rowTable<TaskType>(
    {
      grid: "g-tasks",
      words: tableWords("taskType"),
      labelledBy: title.id,
      rows: set.tasks ?? [],
      id: (task) => task.type,
      cols: [
        {
          key: "type",
          head: upper(t("tasks.colType")),
          name: t("tasks.type"),
          kind: "key",
          required: true,
          get: (task) => task.type,
          check: (v) => (TASK_TYPE_NAME.test(v) ? null : badTaskTypeName()),
        },
        {
          key: "agent",
          head: upper(t("settings.agents.colAgent")),
          name: t("tasks.agent"),
          kind: "select",
          empty: t("skipper.noAgentOnShort"),
          options: (task) => {
            const list: [string, string][] = choices.map((a) => [a.id, label(a)]);
            // A row keeps an agent that is off or gone, so the select shows the truth.
            if (task && !choices.some((a) => a.id === task.agent)) list.unshift([task.agent, t(byId(task.agent) ? "tasks.agentOff" : "tasks.agentGone", { id: task.agent })]);
            return list;
          },
          initial: () => skiffAgentChoice(agents, set.agent)?.id ?? choices[0]?.id ?? "",
          get: (task) => task.agent,
        },
        {
          key: "model",
          head: upper(t("tasks.colModel")),
          name: t("tasks.model"),
          kind: "text",
          mono: true,
          placeholder: () => t("skipper.agentDefault"),
          get: (task) => task.model ?? "",
          // A custom agent sets its model itself. A gone agent has none.
          note: (task, d) => {
            const a = byId(task ? task.agent : (d.agent ?? ""));
            return modelFlag(a) ? null : a ? t("tasks.setByAgent") : "";
          },
          check: modelError,
        },
        {
          key: "instructions",
          head: upper(t("tasks.colInstructions")),
          name: t("tasks.instructions"),
          kind: "lead",
          draw: (task) => {
            const cell = h("span", "c-file");
            const none = task.instructions == null;
            const state = h("span", "c-state" + (none ? " none" : ""));
            state.append(h("span", "sq"), none ? t("tasks.none") : fileSize(task.instructions!));
            const edit = h("button", "set-rm", t("skipper.edit"));
            edit.type = "button";
            edit.setAttribute("aria-label", t("tasks.editInstructions", { type: task.type }));
            edit.addEventListener("click", () => files.edit(task));
            cell.append(state, edit);
            return cell;
          },
          drawDraft: () => {
            const state = h("span", "c-state none");
            state.append(h("span", "sq"), t("tasks.none"));
            return state;
          },
        },
      ],
      blocked: () => (choices.length ? null : t("tasks.blocked")),
      add: (d) => save({ key: "add_task", value: { type: d.type, agent: d.agent, model: d.model || null } }),
      edit: (task, key, v) => {
        if (key === "type") return save({ key: "rename_task", value: { from: task.type, to: v } });
        // A new agent with no model flag drops the model: skiff-core saves none.
        return save({ key: "task", value: row(task, key === "agent" ? { agent: v } : { model: v || null }) });
      },
      remove: (task) => save({ key: "remove_task", value: task.type }),
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
function textRow(label: string, aria: string, value: string, placeholder: string, save: (v: string) => void): HTMLElement {
  const row = h("div", "set-row set-open set-agent on");
  const input = h("input", "c-cmd");
  input.value = value;
  input.placeholder = placeholder;
  input.spellcheck = false;
  input.setAttribute("aria-label", aria);
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
