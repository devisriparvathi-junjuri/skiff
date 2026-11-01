/**
 * Images: one library of this computer's image cache, newest first. Every
 * drop and Skiff Paste puts its image there. The pane header button shows
 * the newest one and opens the library. Its search row filters the list.
 * Each row opens the image, inserts its path into the focused pane, pins it,
 * copies its path or deletes it. Drag a thumbnail onto any pane to insert
 * the path there. Settings › Images (`appearance/imagesPage.ts`) shows the
 * same library as a gallery.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { hostName, isRemote } from "../app/hosts";
import { scheduleRender } from "../app/render";
import { S, sessions } from "../app/state";
import { accent, place } from "../app/stateQueries";
import { copyText } from "../platform/clipboard";
import { showError } from "../ui/alerts";
import { button, h } from "../ui/dom";
import { icon } from "../ui/icons";
import { menuOpened } from "../ui/menu";
import { refocusTerminal } from "../workspace/view";
import { endImageDrag, showImageDrag } from "./fileDrop";
import { insertShown } from "./insertSignal";
import { focusPane } from "./paneActions";
import { type Typed, typeFiles, whoOf } from "./terminalClipboard";
import { panes } from "./terminalState";
import { locale, t, upper } from "../i18n";

/** One library image, as `images` answers. */
export interface Image {
  path: string;
  file: string;
  /** The dropped file's own name. Null: a paste, or a file Skiff has no entry for. */
  name: string | null;
  session: string | null;
  title: string | null;
  project: string | null;
  /** Unix ms of the last drop, paste or Insert. */
  at: number;
  pinned: boolean;
  /** The file's length in bytes. */
  size: number;
}

let list: Image[] = [];

/** Thumbnails by size and path ("160 /path"): a small data URL, or "" for a format the webview cannot draw. */
const thumbs = new Map<string, Promise<string>>();

const MIME: Record<string, string> = { svg: "image/svg+xml", png: "image/png", jpg: "image/jpeg", gif: "image/gif", webp: "image/webp", bmp: "image/bmp", tif: "image/tiff", heic: "image/heic", avif: "image/avif" };

/** Draws the image into a canvas `size` px on its long side, so the page keeps no full-size copy. */
function thumb(path: string, size: number): Promise<string> {
  const key = `${size} ${path}`;
  let t = thumbs.get(key);
  if (!t) {
    t = invoke<ArrayBuffer>("image_thumb", { path })
      .then((bytes) => new Promise<string>((done) => {
        const ext = path.split(".").pop()?.toLowerCase() ?? "";
        const url = URL.createObjectURL(new Blob([bytes], { type: MIME[ext] ?? "" }));
        const img = new Image();
        img.onload = () => {
          const c = document.createElement("canvas");
          const scale = Math.min(1, size / Math.max(img.naturalWidth, img.naturalHeight, 1));
          c.width = Math.max(1, Math.round(img.naturalWidth * scale));
          c.height = Math.max(1, Math.round(img.naturalHeight * scale));
          c.getContext("2d")?.drawImage(img, 0, 0, c.width, c.height);
          URL.revokeObjectURL(url);
          done(c.toDataURL());
        };
        // HEIC on Linux: an empty cell.
        img.onerror = () => {
          URL.revokeObjectURL(url);
          done("");
        };
        img.src = url;
      }))
      .catch(() => "");
    thumbs.set(key, t);
  }
  return t;
}

/** Fills `box` with the thumbnail when it is ready. */
export function fillThumb(box: HTMLElement, path: string, size = 160) {
  void thumb(path, size).then((url) => {
    if (!url || box.dataset.path !== path) return;
    const img = h("img");
    img.src = url;
    img.alt = "";
    img.draggable = false;
    box.replaceChildren(img);
  });
}

/** Why the library did not load, as one line. Null when it loaded. */
let loadError: string | null = null;
export const imagesError = () => loadError;

export const imageCount = () => list.length;
export const newestImage = (): string | null => list[0]?.path ?? null;
export const allImages = (): readonly Image[] => list;

/** Called after each load, so Settings › Images redraws too. */
const watchers = new Set<() => void>();
export const onImagesChanged = (f: () => void) => watchers.add(f);

async function load() {
  try {
    list = await invoke<Image[]>("images");
    loadError = null;
  } catch (e) {
    list = [];
    loadError = String(e);
  }
  const live = new Set(list.map((i) => i.path));
  for (const k of thumbs.keys()) if (!live.has(k.slice(k.indexOf(" ") + 1))) thumbs.delete(k);
  scheduleRender("images");
  watchers.forEach((f) => f());
  if (!lib.hidden) {
    if (!list.length) closeImages();
    else draw();
  }
}

