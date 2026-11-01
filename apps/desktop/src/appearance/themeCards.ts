import { isLight, mix, readable } from "./colors";
import { COLOR_FAMILIES, type ColorFamily, themeProfile } from "./themeProfile";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { dialogFrame, dialogHeader } from "../ui/dialogParts";
import { h } from "../ui/dom";
import { icon } from "../ui/icons";
import type { TerminalTheme } from "../platform/types";
import { t, tn } from "../i18n";

/** A color family in the current language. */
const familyName = (f: ColorFamily) => t(`family.${f}`);

/** A small Skiff window in the theme's colors: a sidebar and a few terminal lines. */
function preview(t: TerminalTheme): HTMLElement {
  const p = t.palette.map((c) => isLight(t.background) ? readable(c, [t.background], 7) : c);
  const fg = isLight(t.background) ? readable(t.foreground, [t.background], 7) : t.foreground;
  const box = h("span", "tc-preview");
  box.style.background = t.background;
  const side = h("span", "tc-side");
  side.style.background = mix(t.background, t.foreground, 0.06);
  side.style.borderRight = `1px solid ${mix(t.background, t.foreground, 0.16)}`;
  for (const w of [70, 50, 60]) {
    const bar = h("span", "tc-bar");
    bar.style.width = `${w}%`;
    bar.style.background = mix(t.background, t.foreground, 0.35);
    side.appendChild(bar);
  }
  const term = h("span", "tc-term");
  const line = (parts: [string, string][]) => {
    const l = h("span", "tc-line");
    for (const [text, color] of parts) {
      const s = h("span", "", text);
      s.style.color = color;
      l.appendChild(s);
    }
    term.appendChild(l);
  };
  line([["~/skiff main $ cargo test", fg]]);
  line([["   Compiling skiffd", fg]]);
  line([["warning", p[3]], [": unused var", readable(mix(fg, t.background, 0.3), [t.background], 4.5)]]);
  line([["error", p[1]], ["[E0308] types", fg]]);
  const cursor = h("span", "tc-cursor");
  cursor.style.background = t.cursor ?? t.foreground;
  const last = h("span", "tc-line");
  const prompt = h("span", "", "$ ");
  prompt.style.color = fg;
  last.append(prompt, cursor);
  term.appendChild(last);
  box.append(side, term);
  return box;
}

/** A representative hue for each color family's swatch. */
const SWATCH: Record<string, string> = {
  Neutral: "#8a8f98", Beige: "#e4d2ac", Red: "#e5484d", Orange: "#f07630", Yellow: "#e8c547", Green: "#46a758",
  Cyan: "#2fb7c4", Blue: "#3e7bfa", Purple: "#8e5cf6", Pink: "#e45fa8",
  Unclassified: "#8a8f98",
};

export interface ThemeFilters {
  query: string;
  family: string | null;
}

const pickerFilters: ThemeFilters = { query: "", family: null };

export interface CardOpts {
  /** The id shown as active. */
  active: string | null;
  pick: (id: string | null) => void;
  /** A first card that clears the choice, e.g. "Same as app". */
  none?: { label: string; theme: TerminalTheme | undefined };
  reload?: () => Promise<TerminalTheme[]>;
  filters?: ThemeFilters;
  /** Cards a lower scope uses on screen, with a label that names it: "Project: skiff". */
  scopes?: { id: string; label: string }[];
}

