import { h } from "../ui/dom";
import { rune } from "../ui/runes";
import { t } from "../i18n";

/**
 * A Settings table with a draft row last: Settings › Agents and Settings ›
 * Task types. One implementation for both, so they add, edit,
 * remove and report errors the same way. A table with no `add` has no
 * draft row, and one with no `remove` has no −, as Settings › Skippers ›
 * Instructions.
 *
 * - The draft row is always last, on the panel color, with "New …" in its
 *   name field. Enter in it, or its "Add agent" button, adds. Blur never
 *   adds. The line under it says what the draft needs: nothing while it is
 *   empty, "Not added yet. …" once it has text, "Adding x…" while it saves
 *   and "Added x." after. The button fills when the draft can add, and a
 *   second add while one saves does nothing. An empty required cell gets the
 *   focus. A bad value shows its error on the line, marks the cell and keeps
 *   the text. A good one adds the row, clears the draft and puts focus back
 *   in its first field.
 * - In a row, a text cell saves on Enter or blur, Esc puts the old value
 *   back, and a select saves on change. A rename to an empty or taken name
 *   reverts, and the error line says why.
 * - − removes at once. Focus goes to the − now at the same place, else the
 *   last one, else the draft.
 * - The draft, the error and the save in flight live in the caller's
 *   {@link RowTableState}, so a redraw for any other reason, a tab change or
 *   closing Settings keeps what the person typed and what the line says.
 *
 * Every control is a native input, select or button, so the Settings arrow
 * keys reach each one. The table adds no key.
 */

export type Draft = Record<string, string>;

/** One column. `key` names the field in the draft and in edits. */
export interface RowCol<R> {
  key: string;
  /** Column header text. Empty for the lead column. */
  head: string;
  /** The word in aria-labels: "name", "command", "model". */
  name: string;
  /** The field in "Type …, then press Enter": "a command". Absent: the column's name. */
  field?: string;
  /** lead: a cell the caller draws (the switch). key: the name, required, renames. preview: read-only text. */
  kind: "lead" | "key" | "text" | "select" | "preview";
  mono?: boolean;
  /** The draft cannot add while this cell is empty. */
  required?: boolean;
  placeholder?: (row: R | null) => string;
  /** select: [value, label] pairs for a row, or for the draft (null). */
  options?: (row: R | null) => [string, string][];
  /** select: the one option shown, off, when there are none. */
  empty?: string;
  /** The draft's starting value. Absent: empty. */
  initial?: () => string;
  /** Text in place of the field, such as "in the command". Null: the field shows. */
  note?: (row: R | null, draft: Draft) => string | null;
  get?: (row: R) => string;
  /** A preview cell that shows an error, not a value. */
  bad?: (row: R) => boolean;
  /** lead: the caller's control for the row. The draft's lead cell is blank. */
  draw?: (row: R) => HTMLElement;
  /** lead: what the draft's cell shows in place of the dim +. */
  drawDraft?: () => HTMLElement;
  /** Why a value cannot be saved, checked before the daemon. Null: good. */
  check?: (value: string) => string | null;
}

/**
 * A table's own words. Each is a whole phrase from the catalog, so a language
 * puts the noun, its article and its gender where it needs them.
 */
export interface TableWords {
  /** "Add agent": the draft's button. */
  add: string;
  /** "New agent": the draft row, and the start of its field labels. */
  newRow: string;
  /** "New agent name": the draft's name placeholder. */
  newName: string;
  /** "The new agent": the draft before it has a name. */
  theNew: string;
  /** "An agent needs a name." */
  needsName: string;
  /** "An agent named x exists." */
  exists: (name: string) => string;
  /** "an agent name": the name field in "Type …". */
  nameField: string;
  /** After `exists` on a duplicate add: "Change its command in its row." */
  dupHint: string;
}

export type TableKind = "agent" | "file" | "taskType";