void load();
void listen("skiff:images-changed", () => void load());

// The library: one floating surface, under the button that opened it.
const lib = h("div");
lib.id = "images-lib";
lib.setAttribute("role", "menu");
lib.setAttribute("aria-label", t("images.title"));
lib.tabIndex = -1;
lib.hidden = true;
const libCount = h("span", "lib-count");
const field = searchField();
const libHead = h("div", "lib-head");
libHead.append(icon('<circle cx="10.5" cy="10.5" r="6.5"></circle><path d="M15.5 15.5L20 20"></path>'), field, libCount);
const rows = h("div", "lib-list");
rows.tabIndex = -1;
lib.append(libHead, rows);
document.body.appendChild(lib);

/** The search field of the drop-down and the gallery: placeholder "Search". */
export function searchField(): HTMLInputElement {
  const f = h("input", "lib-search");
  f.type = "text";
  f.placeholder = t("images.search");
  f.spellcheck = false;
  f.autocomplete = "off";
  f.setAttribute("aria-label", t("images.search"));
  return f;
}

/** Where the keys are: the search field types, the list acts on the lit row. */
let keysIn: "field" | "list" = "field";

/** The lit row, by path, so a redraw keeps it. */
let lit: string | null = null;
/** The row whose right column says "Copied" (1.5 s) or "Inserted" (3 s), in place of its buttons. */
let done: { path: string; word: "Copied" | "Inserted" } | null = null;
let doneTimer = 0;

/** Pinned first, then the rest; each newest first. */
export const ordered = () => [...list.filter((i) => i.pinned), ...list.filter((i) => !i.pinned)];

/** Each row's time when the drop-down opened. Insert does not move a row
 *  while it is open; the new order shows on the next open. Pin and Unpin move
 *  the row to its section at once. */
let held: Map<string, number> | null = null;

/** The drop-down's order: pinned first, then the rest, each by the time it had
 *  when the drop-down opened. A new image has no held time, so it comes first. */
function libOrder(): Image[] {
  const o = ordered();
  if (!held) return o;
  const at = (i: Image) => held!.get(i.path) ?? i.at;
  return o.sort((a, b) => Number(b.pinned) - Number(a.pinned) || at(b) - at(a));
}

/** Lower case with no accents. Each character stays one character, so a match maps back to the text. */
const foldChar = (c: string) => (c.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase()[0] ?? c);
const fold = (t: string) => [...t].map(foldChar).join("");

/** The query's words, folded. */
export const queryWords = (q: string) => fold(q).split(/\s+/).filter(Boolean);

/** Every word is in the shown name, the session title, the project letters or the project name. */
export function matches(i: Image, words: string[]): boolean {
  if (!words.length) return true;
  const p = i.project ? S.projects.find((x) => x.name === i.project) : undefined;
  const hay = fold([shownName(i), i.title ?? "", p?.short ?? "", i.project ?? ""].join("\n"));
  return words.every((w) => hay.includes(w));
}

/** The matched parts in weight 600, with no color. */
export function marked(text: string, words: string[]): (string | HTMLElement)[] {
  if (!words.length) return [text];
  const chars = [...text];
  const f = chars.map(foldChar).join("");
  const on = new Array<boolean>(chars.length).fill(false);
  for (const w of words) for (let at = f.indexOf(w); at >= 0; at = f.indexOf(w, at + 1)) on.fill(true, at, at + w.length);
  const out: (string | HTMLElement)[] = [];
  for (let n = 0; n < chars.length; ) {
    let e = n;
    while (e < chars.length && on[e] === on[n]) e++;
    const part = chars.slice(n, e).join("");
    out.push(on[n] ? h("mark", "", part) : part);
    n = e;
  }
  return out;
}

const query = () => queryWords(field.value);
const shown = () => libOrder().filter((i) => matches(i, query()));

const seen = new IntersectionObserver((es) => {
  for (const e of es) {
    if (!e.isIntersecting) continue;
    const box = e.target as HTMLElement;
    seen.unobserve(box);
    fillThumb(box, box.dataset.path!);
  }
}, { root: rows });

export function when(at: number): string {
  const d = new Date(at);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  return d.toLocaleDateString(locale(), { month: "short", day: "numeric" });
}