/** A grid of theme cards. A click picks; nothing previews on hover. */
export function themeGrid(themes: TerminalTheme[], o: CardOpts): HTMLElement {
  const filters = o.filters ?? { query: "", family: null };
  const browser = h("div", "tc-browser");
  const tools = h("div", "tc-tools");
  const search = h("input", "tc-search");
  search.type = "search";
  search.placeholder = t("theme.search");
  search.setAttribute("aria-label", t("theme.searchAria"));
  search.value = filters.query;
  // One swatch per color family. "All" clears the filter.
  const swatches = h("div", "tc-swatches");
  swatches.setAttribute("role", "group");
  swatches.setAttribute("aria-label", t("theme.color"));
  const swatch = (value: ColorFamily | null) => {
    const b = h("button", "tc-swatch" + (value ? "" : " all"), value ? "" : t("theme.all"));
    b.type = "button";
    b.title = value ? familyName(value) : t("theme.allColors");
    b.setAttribute("aria-label", b.title);
    b.setAttribute("aria-pressed", String(value === filters.family));
    if (value) b.style.setProperty("--sw", SWATCH[value]);
    b.addEventListener("click", () => {
      filters.family = value;
      for (const s of swatches.children) s.setAttribute("aria-pressed", String(s === b));
      draw();
    });
    return b;
  };
  swatches.append(swatch(null), ...COLOR_FAMILIES.map(swatch));
  tools.append(search, swatches);
  const status = h("div", "tc-status");
  status.setAttribute("role", "status");
  const grid = h("div", "tc-grid");
  const card = (theme: TerminalTheme, id: string | null, label: string, badge: string) => {
    const on = o.active === id;
    const b = h("button", "tc-card" + (on ? " active" : ""));
    b.type = "button";
    b.dataset.theme = id ?? "";
    b.setAttribute("aria-pressed", String(on));
    b.appendChild(preview(theme));
    const foot = h("span", "tc-foot");
    const signature = h("span", "tc-signature");
    signature.style.background = themeProfile(theme).color;
    signature.setAttribute("aria-hidden", "true");
    foot.appendChild(signature);
    foot.append(h("span", "tc-name", label), h("span", "tc-badge", badge));
    if (on) foot.appendChild(h("span", "tc-state", t("theme.active")));
    for (const s of o.scopes ?? []) if (s.id === id) foot.appendChild(h("span", "tc-in-scope", s.label));
    b.appendChild(foot);
    b.addEventListener("click", () => {
      o.active = id;
      // Update the mark in place. Keep the filter, scroll and focused card.
      for (const c of grid.querySelectorAll<HTMLButtonElement>(".tc-card")) {
        const active = c.dataset.theme === (id ?? "");
        c.classList.toggle("active", active);
        c.setAttribute("aria-pressed", String(active));
        c.querySelector(".tc-state")?.remove();
        if (active) c.querySelector(".tc-foot")?.appendChild(h("span", "tc-state", "✓ Active"));
      }
      o.pick(id);
    });
    return b;
  };
  const draw = () => {
    grid.replaceChildren();
    if (o.none?.theme) grid.appendChild(card(o.none.theme, null, o.none.label, t("theme.default")));
    const words = filters.query.toLowerCase().trim().split(/\s+/);
    const shown = themes.filter((t) => {
      const p = themeProfile(t);
      // A color matches by its English id and by its name in the current language.
      const text = `${t.name} ${p.family} ${familyName(p.family)}`.toLowerCase();
      return words.every((word) => text.includes(word))
        && (!filters.family || filters.family === p.family);
    });
    for (const t of shown) {
      const p = themeProfile(t);
      grid.appendChild(card(t, t.id, t.name, familyName(p.family)));
    }
    status.textContent = tn("theme.count", themes.length, { n: shown.length });
  };
  search.addEventListener("input", () => {
    filters.query = search.value;
    draw();
  });
  if (o.reload) {
    const tool = (label: string, paths: string) => {
      const b = h("button", "tc-tool");
      b.type = "button";
      b.title = label;
      b.setAttribute("aria-label", label);
      const svg = icon(paths);
      svg.setAttribute("width", "16");
      svg.setAttribute("height", "16");
      b.appendChild(svg);
      return b;
    };
    const add = tool(t("theme.import"), '<path d="M12 4v11M7 10l5 5 5-5M5 20h14"></path>');
    const folder = tool(t("theme.openFolder"), '<path d="M3 6h6l2 2h10v11H3z"></path>');
    const reload = async () => { themes = await o.reload!(); draw(); };
    add.addEventListener("click", async () => {
      try {
        const paths = await open({ multiple: true, title: t("theme.importTitle") });
        if (!paths) return;
        add.disabled = true;
        const errors = await invoke<string[]>("import_themes", { paths: Array.isArray(paths) ? paths : [paths] });
        await reload();
        if (errors.length) status.textContent = errors.join("; ");
      } catch (e) { status.textContent = String(e); }
      finally { add.disabled = false; }
    });
    folder.addEventListener("click", () => {
      invoke("open_theme_folder").catch((e) => { status.textContent = String(e); });
      // Pick up files the user drops in the folder when they come back.
      window.addEventListener("focus", () => { if (browser.isConnected) reload().catch(() => {}); }, { once: true });
    });
    tools.append(add, folder);
  }
  browser.append(tools, status, grid);
  draw();
  return browser;
}

