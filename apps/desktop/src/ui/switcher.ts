import { fuzzy } from "../workspace/model";
import type { SessionState } from "../platform/types";
import { t } from "../i18n";

export interface SwitchItem {
  kind: "command" | "session" | "worktree";
  /** Project accent. */
  color: string;
  primary: string;
  secondary: string;
  state?: SessionState;
  meta: string;
  /** What the query matches against. */
  text: string;
  run: () => void;
}

const MAX_ROWS = 50;

/** The command palette: commands, sessions and worktrees in one list. Arrows move, Enter runs, Esc closes. */
export function createSwitcher(items: () => SwitchItem[], onClose: () => void) {
  const overlay = document.createElement("div");
  overlay.id = "switcher";
  overlay.hidden = true;
  overlay.innerHTML = `
    <div class="sw-panel" role="dialog">
      <div class="sw-input">
        <label for="sw-q" class="visually-hidden"></label>
        <input id="sw-q" type="text" autocomplete="off" spellcheck="false"
          role="combobox" aria-expanded="true" aria-controls="sw-list" aria-autocomplete="list" />
        <kbd>Esc</kbd>
      </div>
      <div id="sw-list" role="listbox"></div>
      <div class="sw-foot"><span><kbd>↑</kbd> <kbd>↓</kbd> <span data-w="move"></span></span><span><kbd>Enter</kbd> <span data-w="focus"></span></span><span><kbd>Esc</kbd> <span data-w="close"></span></span></div>
    </div>`;
  // Words as text, never as markup: a catalog line cannot add HTML.
  overlay.querySelector(".sw-panel")!.setAttribute("aria-label", t("switcher.title"));
  overlay.querySelector("label")!.textContent = t("switcher.search");
  document.body.appendChild(overlay);

  const input = overlay.querySelector("input") as HTMLInputElement;
  const list = overlay.querySelector("#sw-list") as HTMLDivElement;
  input.placeholder = t("switcher.search");
  list.setAttribute("aria-label", t("switcher.matches"));
  for (const w of ["move", "focus", "close"] as const) overlay.querySelector(`[data-w=${w}]`)!.textContent = t(`switcher.${w}`);
  let shown: SwitchItem[] = [];
  let active = 0;

  function filter() {
    const q = input.value.trim();
    const all = items();
    if (!q) {
      shown = all.slice(0, MAX_ROWS);
    } else {
      shown = all
        .map((item, i) => ({ item, i, score: fuzzy(q, item.text) }))
        .filter((x) => x.score >= 0)
        .sort((a, b) => b.score - a.score || a.i - b.i)
        .slice(0, MAX_ROWS)
        .map((x) => x.item);
    }
    active = 0;
    render();
  }

  function render() {
    list.replaceChildren();
    if (shown.length === 0) {
      const empty = document.createElement("div");
      empty.className = "sw-empty";
      empty.textContent = t("switcher.noMatch");
      list.appendChild(empty);
      input.removeAttribute("aria-activedescendant");
      return;
    }
    shown.forEach((item, i) => {
      const row = document.createElement("div");
      row.id = `sw-opt-${i}`;
      row.className = "sw-row" + (i === active ? " active" : "");
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(i === active));
      row.style.setProperty("--pc", item.color);
      row.innerHTML = `<span class="sq"></span><span class="kind"></span><span class="primary"></span><span class="secondary"></span><span class="meta"></span>`;
      const kind = row.querySelector(".kind") as HTMLElement;
      if (item.state) {
        kind.innerHTML = `<span class="dot ${item.state}"></span>`;
        kind.append(t(`state.${item.state}`));
        kind.classList.add("state", item.state);
      } else {
        kind.textContent = t(`switcher.kind.${item.kind}`);
      }
      (row.querySelector(".primary") as HTMLElement).textContent = item.primary;
      (row.querySelector(".secondary") as HTMLElement).textContent = item.secondary;
      (row.querySelector(".meta") as HTMLElement).textContent = item.meta;
      row.addEventListener("mousedown", (e) => e.preventDefault());
      row.addEventListener("click", () => choose(i));
      list.appendChild(row);
    });
    input.setAttribute("aria-activedescendant", `sw-opt-${active}`);
    list.children[active]?.scrollIntoView({ block: "nearest" });
  }

  function move(delta: number) {
    if (shown.length === 0) return;
    active = (active + delta + shown.length) % shown.length;
    render();
  }

  function choose(i: number) {
    const item = shown[i];
    close();
    item?.run();
  }

  function open() {
    overlay.hidden = false;
    input.value = "";
    filter();
    input.focus();
  }

  function close() {
    if (overlay.hidden) return;
    overlay.hidden = true;
    onClose();
  }

  input.addEventListener("input", filter);
  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") move(1);
    else if (e.key === "ArrowUp") move(-1);
    else if (e.key === "Enter") choose(active);
    else if (e.key === "Escape") close();
    else return;
    e.preventDefault();
    e.stopPropagation();
  });
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });

  return {
    open,
    close,
    toggle: () => (overlay.hidden ? open() : close()),
    isOpen: () => !overlay.hidden,
  };
}