/** Line 2: the project letters in its color and the session title. Dim once that session ended. */
export function whoLine(i: Image, words: string[] = []): HTMLElement {
  const s = i.session ? sessions.get(i.session) : undefined;
  const line = h("span", "lib-who" + (!s || s.state === "done" ? " ended" : ""));
  const p = i.project ? S.projects.find((x) => x.name === i.project) : undefined;
  if (p) {
    const mark = h("span", "lib-proj");
    mark.append(...marked(p.short, words));
    mark.style.color = accent(p);
    line.append(mark);
  }
  line.append(...marked(i.title ?? "", words));
  return line;
}

function act(label: string, run: () => void, cls = ""): HTMLButtonElement {
  const b = button("lib-act" + cls, label, run);
  // The row's own click opens the image.
  b.addEventListener("click", (e) => e.stopPropagation());
  b.tabIndex = -1;
  return b;
}

/** The dropped file's name; "Pasted image" for a paste. An image from
 *  before the index has no entry: "PNG image", or "Image" with no extension,
 *  never its cache hash. */
export function shownName(i: Image): string {
  if (i.name) return i.name;
  if (i.session) return t("images.pasted");
  const ext = i.file.includes(".") ? i.file.split(".").pop()!.toUpperCase() : "";
  return ext ? t("images.ofType", { ext }) : t("images.image");
}

function row(i: Image, words: string[]): HTMLElement {
  const word = done?.path === i.path ? done.word : null;
  const r = h("div", "lib-row" + (i.path === lit ? " lit" : "") + (word ? " copied" : ""));
  r.setAttribute("role", "menuitem");
  r.dataset.path = i.path;
  const th = h("span", "lib-th");
  th.dataset.path = i.path;
  seen.observe(th);
  th.addEventListener("pointerdown", (e) => startDrag(e, i, th));
  const text = h("span", "lib-text");
  const name = h("span", "lib-name");
  name.append(...marked(shownName(i), words));
  text.append(name);
  if (i.session) text.append(whoLine(i, words));
  const right = h("span", "lib-right");
  const x = act("", () => void remove(i), " x");
  x.appendChild(icon('<path d="M6 6l12 12M18 6L6 18"></path>'));
  x.setAttribute("aria-label", t("images.delete"));
  right.append(
    doneCell(word, i.at),
    act(t("images.insert"), () => void insertImage(i)),
    act(t(i.pinned ? "images.unpin" : "images.pin"), () => void pin(i)),
    act(t("menu.copyPath"), () => void copyPath(i)),
    x,
  );
  r.append(th, text, right);
  r.addEventListener("mousemove", () => light(i.path));
  r.addEventListener("click", () => openImage(i));
  return r;
}

/** The time; else "Copied", or "Inserted" with a check. */
function doneCell(word: string | null, at: number): HTMLElement {
  const cell = h("span", "lib-time" + (word ? " copied" : ""), word ? t(word === "Inserted" ? "images.inserted" : "images.copied") : when(at));
  if (word === "Inserted") cell.prepend(icon('<path d="M4 12.5l5 5 11-11"></path>'));
  return cell;
}

/** Shows `word` on the row: "Inserted" for 3 s, "Copied" for 1.5 s. */
function showDone(path: string, word: "Copied" | "Inserted") {
  done = { path, word };
  clearTimeout(doneTimer);
  doneTimer = window.setTimeout(() => {
    done = null;
    if (!lib.hidden) draw();
  }, word === "Inserted" ? 3000 : 1500);
  if (!lib.hidden) draw();
}

export function section(name: string, n: number): HTMLElement {
  const s = h("div", "lib-sec");
  s.append(name, h("span", "", String(n)));
  return s;
}

/** "8", or "2 of 8" with a query. */
export const countText = (n: number, words: string[]) => (words.length ? t("images.countOf", { n, total: list.length }) : String(list.length));

function draw() {
  const words = query();
  const o = shown();
  if (!o.some((i) => i.path === lit)) lit = o[0]?.path ?? null;
  libCount.textContent = countText(o.length, words);
  lib.classList.toggle("in-list", keysIn === "list");
  const pins = o.filter((i) => i.pinned).length;
  const kids: HTMLElement[] = [];
  if (!o.length) kids.push(h("div", "lib-none", t("images.noMatch")));
  o.forEach((i, n) => {
    if (pins && n === 0) kids.push(section(upper(t("images.pinned")), pins));
    if (pins && n === pins && pins < o.length) kids.push(section(upper(t("images.recent")), o.length - pins));
    kids.push(row(i, words));
  });
  seen.disconnect();
  rows.replaceChildren(...kids);
  rows.querySelectorAll<HTMLElement>(".lib-th").forEach((t) => seen.observe(t));
  if (moving && o.some((i) => i.path === moving!.path && i.pinned === moving!.pinned)) {
    moving = null;
    rows.querySelector<HTMLElement>(".lib-row.lit")?.scrollIntoView({ block: "nearest" });
  }
}