const KEYS: Record<string, "left" | "right" | "up" | "down" | "first" | "last"> = {
  ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down", Home: "first", End: "last",
};

/** The card an arrow key lands on. Up and down keep to the nearest column; the ends stop. */
export function nextCard(
  cards: HTMLElement[],
  at: number,
  key: "left" | "right" | "up" | "down" | "first" | "last",
  /** Where a card's line starts. Default: its own top. */
  top: (c: HTMLElement) => number = (c) => c.getBoundingClientRect().top,
): number {
  if (key === "first") return 0;
  if (key === "last") return cards.length - 1;
  if (key === "left") return Math.max(0, at - 1);
  if (key === "right") return Math.min(cards.length - 1, at + 1);
  const rect = cards[at].getBoundingClientRect();
  const y = top(cards[at]);
  const dir = key === "down" ? 1 : -1;
  let best = at;
  let bestRow = Infinity;
  let bestDx = Infinity;
  for (const [i, c] of cards.entries()) {
    const r = c.getBoundingClientRect();
    const dy = (top(c) - y) * dir;
    if (dy <= 1) continue;
    const dx = Math.abs(r.left - rect.left);
    if (dy < bestRow - 1 || (Math.abs(dy - bestRow) <= 1 && dx < bestDx)) {
      best = i;
      bestRow = dy;
      bestDx = dx;
    }
  }
  return best;
}

/** Theme picker: a modal of cards. `scope` says what the theme applies to. A click applies and closes. */
export function pickTheme(title: string, scope: string, themes: TerminalTheme[], o: CardOpts): void {
  const back = document.activeElement as HTMLElement | null;
  const { overlay, panel } = dialogFrame("tc-panel", "dialog", title);
  const head = dialogHeader(title, () => close());
  const body = h("div", "tc-body");
  const close = () => {
    overlay.remove();
    back?.focus?.();
  };
  body.appendChild(
    themeGrid(themes, {
      ...o,
      filters: o.filters ?? pickerFilters,
      pick: (id) => {
        close();
        o.pick(id);
      },
    }),
  );
  panel.append(head, h("div", "tc-scope", scope), body);
  overlay.appendChild(panel);
  document.body.appendChild(overlay);
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });
  overlay.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      close();
      return;
    }
    const step = KEYS[e.key];
    if (!step || e.ctrlKey || e.altKey || e.metaKey || !(e.target instanceof HTMLElement)) return;
    const cards = [...body.querySelectorAll<HTMLButtonElement>(".tc-card")];
    const search = body.querySelector<HTMLInputElement>(".tc-search");
    // Down from the search goes into the cards, at the active theme. Left and right stay in the text.
    if (e.target === search) {
      if (step !== "down" || !cards.length) return;
      e.preventDefault();
      document.body.classList.add("kbd");
      (cards.find((c) => c.classList.contains("active")) ?? cards[0]).focus();
      return;
    }
    if (!e.target.closest(".tc-card")) return;
    // Arrows move between cards, not the panel's scroll. Up from the top row goes back to the search.
    e.preventDefault();
    document.body.classList.add("kbd");
    const at = Math.max(0, cards.indexOf(document.activeElement as HTMLButtonElement));
    const next = nextCard(cards, at, step);
    if (step === "up" && next === at && search) search.focus();
    else cards[next]?.focus();
  });
  body.querySelector<HTMLInputElement>(".tc-search")?.focus();
}
