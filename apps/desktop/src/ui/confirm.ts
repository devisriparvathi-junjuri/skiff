import { h } from "./dom";
import { actionDialog } from "./dialogParts";
import { t } from "../i18n";

/**
 * One modal for actions that cannot be undone. Resolves true on the action,
 * false on Cancel, Esc, or a click outside. Enter runs the action.
 */
export function confirmAction(o: { title: string; body: string; action: string }): Promise<boolean> {
  return new Promise((resolve) => {
    let ok = false;
    const back = document.activeElement as HTMLElement | null;
    actionDialog({
      ...o,
      tone: "danger",
      submit: () => void (ok = true),
      onClose: () => {
        back?.focus?.();
        resolve(ok);
      },
    });
  });
}

/**
 * A modal with one text field. `submit` runs on Enter or the action button;
 * a thrown error shows in the dialog and keeps it open. Esc or Cancel closes.
 */
export function promptAction(o: {
  title: string;
  body: string;
  placeholder: string;
  /** The field starts with this text. */
  value?: string;
  /** An empty field is allowed and goes to `submit` as "". */
  allowEmpty?: boolean;
  maxLength?: number;
  /** Names to pick from. The field completes from them as you type, and a list under it shows the matches. */
  options?: string[];
  /** The label of a last list row that submits an empty field. */
  clearLabel?: string;
  /** A section head over the list's names. */
  optionsHead?: string;
  /** More fields, under the list. */
  after?: HTMLElement[];
  /** The field's text after each change, trimmed. */
  onInput?: (value: string) => void;
  action: string;
  submit: (value: string) => Promise<void>;
  onClose?: () => void;
}): void {
  const input = h("input", "prompt-input");
  input.type = "text";
  input.spellcheck = false;
  input.placeholder = o.placeholder;
  if (o.value) input.value = o.value;
  if (o.maxLength) input.maxLength = o.maxLength;
  input.setAttribute("aria-label", o.placeholder);
  const list = o.options || o.clearLabel ? pickList(input, o.options ?? [], o.clearLabel, o.optionsHead, () => dialog.run()) : null;
  // After the list's own handlers, so arrow keys report the name they put in the field.
  if (o.onInput) for (const ev of ["input", "keydown"]) input.addEventListener(ev, () => o.onInput!(input.value.trim()));
  const dialog = actionDialog({
    title: o.title,
    body: o.body,
    content: [input, ...(list ? [list] : []), ...(o.after ?? [])],
    action: o.action,
    tone: "go",
    focus: input,
    onClose: o.onClose,
    submit: async () => {
      // A name typed in other letter case is the listed name.
      const typed = input.value.trim();
      const v = o.options?.find((n) => n.toLowerCase() === typed.toLowerCase()) ?? typed;
      if (!v && !o.allowEmpty) {
        input.focus();
        throw t("dialog.typeField", { field: o.placeholder });
      }
      await o.submit(v);
    },
  });
}

let lists = 0;

/**
 * The list under a prompt's field. Typing completes the field from the first
 * name that starts with the text and shows every name that holds it. The arrows
 * put the lit row's name in the field, so Enter takes the lit row or the typed text.
 * The names scroll; the clear row stays pinned under them, always in view.
 */
function pickList(input: HTMLInputElement, options: string[], clearLabel: string | undefined, head: string | undefined, run: () => void) {
  const list = h("div", "prompt-list");
  const headRow = head ? h("div", "prompt-head", head) : null;
  const scroll = h("div", "prompt-scroll");
  scroll.setAttribute("role", "presentation");
  list.id = `prompt-list-${++lists}`;
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", t("switcher.matches"));
  input.autocomplete = "off";
  input.setAttribute("role", "combobox");
  input.setAttribute("aria-autocomplete", "both");
  input.setAttribute("aria-controls", list.id);
  input.setAttribute("aria-expanded", "true");

  let rows: { label: string; value: string; clear?: boolean }[] = [];
  let active = -1;

  const render = () => {
    const els = rows.map((r, i) => {
      const row = h("div", "lm-item prompt-opt" + (r.clear ? " clear" : "") + (i === active ? " active" : ""), r.label);
      row.id = `${list.id}-${i}`;
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(i === active));
      row.addEventListener("mousedown", (e) => e.preventDefault());
      row.addEventListener("click", () => {
        input.value = r.value;
        run();
      });
      return row;
    });
    scroll.replaceChildren(...els.filter((_, i) => !rows[i].clear));
    const named = rows.some((r) => !r.clear);
    list.replaceChildren(...(headRow && named ? [headRow] : []), scroll, ...els.filter((_, i) => rows[i].clear));
    list.hidden = !rows.length;
    if (active >= 0) input.setAttribute("aria-activedescendant", `${list.id}-${active}`);
    else input.removeAttribute("aria-activedescendant");
    if (active >= 0 && !rows[active].clear) els[active].scrollIntoView({ block: "nearest" });
  };

  const fill = (names: string[]) => (rows = [...names.map((n) => ({ label: n, value: n })), ...(clearLabel ? [{ label: clearLabel, value: "", clear: true }] : [])]);

  // Opened: every name, the current one lit and selected, so typing replaces it.
  fill(options);
  active = options.findIndex((n) => n === input.value);
  queueMicrotask(() => input.select());
  render();

  input.addEventListener("input", (e) => {
    const typed = input.value;
    const q = typed.trim().toLowerCase();
    const starts = options.filter((n) => n.toLowerCase().startsWith(q));
    fill([...starts, ...options.filter((n) => !starts.includes(n) && n.toLowerCase().includes(q))]);
    active = rows.findIndex((r) => !r.clear && r.value.toLowerCase() === q);
    // Complete only while the user adds text: Backspace must be able to undo a completion.
    if (q && starts.length && (e as InputEvent).inputType?.startsWith("insert")) {
      input.value = starts[0];
      input.setSelectionRange(typed.length, starts[0].length);
      active = 0;
    }
    render();
  });
  input.addEventListener("keydown", (e) => {
    if ((e.key !== "ArrowDown" && e.key !== "ArrowUp") || !rows.length) return;
    e.preventDefault();
    active = (active + (e.key === "ArrowDown" ? 1 : active < 0 ? 0 : -1) + rows.length) % rows.length;
    input.value = rows[active].value;
    input.select();
    render();
  });
  return list;
}