function light(path: string | null) {
  if (lit === path) return;
  lit = path;
  rows.querySelectorAll<HTMLElement>(".lib-row").forEach((r) => r.classList.toggle("lit", r.dataset.path === path));
  rows.querySelector<HTMLElement>(".lib-row.lit")?.scrollIntoView({ block: "nearest" });
}

const litImage = () => list.find((i) => i.path === lit);

export function openImage(i: Image) {
  invoke("open_image", { path: i.path }).catch(showError);
}

/** Inserts the paths into `id`'s pane, as a drop of these files types them. False: it failed, and the error shows. */
export async function insertInto(id: string, imgs: readonly Image[]): Promise<boolean> {
  try {
    const t = await invoke<Typed>("image_insert", { paths: imgs.map((i) => i.path), who: whoOf(id), host: isRemote() ? hostName() : null });
    typeFiles(id, t);
    return true;
  } catch (e) {
    showError(e);
    return false;
  }
}

/** The pane Insert types into: the focused one. Null when it has no terminal. */
export const insertTarget = (): string | null => (S.focused && panes.has(S.focused) ? S.focused : null);

async function insertImage(i: Image) {
  const id = insertTarget();
  if (!id) return;
  if (await insertInto(id, [i])) showDone(i.path, "Inserted");
}

export async function pinImages(imgs: readonly Image[], on: boolean) {
  await invoke("image_pin", { paths: imgs.map((i) => i.path), on }).catch(showError);
}

/** A row that Pin or Unpin moves: the next draw that shows it in its new
 *  section scrolls it into view. The light stays on it. */
let moving: { path: string; pinned: boolean } | null = null;

function pin(i: Image) {
  lit = i.path;
  moving = { path: i.path, pinned: !i.pinned };
  return pinImages([i], !i.pinned);
}

export async function deleteImages(imgs: readonly Image[]) {
  await invoke("image_delete", { paths: imgs.map((i) => i.path) }).catch(showError);
}

async function copyPath(i: Image) {
  try {
    await copyText(i.path);
  } catch (e) {
    return showError(e);
  }
  showDone(i.path, "Copied");
}

async function remove(i: Image) {
  // The row below takes the light.
  const o = shown();
  const n = o.findIndex((x) => x.path === i.path);
  lit = (o[n + 1] ?? o[n - 1])?.path ?? null;
  await deleteImages([i]);
}

/** The keys go to the field. `typed` goes in at the caret: a character, or "\b" to remove one. */
function toField(typed = "") {
  keysIn = "field";
  if (typed === "\b") field.value = field.value.slice(0, -1);
  else field.value += typed;
  field.focus();
  lit = null;
  draw();
}

field.addEventListener("input", () => {
  // The first match is always lit, so Enter opens it.
  lit = null;
  draw();
  rows.scrollTop = 0;
});
field.addEventListener("focus", () => {
  if (keysIn === "list") {
    keysIn = "field";
    draw();
  }
});
field.addEventListener("keydown", (e) => {
  const i = litImage();
  if (e.key === "Escape") closeImages();
  else if (e.key === "ArrowDown") {
    if (!shown().length) return e.preventDefault();
    keysIn = "list";
    rows.focus();
    draw();
  } else if (e.key === "Enter" && i) openImage(i);
  else if (e.key !== "Tab") {
    // Letters, Backspace and Delete edit the query.
    if (!e.ctrlKey && !e.metaKey) e.stopPropagation();
    return;
  }
  e.preventDefault();
  e.stopPropagation();
});

rows.addEventListener("keydown", (e) => {
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  const o = shown();
  const n = o.findIndex((i) => i.path === lit);
  const i = litImage();
  const k = e.key.toLowerCase();
  if (e.key === "Escape") closeImages();
  else if (e.key === "ArrowDown") light(o[Math.min(n + 1, o.length - 1)]?.path ?? null);
  else if (e.key === "ArrowUp") {
    if (n <= 0) toField();
    else light(o[n - 1].path);
  } else if (e.key === "Enter" && i) openImage(i);
  else if (k === "i" && i) void insertImage(i);
  else if (k === "p" && i) void pin(i);
  else if (k === "c" && i) void copyPath(i);
  else if (e.key === "Delete" && i) void remove(i);
  else if (e.key === "Backspace") toField("\b");
  else if (e.key.length === 1) toField(e.key);
  else if (e.key !== "Tab") return;
  e.preventDefault();
  e.stopPropagation();
});

