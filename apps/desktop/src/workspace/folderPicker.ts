/**
 * The folder picker for a remote Host (`folders@1`). One dialog for every
 * remote action that needs a folder: the person browses the Host's disk and
 * never types a path. Places on the left, the current folder's folders on
 * the right, the highlighted folder's full path in the footer. On This Mac
 * the system folder dialog stays.
 *
 * Every path here is the Host's. It goes only into another Host request,
 * never to an opener or a file call on this machine.
 */
import { hostCan, hostInvoke } from "../app/hostCaps";
import { checkShownHost, hostName, reinstallShownHost } from "../app/hosts";
import type { FolderListing, FolderPlaces, HostError } from "../platform/types";
import { actionDialog, dialogActions, dialogFrame } from "../ui/dialogParts";
import { button, h } from "../ui/dom";
import { rune } from "../ui/runes";
import { showError } from "../ui/alerts";
import { S } from "../app/state";
import { crumbs, filterRows, folderName, listRows, newFolderName, parentPath, whenText, type PickRow } from "./folderPickerModel";
import { t } from "../i18n";

/** A place's words. Its label stays the id the code matches on. */
const placeLabel = (label: string) =>
  label === "Home" ? t("picker.home") : label === "Recent" ? t("picker.recent") : label === "Git repos" ? t("picker.repos") : label;

/** What the primary button does for a folder when it is not the plain pick. */
export interface PickVerdict {
  /** The primary button's label, such as "Initialize repository…". */
  label: string;
  /** One line under the list that says why. */
  note: string;
  /** Resolves true when the picker is done and closes. */
  run: () => Promise<boolean>;
}

export interface PickerUse {
  /** The primary button: "Add project", "Start here". */
  action: string;
  /** Checks the highlighted folder. `null`: the primary button picks it. */
  check?: (path: string, name: string) => Promise<PickVerdict | null>;
  /** The plain pick. A throw shows in the picker and keeps it open. */
  pick: (path: string) => Promise<void>;
  /** Opens on Git repos when the Host found any. Default: Home. */
  start?: "home" | "repos";
  /** Runs after the picker closes, picked or not. Default: the keys go back. */
  onClose?: (picked: boolean) => void;
}

/** The Host error's code, or "" for an untyped one. */
function code(e: unknown): string {
  return typeof e === "object" && e !== null && "code" in e ? String((e as HostError).code) : "";
}
function message(e: unknown): string {
  if (typeof e === "object" && e !== null && "message" in e) return String((e as HostError).message ?? "");
  return String(e);
}

/**
 * The picker cannot open: the Host has no `folders@1`, or its projects
 * service is not reachable. Says so, with the fix. Answers true when it did.
 */
export function pickerUnavailable(): boolean {
  const host = hostName();
  if (S.dataState === "unavailable") {
    showError(t("picker.noService", { host }), {
      label: t("picker.start"),
      run: async () => checkShownHost(),
    });
    return true;
  }
  if (!hostCan("folders")) {
    showError(t("picker.tooOld", { host }), {
      label: t("skiffAgent.updateSkiff"),
      run: async () => reinstallShownHost(),
    });
    return true;
  }
  return false;
}