/** The words of a table from the catalog: `table.<kind>.*`. */
export const tableWords = (k: TableKind): TableWords => ({
  add: t(`table.${k}.add`),
  newRow: t(`table.${k}.newRow`),
  newName: t(`table.${k}.newName`),
  theNew: t(`table.${k}.theNew`),
  needsName: t(`table.${k}.needsName`),
  exists: (name) => t(`table.${k}.exists`, { name }),
  nameField: t(`table.${k}.nameField`),
  dupHint: t(`table.${k}.dupHint`),
});

export interface RowTableSpec<R> {
  /** Grid class: "g-agents", "g-tasks". Also names the table in the DOM. */
  grid: string;
  /** What the table holds, in whole phrases: see `tableWords()`. */
  words: TableWords;
  /** The id of the section's heading. */
  labelledBy: string;
  /** Header of the end column. Empty: Remove, for screen readers only. */
  endHead?: string;
  cols: RowCol<R>[];
  rows: R[];
  id: (row: R) => string;
  rowClass?: (row: R) => string;
  /** What shows in the end cell before the −: the agent's status, or the row's own buttons. */
  status?: (row: R) => Node;
  /** Why a row's − is off, short, after the status in the end cell: "Admiral +3". Null: it can go. */
  kept?: (row: R) => string | null;
  /** The draft's preview column, as the person types. */
  preview?: (draft: Draft) => string;
  /** Why the draft cannot add at all, such as no agent being on. */
  blocked?: () => string | null;
  /** Why a row's new value cannot be saved, past the column's own check. */
  checkEdit?: (row: R, key: string, value: string) => string | null;
  /** Each saves and answers with the error to show, or null. They do not redraw. */
  add?: (draft: Draft) => Promise<string | null>;
  edit?: (row: R, key: string, value: string) => Promise<string | null>;
  remove?: (row: R) => Promise<string | null>;
}

/** What survives a redraw. The caller keeps one per table. */
export interface RowTableState {
  draft: Draft;
  error: string | null;
  /** The draft column the error is about. */
  bad: string | null;
  /** The name of the draft being saved. While set, another add does nothing. */
  pending: string | null;
  /** The name of the last draft added. The line says so until the next edit. */
  done: string | null;
}

/**
 * A section head inside a Settings page: one 40px tinted band, the title
 * left as a column label, a count right. The 80px head is for the page title.
 */
export function sectionHead(title: string, id?: string, count?: string): HTMLElement {
  const head = h("div", "set-band");
  const band = h("span", "set-band-title", title);
  if (id) band.id = id;
  head.append(band);
  if (count !== undefined) head.append(h("span", "set-band-count", count));
  return head;
}

export const tableState = (): RowTableState => ({ draft: {}, error: null, bad: null, pending: null, done: null });

/** What the line under the draft says. "hidden": nothing. "error" is an alert, the others are polite. */
export type LineKind = "hidden" | "incomplete" | "check" | "ready" | "adding" | "added" | "blocked" | "error";

const FIELDS = new Set(["key", "text", "select"]);