document.addEventListener("mousedown", (e) => {
  const t = e.target as Element;
  if (!lib.hidden && !lib.contains(t) && !t.closest(".head-images")) closeImages(false);
}, true);

function expanded(on: boolean) {
  document.querySelectorAll(".head-images").forEach((b) => b.setAttribute("aria-expanded", String(on)));
}

/** Opens the library under `at`, its right edge on the button's right edge. */
function openAt(at: DOMRect) {
  if (!list.length) return;
  menuOpened();
  field.value = "";
  keysIn = "field";
  lit = null;
  held = new Map(list.map((i) => [i.path, i.at]));
  const s = S.focused ? sessions.get(S.focused) : undefined;
  lib.style.setProperty("--pc", accent(s ? place(s)?.project : null));
  lib.hidden = false;
  draw();
  const w = lib.offsetWidth;
  const hgt = lib.offsetHeight;
  lib.style.left = `${Math.max(0, Math.min(at.right - w, window.innerWidth - w - 8))}px`;
  lib.style.top = `${Math.max(0, Math.min(at.bottom, window.innerHeight - hgt - 8))}px`;
  expanded(true);
  field.focus();
}

export function closeImages(refocus = true) {
  if (lib.hidden) return;
  lib.hidden = true;
  held = null;
  moving = null;
  seen.disconnect();
  expanded(false);
  if (refocus) refocusTerminal();
}

/** Palette and pane menu: under the focused pane's button, else at its top right. */
export function openImages() {
  const id = S.focused;
  if (!id) return;
  const cell = document.querySelector<HTMLElement>(`.cell[data-session="${CSS.escape(id)}"]`);
  const b = cell?.querySelector<HTMLElement>(".head-images");
  const at = (b ?? cell?.querySelector<HTMLElement>(".cell-head") ?? cell)?.getBoundingClientRect();
  if (at) openAt(at);
}

/** The pane header button: the newest image, on the focused pane and a hovered header. */
export function imagesButton(id: string): HTMLElement | null {
  // Just after an insert: that image, with a check, for 1.5 s.
  const done = insertShown(id);
  const newest = done ?? newestImage();
  if (!newest) return null;
  const b = button("head-btn head-images" + (done ? " done" : ""), "", () => {
    if (!lib.hidden) return closeImages();
    const at = b.getBoundingClientRect();
    if (id !== S.focused) focusPane(id);
    openAt(at);
  });
  b.setAttribute("aria-label", t("images.title"));
  b.setAttribute("aria-haspopup", "menu");
  b.setAttribute("aria-expanded", String(!lib.hidden));
  const mini = h("span", "head-mini");
  mini.dataset.path = newest;
  fillThumb(mini, newest);
  b.append(mini);
  if (done) {
    const check = h("span", "head-check");
    check.append(icon('<path d="M4 12.5l5 5 11-11"></path>'));
    b.append(check);
  }
  return b;
}

/** A thumbnail dragged out of the library: a ghost row under the pointer,
 *  the drop look on the pane under it, and Insert into that pane on release. */
function startDrag(e: PointerEvent, i: Image, th: HTMLElement) {
  if (e.button !== 0) return;
  e.preventDefault();
  const [x0, y0] = [e.clientX, e.clientY];
  let ghost: HTMLElement | null = null;
  const url = th.querySelector("img")?.src ?? null;
  const move = (m: PointerEvent) => {
    if (!ghost && Math.hypot(m.clientX - x0, m.clientY - y0) < 4) return;
    if (!ghost) {
      ghost = h("div", "lib-ghost");
      const mini = h("span", "head-mini");
      if (url) mini.append(Object.assign(h("img"), { src: url, alt: "" }));
      ghost.append(mini, shownName(i));
      document.body.appendChild(ghost);
    }
    ghost.style.left = `${m.clientX + 12}px`;
    ghost.style.top = `${m.clientY + 12}px`;
    showImageDrag(m.clientX, m.clientY, url);
  };
  const up = (u: PointerEvent) => {
    window.removeEventListener("pointermove", move, true);
    window.removeEventListener("pointerup", up, true);
    if (!ghost) return;
    ghost.remove();
    const id = endImageDrag(u.clientX, u.clientY);
    if (!id) return;
    if (id !== S.focused) focusPane(id);
    void insertInto(id, [i]);
  };
  window.addEventListener("pointermove", move, true);
  window.addEventListener("pointerup", up, true);
}
