import { isMac } from "../app/keys";
import { sessions } from "../app/state";
import { accent, place } from "../app/stateQueries";
import { copyText } from "../platform/clipboard";
import { showError } from "../ui/alerts";
import { confirmAction } from "../ui/confirm";
import { h } from "../ui/dom";
import { icon } from "../ui/icons";
import { taskTitle } from "../workspace/model";
import {
  type Image, allImages, countText, deleteImages, fillThumb, insertInto, insertTarget, marked, matches, openImage, ordered,
  imagesError, pinImages, queryWords, searchField, section, shownName, when, whoLine,
} from "../terminal/images";
import { size } from "./skiffFolderDialog";
import { t, tn, upper } from "../i18n";

/** The page's state. It survives every redraw of Settings. */
export interface ImagesState {
  query: string;
  /** Selected paths. */
  sel: Set<string>;
  /** Where Shift+click and Shift+arrows start. */
  anchor: string | null;
  /** The current tile: arrows move it, Space toggles it. */
  now: string | null;
  /** Where the keys were, so a redraw puts them back. */
  keys: "field" | "grid" | null;
  scroll: number;
  /** The count cell says "Copied". */
  copied: boolean;
  copiedTimer: number;
  /** The tiles that Pin or Unpin moves: the rebuild that shows them in their new
   *  section scrolls the current one into view. */
  moving: { paths: string[]; pinned: boolean } | null;
}

export const imagesState = (): ImagesState => ({ query: "", sel: new Set(), anchor: null, now: null, keys: null, scroll: 0, copied: false, copiedTimer: 0, moving: null });


/** The cache folder with `~` for home, from an image's path. */
function folder(): string {
  const p = allImages()[0]?.path;
  if (!p) return "";
  return p.slice(0, p.lastIndexOf("/")).replace(/^\/(home|Users)\/[^/]+/, "~");
}

/**
 * Settings › Images: the library as a gallery. Search, select as in a file
 * manager, and act on the selection: Open, Insert, Pin, Copy path, Delete.
 * Insert closes Settings, so the pane shows the insert signals. `close`
 * closes Settings.
 */