export function rowTable<R>(spec: RowTableSpec<R>, state: RowTableState, redraw: () => void): HTMLElement {
  const lineId = `${spec.grid}-line`;
  const table = h("div", `set-table ${spec.grid}`);
  table.dataset.rowTable = spec.grid;
  table.setAttribute("role", "table");
  table.setAttribute("aria-labelledby", spec.labelledBy);

  const value = (c: RowCol<R>) => draftValue(c, state);
  const current = (): Draft => Object.fromEntries(spec.cols.map((c) => [c.key, value(c)]));
  const draftLabel = (c: RowCol<R>) => t("table.draftField", { row: spec.words.newRow, field: c.name });
  const firstField = spec.cols.find((c) => FIELDS.has(c.kind));

  /** Focuses a control of this table by its label, after a redraw. */
  function focus(label: string) {
    const el = document.querySelector<HTMLElement>(`[data-row-table="${spec.grid}"] [aria-label="${CSS.escape(label)}"]`);
    el?.focus();
    if (el instanceof HTMLInputElement) el.setSelectionRange(el.value.length, el.value.length);
  }
  const focusDraft = () => firstField && focus(draftLabel(firstField));

  /** Shows the outcome of a save. True when it worked. */
  function settle(error: string | null, bad: string | null = null, done: string | null = null): boolean {
    state.error = error;
    state.bad = error ? bad : null;
    state.done = done;
    redraw();
    return !error;
  }

  // Header.
  const head = h("div", "set-row set-cols");
  head.setAttribute("role", "row");
  for (const c of spec.cols) {
    const cell = h("span", cellClass(c), c.head);
    cell.setAttribute("role", "columnheader");
    if (!c.head) cell.setAttribute("aria-label", c.name);
    head.appendChild(cell);
  }
  const endHead = h("span", "c-status c-end", spec.endHead ?? "");
  endHead.setAttribute("role", "columnheader");
  if (!spec.endHead && spec.remove) endHead.setAttribute("aria-label", t("table.remove"));
  head.appendChild(endHead);
  table.appendChild(head);

  // Rows.
  spec.rows.forEach((r, at) => {
    const id = spec.id(r);
    const row = h("div", ("set-row " + (spec.rowClass?.(r) ?? "")).trim());
    row.setAttribute("role", "row");
    row.dataset.id = id;
    for (const c of spec.cols) {
      const cell = h("span", cellClass(c));
      cell.setAttribute("role", "cell");
      const label = t("table.cell", { id, field: c.name });
      const note = c.note?.(r, {});
      if (c.kind === "lead") cell.appendChild(c.draw!(r));
      else if (c.kind === "preview") fillPreview(cell, c.get?.(r) ?? "", !!c.bad?.(r));
      else if (note !== null && note !== undefined) fillNote(cell, note, !!c.mono);
      else if (c.kind === "select") {
        const old = c.get?.(r) ?? "";
        const pick = select(c, c.options?.(r) ?? [], old, label);
        pick.addEventListener("change", () => void commit(r, c, pick.value, old, label));
        cell.appendChild(pick);
      } else {
        const old = c.get?.(r) ?? "";
        const input = field(c, old, c.placeholder?.(r) ?? "", label);
        input.addEventListener("keydown", (e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            input.blur();
          } else if (e.key === "Escape") {
            // As every Settings field: the old value comes back, and Esc goes on to close Settings.
            input.value = old;
            input.blur();
          }
        });
        input.addEventListener("blur", () => void commit(r, c, input.value.trim(), old, label));
        cell.appendChild(input);
      }
      row.appendChild(cell);
    }
    const end = h("span", "c-status c-end c-acts");
    end.setAttribute("role", "cell");
    if (spec.status) end.appendChild(spec.status(r));
    const kept = spec.kept?.(r) ?? null;
    if (kept !== null) {
      const why = h("span", "c-kept", `· ${kept}`);
      why.title = kept;
      end.appendChild(why);
    }
    if (spec.remove) {
      const rm = iconButton("indicators-minus", t("table.remove"), t("table.removeRow", { id }));
      rm.classList.add("c-remove");
      rm.disabled = kept !== null;
      if (kept !== null) rm.title = t("table.usedBy", { who: kept });
      rm.addEventListener("click", () => void remove(r, at));
      end.appendChild(rm);
    }
    row.appendChild(end);
    table.appendChild(row);
  });
  if (!spec.add) return table;

  // The draft row. It looks unlike the rows above: the panel color, "New …" and a labeled button.
  const draft = h("div", "set-row set-draft");
  draft.setAttribute("role", "row");
  draft.setAttribute("aria-label", spec.words.newRow);
  let preview: HTMLElement | null = null;
  const values = current();
  const inputs: HTMLInputElement[] = [];
  for (const c of spec.cols) {
    const cell = h("span", cellClass(c));
    cell.setAttribute("role", "cell");
    const label = draftLabel(c);
    const note = c.note?.(null, values);
    if (c.kind === "lead" && c.drawDraft) {
      cell.appendChild(c.drawDraft());
    } else if (c.kind === "lead") {
      // No switch: a dim + says the row is new.
      const plus = rune("indicators-plus");
      plus.classList.add("set-draft-mark");
      plus.setAttribute("aria-hidden", "true");
      cell.appendChild(plus);
    } else if (c.kind === "preview") {
      preview = fillPreview(cell, spec.preview?.(values) ?? "", false);
    } else if (note !== null && note !== undefined) {
      fillNote(cell, note, !!c.mono);
    } else {
      const el =
        c.kind === "select"
          ? select(c, c.options?.(null) ?? [], value(c), label)
          : field(c, value(c), c.placeholder?.(null) ?? (c.kind === "key" ? spec.words.newName : ""), label);
      el.setAttribute("aria-describedby", lineId);
      if (state.bad === c.key && state.error) el.setAttribute("aria-invalid", "true");
      if (el instanceof HTMLSelectElement) {
        el.addEventListener("change", () => {
          if (state.pending !== null) {
            el.value = value(c);
            return;
          }
          state.draft[c.key] = el.value;
          state.error = state.bad = state.done = null;
          // The agent decides whether the model is a field.
          redraw();
          focus(label);
        });
      } else {
        inputs.push(el);
        el.addEventListener("input", () => {
          state.draft[c.key] = el.value;
          // An edit ends what the line said about the last add or error. No redraw: the keys stay in the field.
          state.error = state.bad = state.done = null;
          if (preview) preview.textContent = preview.title = spec.preview?.(current()) ?? "";
          paint();
        });
      }
      el.addEventListener("keydown", (e) => {
        const k = (e as KeyboardEvent).key;
        if (k === "Enter") {
          e.preventDefault();
          void submit();
        } else if (k === "Escape" && dirty() && state.pending === null) {
          // Clears the draft. Settings stays open: Esc in an empty draft closes it.
          e.stopPropagation();
          e.preventDefault();
          state.draft = {};
          settle(null);
          focusDraft();
        }
      });
      cell.appendChild(el);
    }
    draft.appendChild(cell);
  }
  const end = h("span", "c-status c-end c-acts");
  end.setAttribute("role", "cell");
  // Never disabled: an add that cannot go says why on the line. While a save runs it is aria-disabled and does nothing.
  const addText = spec.words.add;
  const add = h("button", "set-add");
  add.type = "button";
  add.setAttribute("aria-label", addText);
  add.setAttribute("aria-describedby", lineId);
  const addWord = h("span", "", addText);
  add.append(rune("indicators-plus"), addWord);
  add.addEventListener("click", () => void submit());
  end.appendChild(add);
  draft.appendChild(end);
  table.appendChild(draft);

  // One line under the draft: the hint, the save in flight, the outcome or the error.
  // It stays in the DOM when empty, so a screen reader hears each change.
  const line = h("div", "set-hint");
  line.id = lineId;
  table.appendChild(line);
  paint();
  return table;

  /** Puts the line, the button and the fields in step with the state, in place. */
  function paint() {
    const { kind, text } = draftLine(spec, state);
    line.dataset.kind = kind;
    line.classList.toggle("set-error", kind === "error");
    line.setAttribute("role", kind === "error" ? "alert" : "status");
    line.textContent = text;
    const adding = kind === "adding";
    add.classList.toggle("ready", kind === "ready");
    if (adding) add.setAttribute("aria-disabled", "true");
    else add.removeAttribute("aria-disabled");
    addWord.textContent = adding ? t("table.adding") : addText;
    // While it saves, the draft keeps what was sent.
    for (const i of inputs) i.readOnly = adding;
    if (!state.error) for (const el of draft.querySelectorAll("[aria-invalid]")) el.removeAttribute("aria-invalid");
  }

  function dirty(): boolean {
    return draftDirty(spec, state);
  }

  async function submit() {
    if (state.pending !== null) return;
    const blocked = spec.blocked?.();
    if (blocked) {
      settle(blocked, firstField?.key ?? null);
      return focusDraft();
    }
    const { d, shown } = draftValues(spec, state);
    // An empty required cell: the keys go there, and nothing is wrong yet.
    const empty = missing(spec, d, shown);
    if (empty) return focus(draftLabel(empty));
    const bad = invalid(spec, d, shown);
    if (bad) {
      settle(bad.why, bad.col.key);
      return focus(draftLabel(bad.col));
    }
    const name = d[spec.cols.find((c) => c.kind === "key")?.key ?? ""];
    state.pending = name;
    state.error = state.bad = state.done = null;
    paint();
    let error: string | null;
    try {
      error = await spec.add!(d);
    } catch (e) {
      error = String(e);
    } finally {
      state.pending = null;
    }
    if (!error) state.draft = {};
    settle(error, null, error ? null : name);
    focusDraft();
  }

  async function commit(r: R, c: RowCol<R>, v: string, old: string, label: string) {
    if (v === old.trim()) return;
    const id = spec.id(r);
    if (c.kind === "key") {
      const why = !v
        ? spec.words.needsName
        : (c.check?.(v) ?? (spec.rows.some((x) => spec.id(x) === v) ? spec.words.exists(v) : null));
      if (why) return void settle(t("table.keepsName", { why, id }));
    } else {
      const why = c.check?.(v) ?? spec.checkEdit?.(r, c.key, v);
      if (why) return void settle(why);
    }
    settle((await spec.edit?.(r, c.key, v)) ?? null);
    // Settings puts back a focused input itself. A select, or a renamed row, needs it here.
    if (c.kind === "select") focus(label);
  }

  async function remove(r: R, at: number) {
    settle(await spec.remove!(r));
    const table = document.querySelector(`[data-row-table="${spec.grid}"]`);
    const rms = table ? [...table.querySelectorAll<HTMLButtonElement>(".c-remove:not(:disabled)")] : [];
    const next = rms[Math.min(at, rms.length - 1)];
    if (next) next.focus();
    else focusDraft();
  }
}

