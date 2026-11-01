import { menuRow } from "./menuRow";
import type { AgentInfo, Project, Worktree } from "../platform/types";
import { launchIcon } from "../appearance/agentIcon";
import { followPointer, menuOpened, type MenuEntry } from "./menu";
import { launchDigit } from "../app/stateQueries";
import { branchName } from "../workspace/model";
import { t, upper } from "../i18n";
import { rune } from "./runes";

/** The Skipper a start menu offers first. */
export interface SkipperStart {
  label: string;
  run: () => void;
  disabled?: boolean;
}

interface Opts {
  /** Shell (null), then enabled agents. */
  choices: () => (AgentInfo | null)[];
  /** The project's Skipper, or the fleet's while it runs. Null with no Skipper folder. */
  skipper: (p: Project) => SkipperStart | null;
  /** `agent` null starts a plain shell. */
  start: (p: Project, w: Worktree, agent: AgentInfo | null) => void;
  openSettings: () => void;
  /** Layouts for a blank split, with a small picture of each. */
  splits: () => { name: string; glyph: SVGElement }[];
  split: (p: Project, w: Worktree, i: number) => void;
  onClose: () => void;
}

/** The menu's title: the branch, "detached" with no branch, or a plain folder's name. */
export function startIn(w: Worktree): string {
  return upper(t("launch.startIn", { name: w.branch ?? (w.head ? t("launch.detached") : branchName(w)) }));
}

function skipperIcon() {
  const span = document.createElement("span");
  span.className = "agent-icon";
  span.setAttribute("aria-hidden", "true");
  span.append(rune("tools-sparkles", 14));
  return span;
}

/** The + menu on a worktree: the Skipper, Shell, then enabled agents. Digits pick, Esc closes. */
export function createLaunchMenu(o: Opts) {
  const menu = document.createElement("div");
  menu.id = "launch-menu";
  menu.setAttribute("role", "menu");
  menu.hidden = true;
  document.body.appendChild(menu);

  let anchor: HTMLElement | null = null;

  function close(refocus = true) {
    if (menu.hidden) return;
    menu.hidden = true;
    anchor?.setAttribute("aria-expanded", "false");
    anchor = null;
    if (refocus) o.onClose();
  }

  function item(key: string, name: string, hint: string, run: () => void, cls = "", icon?: HTMLElement) {
    const { row: b, key: k, name: n, trailing: h } = menuRow(name, hint, "lm-item " + cls);
    followPointer(b);
    k.textContent = key;
    b.append(k);
    if (icon) b.append(icon);
    b.append(n, h);
    b.addEventListener("click", () => {
      close(false);
      run();
    });
    return b;
  }

  /** `at` is the + button it hangs off, or the point of a right-click. */
  function open(at: HTMLElement | { x: number; y: number }, p: Project, w: Worktree) {
    // A render may have replaced the + a menu held on to: hang off the one on screen.
    const el = at instanceof HTMLElement
      ? at.isConnected ? at : document.querySelector<HTMLElement>(`#sidebar-scroll .wt-plus[data-wt="${CSS.escape(w.path)}"]`)
      : null;
    if (el && !menu.hidden && anchor === el) return close();
    close(false);
    menuOpened();
    anchor = el;
    el?.setAttribute("aria-expanded", "true");
    const title = document.createElement("div");
    title.className = "lm-title";
    title.textContent = startIn(w);
    const sk = o.skipper(p);
    const rows = o.choices().map((a, i) =>
      item(launchDigit(i + (sk ? 1 : 0)), a?.id ?? t("menu.shell"), "", () => o.start(p, w, a), "", launchIcon(a)),
    );
    if (sk) {
      const b = item(launchDigit(0), sk.label, "", sk.run, "", skipperIcon());
      b.disabled = !!sk.disabled;
      rows.unshift(b);
    }
    const head = document.createElement("div");
    head.className = "lm-head";
    head.textContent = upper(t("launch.newGroupHead"));
    const splits = o.splits().map((s, i) => {
      const g = document.createElement("span");
      g.className = "lm-glyph";
      g.append(s.glyph);
      return item("", s.name, "", () => o.split(p, w, i), "", g);
    });
    const settings = item("", t("launch.agentSettings"), "", () => o.openSettings(), "lm-settings");
    menu.replaceChildren(title, ...rows, head, ...splits, settings);

    menu.hidden = false;
    const h = menu.offsetHeight;
    const r = el ? el.getBoundingClientRect() : null;
    const left = r ? r.right : at instanceof HTMLElement ? 0 : at.x;
    const top = r ? r.top : at instanceof HTMLElement ? 0 : at.y;
    menu.style.left = `${Math.max(0, Math.min(left, window.innerWidth - menu.offsetWidth))}px`;
    menu.style.top = `${Math.max(0, Math.min(top, window.innerHeight - h))}px`;
    rows.find((b) => !b.disabled)?.focus();
  }

  menu.addEventListener("keydown", (e) => {
    const items = [...menu.querySelectorAll<HTMLButtonElement>(".lm-item:not(:disabled)")];
    const i = items.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "Escape") close();
    else if (e.key === "ArrowDown") items[(i + 1) % items.length]?.focus();
    else if (e.key === "ArrowUp") items[(i - 1 + items.length) % items.length]?.focus();
    else if (/^[1-9]$/.test(e.key)) items.find((b) => b.querySelector(".lm-key")?.textContent === e.key)?.click();
    else return;
    e.preventDefault();
    e.stopPropagation();
  });
  document.addEventListener("mousedown", (e) => {
    if (!menu.hidden && !menu.contains(e.target as Node) && e.target !== anchor && !anchor?.contains(e.target as Node)) close(false);
  });

  /** The same choices as entries of another menu: the project menu of a project with one worktree. */
  function entries(p: Project, w: Worktree): MenuEntry[] {
    const sk = o.skipper(p);
    return [
      { head: startIn(w) },
      ...(sk ? [{ icon: "tools-sparkles", label: sk.label, hint: launchDigit(0), disabled: sk.disabled, run: sk.run }] : []),
      ...o.choices().map((a, i) => ({ glyph: launchIcon(a), label: a?.id ?? t("menu.shell"), hint: launchDigit(i + (sk ? 1 : 0)), run: () => o.start(p, w, a) })),
      {
        icon: "code-group",
        label: t("select.newGroup"),
        sub: o.splits().map((s, i) => {
          const g = document.createElement("span");
          g.className = "lm-glyph";
          g.append(s.glyph);
          return { glyph: g, label: s.name, run: () => o.split(p, w, i) };
        }),
      },
    ];
  }

  return { open, close, entries };
}