export function imagesSection(st: ImagesState, close: () => void): HTMLElement {
  const main = h("div", "set-main img-page");
  const head = h("div", "set-head");
  head.append(h("div", "set-h", t("images.title")), h("div", "set-sub mono", folder()));

  const tools = h("div", "img-tools");
  const field = searchField();
  field.value = st.query;
  const count = h("span", "lib-count");
  const find = h("div", "lib-head");
  find.append(icon('<circle cx="10.5" cy="10.5" r="6.5"></circle><path d="M15.5 15.5L20 20"></path>'), field, count);
  const btn = (cls: string, run: () => void) => {
    const b = h("button", "img-btn" + cls);
    b.type = "button";
    b.addEventListener("click", run);
    return b;
  };
  const openB = btn("", () => openAll());
  openB.textContent = t("menu.open");
  const insB = btn(" ins", () => void insert());
  const pinB = btn("", () => void pin());
  const copyB = btn("", () => void copy());
  const delB = btn(" del", () => void remove());
  delB.textContent = t("images.delete");
  tools.append(find, openB, insB, pinB, copyB, delB);

  const scroll = h("div", "img-scroll");
  scroll.tabIndex = 0;
  scroll.setAttribute("role", "listbox");
  scroll.setAttribute("aria-multiselectable", "true");
  scroll.setAttribute("aria-label", t("images.title"));
  main.append(head, tools, scroll);

  const seen = new IntersectionObserver((es) => {
    for (const e of es) {
      if (!e.isIntersecting) continue;
      const box = e.target as HTMLElement;
      seen.unobserve(box);
      fillThumb(box, box.dataset.path!, 480);
    }
  }, { root: scroll });

  let shown: Image[] = [];
  const tiles = new Map<string, HTMLElement>();
  const picked = () => shown.filter((i) => st.sel.has(i.path));

  /** The grid, built again on a new query or a library change. */
  function build() {
    const words = queryWords(st.query);
    shown = ordered().filter((i) => matches(i, words));
    const live = new Set(shown.map((i) => i.path));
    // A new query keeps the selected images that still match.
    for (const p of st.sel) if (!live.has(p)) st.sel.delete(p);
    if (st.anchor && !live.has(st.anchor)) st.anchor = null;
    if (st.now && !live.has(st.now)) st.now = shown[0]?.path ?? null;
    seen.disconnect();
    tiles.clear();
    const kids: HTMLElement[] = [];
    const error = imagesError();
    if (error) {
      const el = h("div", "set-error", error);
      el.setAttribute("role", "alert");
      kids.push(el);
    } else if (!allImages().length) kids.push(h("div", "lib-none", t("images.none")));
    else if (!shown.length) kids.push(h("div", "lib-none", t("images.noMatch")));
    const pins = shown.filter((i) => i.pinned);
    const rest = shown.filter((i) => !i.pinned);
    const grid = (part: Image[]) => {
      const g = h("div", "img-grid");
      for (const i of part) g.append(tile(i, words));
      return g;
    };
    if (pins.length) kids.push(section(upper(t("images.pinned")), pins.length), grid(pins));
    if (rest.length) {
      if (pins.length) kids.push(section(upper(t("images.recent")), rest.length));
      kids.push(grid(rest));
    }
    scroll.replaceChildren(...kids);
    const off = !allImages().length;
    field.disabled = off;
    paint();
  }

  function tile(i: Image, words: string[]): HTMLElement {
    const t = h("div", "img-tile");
    t.setAttribute("role", "option");
    t.dataset.path = i.path;
    const pic = h("div", "img-pic");
    pic.dataset.path = i.path;
    seen.observe(pic);
    const cap = h("div", "img-cap");
    const name = h("span", "lib-name");
    name.append(...marked(shownName(i), words));
    cap.append(name, h("span", "lib-time", when(i.at)));
    if (i.session) cap.append(whoLine(i, words));
    t.append(pic, cap);
    t.addEventListener("click", (e) => {
      e.stopPropagation();
      click(i.path, e.ctrlKey || e.metaKey, e.shiftKey);
    });
    t.addEventListener("dblclick", () => openAll());
    tiles.set(i.path, t);
    return t;
  }

  /** Selection, the current tile and the tool row; no rebuild, so the thumbnails stay. */
  function paint() {
    for (const [p, tile] of tiles) {
      const on = st.sel.has(p);
      tile.classList.toggle("sel", on);
      tile.classList.toggle("now", p === st.now);
      tile.setAttribute("aria-selected", String(on));
    }
    const sel = picked();
    const words = queryWords(st.query);
    count.textContent = st.copied ? t("images.copied") : countText(shown.length, words) + (sel.length ? ` · ${t("images.selected", { n: sel.length })}` : "");
    const none = !sel.length;
    openB.disabled = none;
    pinB.disabled = none;
    copyB.disabled = none;
    delB.disabled = none;
    pinB.textContent = t(sel.length && sel.every((i) => i.pinned) ? "images.unpin" : "images.pin");
    copyB.textContent = t(sel.length > 1 ? "images.copyPaths" : "menu.copyPath");
    // The label names the target: the project letters in their color, then the session title.
    const id = insertTarget();
    const s = id ? sessions.get(id) : undefined;
    insB.replaceChildren();
    insB.disabled = none || !id;
    if (!s) insB.textContent = t("images.insert");
    else {
      const p = place(s)?.project;
      const label = h("span", "t");
      label.append(t("images.insertInto"));
      if (p) {
        const mark = h("span", "lib-proj", p.short);
        mark.style.color = accent(p);
        label.append(mark);
      }
      label.append(taskTitle(s));
      insB.append(label);
    }
  }

  const at = (p: string | null) => shown.findIndex((i) => i.path === p);

  function range(from: string | null, to: string): string[] {
    const a = Math.max(0, at(from ?? to));
    const b = at(to);
    return shown.slice(Math.min(a, b), Math.max(a, b) + 1).map((i) => i.path);
  }

  function click(path: string, add: boolean, extend: boolean) {
    if (extend) {
      if (!add) st.sel.clear();
      for (const p of range(st.anchor, path)) st.sel.add(p);
    } else {
      if (add) {
        if (!st.sel.delete(path)) st.sel.add(path);
      } else st.sel = new Set([path]);
      st.anchor = path;
    }
    st.now = path;
    paint();
  }

  /** The tile in the row above or below, nearest in x; across the PINNED and RECENT grids too. */
  function vertical(path: string, dir: 1 | -1): string | null {
    const r = tiles.get(path)?.getBoundingClientRect();
    if (!r) return null;
    let best: [number, number, string] | null = null;
    for (const [p, t] of tiles) {
      const q = t.getBoundingClientRect();
      const dy = (q.top - r.top) * dir;
      if (dy < 1) continue;
      const score: [number, number] = [dy, Math.abs(q.left - r.left)];
      if (!best || score[0] < best[0] - 1 || (Math.abs(score[0] - best[0]) <= 1 && score[1] < best[1])) best = [...score, p];
    }
    return best?.[2] ?? null;
  }

  function move(to: string | null, extend: boolean) {
    if (!to) return;
    st.now = to;
    if (extend) st.sel = new Set(range(st.anchor, to));
    else {
      st.sel = new Set([to]);
      st.anchor = to;
    }
    paint();
    tiles.get(to)?.scrollIntoView({ block: "nearest" });
  }

  function openAll() {
    for (const i of picked()) openImage(i);
  }

  async function insert() {
    const id = insertTarget();
    const sel = picked();
    if (!id || !sel.length) return;
    // Close first: the keys go to that pane, where the signals show.
    close();
    await insertInto(id, sel);
  }

  async function pin() {
    const sel = picked();
    if (!sel.length) return;
    const on = !sel.every((i) => i.pinned);
    st.moving = { paths: sel.map((i) => i.path), pinned: on };
    await pinImages(sel, on);
  }

  /** After Pin or Unpin: the current tile, else the first moved one, comes into view. */
  function follow() {
    const m = st.moving;
    if (!m || shown.some((i) => m.paths.includes(i.path) && i.pinned !== m.pinned)) return;
    st.moving = null;
    const to = st.now && m.paths.includes(st.now) ? st.now : m.paths.find((p) => tiles.has(p));
    if (to) tiles.get(to)?.scrollIntoView({ block: "nearest" });
  }

  async function copy() {
    const sel = picked();
    if (!sel.length) return;
    try {
      await copyText(sel.map((i) => i.path).join("\n"));
    } catch (e) {
      return showError(e);
    }
    st.copied = true;
    clearTimeout(st.copiedTimer);
    st.copiedTimer = window.setTimeout(() => {
      st.copied = false;
      if (count.isConnected) paint();
    }, 1500);
    paint();
  }

  /** One image goes at once, as in the drop-down. Two or more ask first. */
  async function remove() {
    const sel = picked();
    if (!sel.length) return;
    if (sel.length > 1) {
      const pinned = sel.filter((i) => i.pinned).length;
      const bytes = sel.reduce((n, i) => n + i.size, 0);
      const lines = [t("images.deleteBody", { images: tn("disk.images", sel.length), size: size(bytes) })];
      if (pinned) lines.push(tn("images.pinnedOf", pinned));
      const ok = await confirmAction({ title: tn("images.deleteTitle", sel.length), body: lines.join("\n"), action: t("images.delete") });
      if (!ok) return;
    }
    // The tile after the last selected one becomes current.
    const last = Math.max(...sel.map((i) => at(i.path)));
    const next = shown.slice(last + 1).find((i) => !st.sel.has(i.path)) ?? [...shown].reverse().find((i) => !st.sel.has(i.path));
    st.sel.clear();
    st.now = st.anchor = next?.path ?? null;
    await deleteImages(sel);
  }

  /** Esc: clear the selection. With none, Esc goes on to Settings, which closes. */
  function escape(e: KeyboardEvent): boolean {
    if (!st.sel.size) return false;
    st.sel.clear();
    paint();
    e.preventDefault();
    e.stopPropagation();
    return true;
  }

  field.addEventListener("input", () => {
    st.query = field.value;
    build();
    scroll.scrollTop = 0;
  });
  field.addEventListener("keydown", (e) => {
    if (e.key === "Escape") return void escape(e);
    if (e.key !== "ArrowDown" || e.ctrlKey || e.altKey || e.metaKey || e.shiftKey) return;
    e.preventDefault();
    e.stopPropagation();
    if (!shown.length) return;
    scroll.focus();
    if (!st.now) move(shown[0].path, false);
  });

  scroll.addEventListener("click", () => {
    if (!st.sel.size) return;
    st.sel.clear();
    paint();
  });
  scroll.addEventListener("focus", () => {
    if (!st.now && shown.length) {
      st.now = shown[0].path;
      paint();
    }
  });
  scroll.addEventListener("scroll", () => (st.scroll = scroll.scrollTop));
  main.addEventListener("focusin", (e) => (st.keys = e.target === field ? "field" : e.target === scroll ? "grid" : null));

  scroll.addEventListener("keydown", (e) => {
    const mod = isMac ? e.metaKey : e.ctrlKey;
    const k = e.key.toLowerCase();
    const now = st.now ?? shown[0]?.path ?? null;
    if (e.key === "Escape") return void escape(e);
    if (e.altKey || (e.ctrlKey && !mod) || (e.metaKey && !mod)) return;
    if (mod && k === "a") {
      st.sel = new Set(shown.map((i) => i.path));
      paint();
    } else if (mod) return;
    else if (!now) return;
    else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      const n = at(now) + (e.key === "ArrowLeft" ? -1 : 1);
      const r = tiles.get(now)?.getBoundingClientRect();
      const left = r && shown[n] && Math.abs((tiles.get(shown[n].path)?.getBoundingClientRect().top ?? 0) - r.top) > 1;
      // Left from the first column goes back to the tab.
      if (e.key === "ArrowLeft" && (n < 0 || left) && !e.shiftKey) {
        document.querySelector<HTMLElement>("#settings .set-tab.active")?.focus();
      } else move(shown[Math.max(0, Math.min(n, shown.length - 1))].path, e.shiftKey);
    } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      const to = vertical(now, e.key === "ArrowUp" ? -1 : 1);
      // Up from the top row goes back to the search field.
      if (!to && e.key === "ArrowUp" && !e.shiftKey) field.focus();
      else move(to, e.shiftKey);
    } else if (e.key === " ") {
      if (!st.sel.delete(now)) st.sel.add(now);
      st.now = st.anchor = now;
      paint();
    } else if (e.key === "Enter") openAll();
    else if (k === "i") void insert();
    else if (k === "p") void pin();
    else if (k === "c") void copy();
    else if (e.key === "Delete") void remove();
    else return;
    e.preventDefault();
    e.stopPropagation();
  });

  build();
  // After Settings puts the page in the document: the scroll and the keys come back.
  queueMicrotask(() => {
    scroll.scrollTop = st.scroll;
    if (st.keys === "grid") scroll.focus();
    else if (st.keys === "field") field.focus();
    follow();
  });
  return main;
}