/** The draft as it would be sent: trimmed, and empty where a cell shows a note. */
export function draftValues<R>(spec: RowTableSpec<R>, state: RowTableState): { d: Draft; shown: (c: RowCol<R>) => boolean } {
  const d: Draft = {};
  for (const c of spec.cols) d[c.key] = draftValue(c, state).trim();
  const shown = (c: RowCol<R>) => c.note?.(null, d) == null;
  // A cell that shows a note has no field: what was typed there before does not count.
  for (const c of spec.cols) if (!shown(c)) d[c.key] = "";
  return { d, shown };
}

const initialOf = <R>(c: RowCol<R>) => c.initial?.() ?? "";

/** A draft cell's value as it shows. */
function draftValue<R>(c: RowCol<R>, state: RowTableState): string {
  const v = state.draft[c.key] ?? initialOf(c);
  // A draft select keeps a value only while it is an option.
  if (c.kind === "select" && !(c.options?.(null) ?? []).some(([o]) => o === v)) return initialOf(c);
  return v;
}

/** True when a draft field differs from its starting value. */
export function draftDirty<R>(spec: RowTableSpec<R>, state: RowTableState): boolean {
  return spec.cols.some((c) => FIELDS.has(c.kind) && draftValue(c, state) !== initialOf(c));
}