/** Opens the picker on the Host's home folder. */
export function openFolderPicker(use: PickerUse): void {
  if (pickerUnavailable()) return use.onClose?.(false);
  const host = hostName();
  const back = document.activeElement as HTMLElement | null;
  const title = t("picker.title", { host });
  const { overlay, panel } = dialogFrame("confirm-panel fp-panel", "dialog", title);

  const crumbBar = h("div", "fp-crumbs");
  const filter = h("input", "fp-filter");
  filter.type = "text";
  filter.spellcheck = false;
  filter.placeholder = t("picker.filter");
  filter.setAttribute("aria-label", t("picker.filter"));
  const hiddenToggle = button("fp-hidden", t("picker.hidden"), () => {
    hidden = !hidden;
    hiddenToggle.setAttribute("aria-pressed", String(hidden));
    if (view.kind === "dir") void go(view.listing.real, { keep: true });
  });
  hiddenToggle.title = t("picker.hiddenTip");
  hiddenToggle.setAttribute("aria-pressed", "false");
  const bar = h("div", "fp-bar");
  bar.append(crumbBar, hiddenToggle, filter);

  const placesCol = h("div", "fp-places");
  const list = h("div", "fp-list");
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", t("picker.folders"));
  const main = h("div", "fp-main");
  main.append(placesCol, list);

  const note = h("div", "fp-note");
  note.setAttribute("role", "alert");
  const pathLine = h("div", "fp-path");
  const { foot, cancel, act } = dialogActions(use.action, "confirm-act go");
  const make = h("button", "confirm-cancel", t("picker.newFolder"));
  make.type = "button";
  foot.prepend(make);
  panel.append(h("div", "confirm-title", title), bar, main, note, pathLine, foot);
  overlay.appendChild(panel);
  document.body.appendChild(overlay);

  type View = { kind: "dir"; listing: FolderListing } | { kind: "place"; label: string; rows: PickRow[] };
  let view: View = { kind: "place", label: "", rows: [] };
  let hidden = false;
  let rows: PickRow[] = [];
  let lit = 0;
  let home: string | null = null;
  let open = true;
  let busy = false;
  let picked = false;
  /** The verdict for the highlighted folder, and the folder it is for. */
  let verdict: { path: string; v: Promise<PickVerdict | null> } | null = null;
  let checkTimer = 0;
  let filterTimer = 0;
  let seq = 0;
  /** The listing shown came filtered by the Host. */
  let hostFiltered = false;
  let known: FolderPlaces | null = null;
  let polls = 0;

  const close = () => {
    if (!open) return;
    open = false;
    clearTimeout(checkTimer);
    clearTimeout(filterTimer);
    overlay.remove();
    if (use.onClose) use.onClose(picked);
    else back?.focus?.();
  };

  /** The folder the primary button acts on: the lit row, else the folder shown. */
  const target = (): { path: string; name: string } | null => {
    const r = rows[lit];
    if (r) return { path: r.path, name: r.name };
    if (view.kind === "dir") return { path: view.listing.real, name: folderName(view.listing.real) };
    return null;
  };

  function renderCrumbs() {
    crumbBar.replaceChildren();
    const parts = view.kind === "dir" ? crumbs(view.listing.real, host) : [{ label: host, path: "/" }, { label: placeLabel(view.label), path: "" }];
    parts.forEach((c, i) => {
      if (i) crumbBar.append(h("span", "fp-sep", "›"));
      const b = button("fp-crumb", c.label, () => c.path && void go(c.path));
      b.tabIndex = -1;
      if (!c.path) b.disabled = true;
      crumbBar.append(b);
    });
  }

  function renderPlaces() {
    placesCol.replaceChildren();
    for (const p of places()) {
      const b = button("fp-place", placeLabel(p.label), () => void p.open());
      b.tabIndex = -1;
      if (p.label === (view.kind === "place" ? view.label : view.listing.real === home ? "Home" : "")) b.classList.add("on");
      placesCol.append(b);
    }
  }

  function renderRows() {
    const all = view.kind === "dir" ? listRows(view.listing.real, view.listing.entries) : view.rows;
    rows = filterRows(all, filter.value);
    lit = Math.min(lit, Math.max(0, rows.length - 1));
    list.replaceChildren();
    rows.forEach((r, i) => {
      const row = h("button", "fp-row");
      row.type = "button";
      row.tabIndex = -1;
      row.setAttribute("role", "option");
      row.classList.toggle("lit", i === lit);
      row.setAttribute("aria-selected", String(i === lit));
      const lead = h("span", "fp-lead");
      lead.append(rune(r.repo ? "code-git-branch" : "documents-folder-open", 12));
      row.append(lead, h("span", "fp-name", r.name));
      if (r.sub) row.append(h("span", "fp-sub", r.sub));
      if (r.repo) {
        row.append(h("span", "fp-branch", r.repo.branch ?? t("picker.detached")), h("span", "fp-when", whenText(r.repo.last_commit)));
      }
      row.addEventListener("click", () => setLit(i));
      row.addEventListener("dblclick", () => void enter(r));
      list.append(row);
    });
    if (!rows.length && !(view.kind === "place" && view.label === "Git repos" && !filter.value)) list.append(h("div", "fp-empty", t(view.kind === "dir" && !filter.value ? "picker.noFolders" : "picker.noMatch")));
    list.querySelector(".fp-row.lit")?.scrollIntoView({ block: "nearest" });
    make.disabled = view.kind !== "dir";
    refreshTarget();
  }

  function setLit(i: number) {
    if (!rows.length) return;
    lit = Math.max(0, Math.min(rows.length - 1, i));
    list.querySelectorAll(".fp-row").forEach((el, j) => {
      el.classList.toggle("lit", j === lit);
      el.setAttribute("aria-selected", String(j === lit));
    });
    list.querySelector(".fp-row.lit")?.scrollIntoView({ block: "nearest" });
    refreshTarget();
  }

  /** The footer path, and the primary button for that folder. */
  function refreshTarget() {
    const t = target();
    pathLine.textContent = t?.path ?? "";
    act.disabled = !t;
    clearTimeout(checkTimer);
    act.textContent = use.action;
    if (!busy) note.textContent = baseNote();
    verdict = null;
    if (!t || !use.check) return;
    const check = use.check;
    checkTimer = window.setTimeout(() => {
      const mine = { path: t.path, v: check(t.path, t.name).catch(() => null) };
      verdict = mine;
      void mine.v.then((v) => {
        if (verdict !== mine || !open || busy) return;
        act.textContent = v?.label ?? use.action;
        note.textContent = v?.note ?? baseNote();
      });
    }, 120);
  }

  const baseNote = () => (view.kind === "dir" ? truncatedNote() : placeNote(view.label));
  const truncatedNote = () =>
    view.kind === "dir" && view.listing.truncated ? t("picker.truncated", { host, count: 2000 }) : "";

  /** Shows the folder at `path`. A gone folder goes up one level. */
  async function go(path: string, o: { keep?: boolean; light?: string; query?: string } = {}): Promise<void> {
    const mine = ++seq;
    try {
      const listing = await hostInvoke<FolderListing>("folders", "folder_list", { path, hidden, filter: o.query ?? null });
      if (mine !== seq || !open) return;
      const was = rows[lit]?.path;
      view = { kind: "dir", listing };
      hostFiltered = !!o.query;
      if (!o.query) filter.value = o.keep ? filter.value : "";
      const all = listRows(listing.real, listing.entries);
      const want = o.light ?? (o.keep ? was : undefined);
      lit = Math.max(0, filterRows(all, filter.value).findIndex((r) => r.path === want));
      renderCrumbs();
      renderPlaces();
      renderRows();
    } catch (e) {
      if (mine !== seq || !open) return;
      const c = code(e);
      if (c === "not_found") {
        const up = parentPath(path);
        if (up && up !== path) await go(up);
        note.textContent = t("picker.gone", { host });
      } else if (c === "permission") {
        note.textContent = t("picker.denied", { host });
      } else {
        note.textContent = message(e);
      }
    }
  }

  /** Opens a row: its folder, at its real path. */
  async function enter(r: PickRow | undefined) {
    if (r) await go(r.path);
  }

  async function up() {
    if (view.kind !== "dir" || !view.listing.parent) return;
    await go(view.listing.parent, { light: view.listing.real });
  }

    /** Places: Home, Recent and Git repos. Recent and Git repos come from the Host. */
  function places(): { label: string; open: () => Promise<void> }[] {
    return [
      { label: "Home", open: async () => void (home && (await go(home))) },
      { label: "Recent", open: async () => showPlace("Recent") },
      { label: "Git repos", open: async () => showPlace("Git repos") },
    ];
  }

  function placeRows(label: string): PickRow[] {
    const repos = known?.repos ?? [];
    const row = (path: string): PickRow => {
      const r = repos.find((x) => x.path === path);
      return { name: folderName(path), path, repo: r ? { branch: r.branch, last_commit: r.last_commit } : null, link: false, sub: parentPath(path) ?? "" };
    };
    return label === "Recent" ? (known?.recent ?? []).map(row) : repos.map((r) => row(r.path));
  }

  /** The note a place shows when it has no rows, or while the scan runs. */
  function placeNote(label: string): string {
    if (label !== "Git repos") return "";
    if (known?.scanning) return t("picker.scanning", { host });
    return known && !known.repos.length ? t("picker.noRepos", { host }) : "";
  }

  function showPlace(label: string) {
    ++seq;
    const was = view.kind === "place" && view.label === label ? rows[lit]?.path : undefined;
    view = { kind: "place", label, rows: placeRows(label) };
    hostFiltered = false;
    filter.value = was ? filter.value : "";
    lit = Math.max(0, filterRows(view.rows, filter.value).findIndex((r) => r.path === was));
    renderCrumbs();
    renderPlaces();
    renderRows();
    note.textContent = placeNote(label);
  }

  /** Asks the Host for Places, and again while its scan runs. */
  async function loadPlaces() {
    try {
      known = await hostInvoke<FolderPlaces>("folders", "folder_places");
    } catch {
      return;
    }
    if (!open) return;
    if (view.kind === "place") showPlace(view.label);
    if (known.scanning && polls++ < 12) window.setTimeout(() => void loadPlaces(), 600);
  }

  async function primary() {
    const t = target();
    if (!t || busy) return;
    busy = act.disabled = true;
    const label = act.textContent ?? use.action;
    act.textContent = "…";
    try {
      const v = verdict?.path === t.path ? await verdict.v : use.check ? await use.check(t.path, t.name) : null;
      if (!open) return;
      let done: boolean;
      if (v) {
        busy = false;
        act.disabled = false;
        act.textContent = label;
        done = await v.run();
      } else {
        await use.pick(t.path);
        done = true;
      }
      if (done) {
        picked = true;
        close();
      }
    } catch (e) {
      note.textContent = message(e);
    } finally {
      if (open) {
        busy = act.disabled = false;
        if (act.textContent === "…") act.textContent = label;
        filter.focus();
      }
    }
  }

  function newFolder() {
    if (view.kind !== "dir" || busy) return;
    const parent = view.listing.real;
    const name = folderName(parent);
    const input = h("input", "prompt-input");
    input.type = "text";
    input.spellcheck = false;
    input.placeholder = t("picker.namePlaceholder");
    input.setAttribute("aria-label", t("picker.namePlaceholder"));
    let made: string | null = null;
    actionDialog({
      title: t("picker.newFolderTitle", { name }),
      content: [input],
      action: t("picker.create"),
      tone: "go",
      focus: input,
      submit: async () => {
        const n = newFolderName(input.value);
        try {
          made = await hostInvoke<string>("folders", "folder_create", { parent, name: n });
        } catch (e) {
          if (code(e) === "conflict") throw t("picker.exists", { name });
          if (code(e) === "invalid") throw t("picker.oneName");
          throw message(e);
        }
      },
      onClose: () => {
        filter.focus();
        if (made) void go(parent, { light: made });
      },
    });
  }

  cancel.addEventListener("click", () => !busy && close());
  act.addEventListener("click", () => void primary());
  make.addEventListener("click", newFolder);
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay && !busy) close();
  });
  filter.addEventListener("input", () => {
    lit = 0;
    renderRows();
    // A cut listing hides folders: then the Host filters the whole folder.
    clearTimeout(filterTimer);
    if (view.kind !== "dir" || !(view.listing.truncated || hostFiltered)) return;
    const real = view.listing.real;
    filterTimer = window.setTimeout(() => void go(real, { keep: true, query: filter.value }), 200);
  });
  overlay.addEventListener("keydown", (e) => {
    e.stopPropagation();
    const inFilter = document.activeElement === filter;
    const atStart = inFilter && filter.selectionStart === 0 && filter.selectionEnd === 0;
    const atEnd = inFilter && filter.selectionStart === filter.value.length;
    const onButton = document.activeElement instanceof HTMLButtonElement && foot.contains(document.activeElement);
    if (e.key === "Escape") {
      if (!busy) close();
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      void primary();
    } else if (e.key === "Enter") {
      if (onButton) return;
      void enter(rows[lit]);
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      setLit(lit + (e.key === "ArrowDown" ? 1 : -1));
      filter.focus();
    } else if (e.key === "ArrowRight" && (atEnd || !inFilter) && !onButton) {
      void enter(rows[lit]);
    } else if ((e.key === "ArrowLeft" && (atStart || !inFilter) && !onButton) || (e.key === "Backspace" && !filter.value && !onButton)) {
      void up();
    } else if (e.key === "Tab") {
      const stops = [filter, hiddenToggle, ...foot.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
      const i = stops.indexOf(document.activeElement as HTMLInputElement);
      stops[(i + (e.shiftKey ? -1 : 1) + stops.length) % stops.length]?.focus();
    } else {
      // Typing goes to the filter.
      if (!inFilter && e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) filter.focus();
      return;
    }
    e.preventDefault();
  });

  renderCrumbs();
  renderPlaces();
  renderRows();
  filter.focus();
  const places0 = loadPlaces();
  hostInvoke<string>("folders", "folder_home")
    .then(async (path) => {
      home = path;
      if (use.start === "repos") {
        await places0;
        if (known?.repos.length) return showPlace("Git repos");
      }
      return go(path);
    })
    .catch((e) => {
      note.textContent = message(e);
    });
}