/** The first required cell that is empty, if one is. */
function missing<R>(spec: RowTableSpec<R>, d: Draft, shown: (c: RowCol<R>) => boolean): RowCol<R> | undefined {
  return spec.cols.find((c) => c.required && shown(c) && !d[c.key]);
}

/** The first cell with a value that cannot be added, and why. */
function invalid<R>(spec: RowTableSpec<R>, d: Draft, shown: (c: RowCol<R>) => boolean): { col: RowCol<R>; why: string } | null {
  for (const c of spec.cols.filter((c) => FIELDS.has(c.kind) && shown(c) && d[c.key])) {
    const why =
      c.check?.(d[c.key]) ??
      (c.kind === "key" && spec.rows.some((r) => spec.id(r) === d[c.key]) ? `${spec.words.exists(d[c.key])} ${spec.words.dupHint}`.trim() : null);
    if (why) return { col: c, why };
  }
  return null;
}

/** "a command", "an agent name": the field named in "Type …". */
function fieldWords<R>(spec: RowTableSpec<R>, c: RowCol<R>): string {
  return c.kind === "key" ? spec.words.nameField : (c.field ?? c.name);
}

/** The kind and the text of the line under the draft. */
export function draftLine<R>(spec: RowTableSpec<R>, state: RowTableState): { kind: LineKind; text: string } {
  const add = spec.words.add;
  if (state.pending !== null) return { kind: "adding", text: t("table.addingName", { name: state.pending }) };
  if (state.error) return { kind: "error", text: state.error };
  const blocked = spec.blocked?.();
  if (blocked) return { kind: "blocked", text: blocked };
  if (!draftDirty(spec, state)) return state.done !== null ? { kind: "added", text: t("table.added", { name: state.done }) } : { kind: "hidden", text: "" };
  const { d, shown } = draftValues(spec, state);
  const empty = missing(spec, d, shown);
  if (empty) return { kind: "incomplete", text: t("table.typeField", { field: fieldWords(spec, empty) }) };
  const name = d[spec.cols.find((c) => c.kind === "key")?.key ?? ""] || spec.words.theNew;
  if (invalid(spec, d, shown)) return { kind: "check", text: t("table.check", { name, add }) };
  return { kind: "ready", text: t("table.ready", { name, add }) };
}

function cellClass<R>(c: RowCol<R>): string {
  if (c.kind === "lead") return "c-lead";
  if (c.kind === "preview") return "c-preview";
  return `c-field c-${c.key}`;
}

function field<R>(c: RowCol<R>, value: string, placeholder: string, label: string): HTMLInputElement {
  const input = h("input", (c.kind === "key" ? "c-name" : "c-cmd") + (c.mono ? " mono" : ""));
  input.value = value;
  input.placeholder = placeholder;
  input.spellcheck = false;
  input.setAttribute("aria-label", label);
  return input;
}

function select<R>(c: RowCol<R>, options: [string, string][], value: string, label: string): HTMLSelectElement {
  const pick = h("select", "c-cmd");
  pick.setAttribute("aria-label", label);
  if (!options.length) pick.append(h("option", "", c.empty ?? ""));
  for (const [v, text] of options) {
    const o = h("option", "", text);
    o.value = v;
    pick.append(o);
  }
  if (options.length) pick.value = value;
  pick.disabled = !options.length;
  return pick;
}

function fillPreview(cell: HTMLElement, text: string, bad: boolean): HTMLElement {
  const span = h("span", "mono" + (bad ? " bad" : ""), text);
  span.title = text;
  cell.appendChild(span);
  return span;
}

function fillNote(cell: HTMLElement, text: string, mono: boolean) {
  cell.classList.add("c-note");
  if (mono) cell.classList.add("mono");
  cell.textContent = text;
}

function iconButton(icon: Parameters<typeof rune>[0], title: string, label: string): HTMLButtonElement {
  const b = h("button", "set-rm set-ico");
  b.type = "button";
  b.title = title;
  b.setAttribute("aria-label", label);
  b.appendChild(rune(icon));
  return b;
}
