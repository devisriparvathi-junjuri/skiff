import { worktreeLines, type Line } from "./sidebarLines";
import { groupRenameInput } from "./rename";
/** The session tree and its focus, caret, and hover restoration. */

import { filledOf } from "./layoutSlots";
import { agentIcon, agentKind, launchIcon } from "../appearance/agentIcon";
import { ink } from "../appearance/appTheme";
import { appliedTheme, ownTheme, signatureColor } from "../appearance/themes";
import { glyph } from "./layoutIcon";
import { agentName, basename, branchName, scopeName, taskTitle } from "./model";
import { faceSig, skipperFace } from "./skipperFace";
import type { Group, Project, SessionInfo, Worktree } from "../platform/types";

import { changeCounts, changesBlock, changesSig, showsChanges } from "./changes";
import { filesBlock, filesSig, showsFiles } from "./files";
import { $, button, h } from "../ui/dom";
import { branchIcon, chevron, icon, plusIcon } from "../ui/icons";

import { fleetMenu, groupMenu, projectMenu, rowMenu, skiffAgentMenu, worktreeMenu } from "./menus";

import { launchMenu } from "../app/panels";

import { renameRow, renamingKey, startRename, startSessionRename } from "./rename";

import { findRow, itemKey, listItems } from "../app/keyboard";
import { clearSelection, keepRow, selectRange, toggleSelect, toggleSelectGroup } from "./selection";
import { startPreview } from "../terminal/peek";
import { stateIcon } from "../appearance/stateIcon";
import { collapsed, DEFAULT_ACCENT, removeErrors, removing, S, selectedWorktree, sessions } from "../app/state";
import { accent, awayPlaces, currentWorktree, place } from "../app/stateQueries";

import { dragGroup, dragSessions, groupHome, revealSession, selectWorktree, showGroup } from "./view";
import { addProject } from "../app/panels";
import { animateFold, growOpened } from "../ui/fold";
import { dragTree } from "./treeDrag";
import { bareWorktree, fleetKey, folded, hiddenSplit, isOpen, openToFocused, projectBadge, projectKey, projectsHidden, rollCounts, rollup, setProjectsHidden, toggleRow, treeDrag, treeSections, type Section } from "./tree";
import { render } from "../app/renderRequest";
import { projectsPlus, renderWorkspaceStatus, shows, workspaceOpen } from "./profile";
import { ledgerCounts, ledgerNeeds, ledgerWorkTitle } from "./ledger";
import { fleetScope, fleetSkipperOf, projectSkipperOf, projectWatch, openSkiffAgent, SKIFF_AGENT_KEY, skiffAgentChoice, skiffAgentFolder, skiffAgentName, skiffAgentSession, skiffWatch, skipperWord, syncSkiffAgent } from "../app/skiffAgent";
import { hostConnected, hostName, isRemote } from "../app/hosts";
import { step, tally } from "../diagnostics/stalls";
import { sentences, t, tn } from "../i18n";
import { renderSkippers, renderViewToggle, skippersOn } from "./skippersView";

/** 20px lines, flush against whatever follows. */
function sessionBlock(lines: Line[], color: string): HTMLElement {
  const block = h("div", "sessions-block");
  const n = lines.length;
  block.style.height = `calc(${n ? Math.max(40, n * 20) : 0} * var(--u))`;
  for (const l of lines) {
    if (l.group) block.appendChild(groupRow(l.group, l.hasPrevious, l.hasNext));
    else if (l.pointer) block.appendChild(pointerRow(l.pointer));
    else block.appendChild(sessionRow(l.session, color, l));
  }
  return block;
}

function sessionRow(s: SessionInfo, color: string, o: { branch?: boolean; in?: Group; groupRail?: boolean; where?: string }): HTMLElement {
  if (S.renamingSession === s.id) return renameRow(s, color);
  const cls = ["session-row"];
  if (o.in) cls.push("nested");
  if (o.groupRail) cls.push("group-rail");
  if (S.selection.includes(s.id)) cls.push("selected");
  const row = h("button", cls.join(" "));
  row.type = "button";
  row.dataset.session = s.id;
  if (o.in) row.dataset.inGroup = o.in.id;
  row.addEventListener("mousedown", (e) => {
    // dragSessions prevents the browser's default mousedown behavior to avoid
    // text selection, so focus the row explicitly before starting the drag.
    // Otherwise Delete stays in the terminal after a mouse selection.
    if (e.button === 0) row.focus();
    dragSessions(e, S.selection.length > 1 && S.selection.includes(s.id) ? [...S.selection] : [s.id]);
  });
  row.addEventListener("click", (e) => {
    // detail counts clicks across the re-render the first click causes.
    if (e.detail >= 2 && !e.shiftKey && !e.ctrlKey && !e.metaKey) startSessionRename(s.id);
    else if (e.shiftKey) selectRange(s.id);
    else if (e.ctrlKey || e.metaKey) toggleSelect(s.id);
    else {
      // A plain click opens and ends any selection. It never picks.
      clearSelection();
      revealSession(s.id);
      // The keys stay on the row, as on a picked group's, so Delete can end it.
      // Typing still reaches the terminal: see passToTerminal in keyboard.ts.
      requestAnimationFrame(() => keepRow(s.id, o.in?.id ?? null));
    }
  });
  row.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    rowMenu(s, e.clientX, e.clientY);
  });
  row.style.setProperty("--pc", color);
  if (o.in) row.style.setProperty("--gc", groupColor(o.in));
  row.append(agentIcon(s), h("span", "title", taskTitle(s)));
  if (o.branch) {
    const at = place(s);
    row.appendChild(h("span", "where mono", at ? branchName(at.worktree) : t("sidebar.otherWhere")));
  }
  if (o.where) row.appendChild(h("span", "where mono", o.where));
  const away = awayPlaces(s);
  if (away.length) {
    const names = away.map((a) => branchName(a.worktree)).join(", ");
    const tag = h("span", "where away mono", `→ ${names}`);
    row.appendChild(tag);
  }
  applyLook(row, s);
  return row;
}

/** What a session row shows of the session's state. A kept row takes a new one in place. */
const look = (s: SessionInfo) => JSON.stringify([s.state, s.unread, s.exit_code, s.agent_exit]);

/**
 * Writes the session's state into its row: the state class, the label, the
 * mark at the row's end and the bold title. State changes many times a
 * minute while agents work; this keeps the row, its hover and its icons.
 * A mark of the same kind stays, so a working skiff keeps rocking.
 */
function applyLook(row: HTMLElement, s: SessionInfo) {
  const l = look(s);
  if (row.dataset.look === l) return;
  const kept = row.dataset.look !== undefined;
  row.dataset.look = l;
  // State shows only when it matters: waiting stands out, working pulses, done fades.
  for (const st of STATES) if (st !== s.state) row.classList.remove(`st-${st}`);
  row.classList.add(`st-${s.state}`);
  row.setAttribute("aria-label", t("sidebar.rowLabel", { agent: agentName(s), title: taskTitle(s), state: t(`state.${s.state}`) }));
  const mark = stateIcon(s);
  row.classList.toggle("unread-row", s.state === "idle" && mark.classList.contains("unread"));
  // The mark is the row's last child.
  if (!kept) row.appendChild(mark);
  else if (row.lastElementChild!.className !== mark.className) row.lastElementChild!.replaceWith(mark);
}

const STATES: SessionInfo["state"][] = ["idle", "working", "waiting", "done"];

/**
 * The row's state mark. An idle row with an unread mark shows its title in
 * bold. A class does this, not `:has(.unread)`: WebKit and Chromium restyle
 * every row title on any change under a `:has()` rule.
 */
function unreadMark(row: HTMLElement, s: SessionInfo): HTMLElement {
  const mark = stateIcon(s);
  if (s.state === "idle" && mark.classList.contains("unread")) row.classList.add("unread-row");
  return mark;
}

/**
 * A session from another worktree that works in this one. Not a second
 * session: a click goes to the real one. The keys skip it.
 */
function pointerRow(s: SessionInfo): HTMLElement {
  const at = place(s);
  const from = at ? branchName(at.worktree) : basename(s.cwd);
  const row = h("div", "session-row pointer-row");
  row.append(agentIcon(s), h("span", "title", taskTitle(s)), h("span", "where mono", t("sidebar.from", { name: from })));
  row.addEventListener("click", () => {
    clearSelection();
    revealSession(s.id);
    requestAnimationFrame(() => keepRow(s.id));
  });
  return row;
}

/**
 * What the list says about the projects themselves, from the selected
 * Host's data plane: they load, they are out of reach and why, or there are
 * none. Null when the projects show and say it all.
 */
export function projectsNote(): HTMLElement | null {
  const remote = isRemote();
  const host = remote ? hostName() : "";
  // A remote Host says what it waits for: the connect, then its projects.
  if (S.dataState === "loading" || (remote && !hostConnected())) {
    if (S.projects.length) return null;
    return h("div", "note-row", remote ? t("sidebar.waitingFor", { host }) : t("sidebar.loadingProjects"));
  }
  if (S.dataState === "unavailable") {
    // Each cause says what is wrong and what Skiff does about it.
    const why = S.dataNote ?? (remote ? t("sidebar.cannotShowOn", { host }) : t("sidebar.cannotShow"));
    const text = S.projects.length ? sentences([why, t("sidebar.lastRead")]) : why;
    // A remote Host without its service is a setup, not a fault: no red.
    return remote ? h("div", "note-row", text) : errorRow(text);
  }
  return S.projects.length ? null : h("div", "note-row", remote ? t("sidebar.noProjectsOn", { host }) : t("sidebar.noProjects"));
}

function errorRow(text: string): HTMLElement {
  const row = h("div", "error-row", text);
  row.setAttribute("role", "alert");
  return row;
}

let lastProjects: Project[] | null = null;
let lastGroups: Group[] | null = null;
let listGen = 0;
let lastSig = "";

/** What each kept block draws, by its key: the agent row, a section head, a project, Other. */
let blockSigs = new Map<string, string>();
/** The blocks the last render drew, by the same keys, and each section's rows. */
let blocks = new Map<string, HTMLElement>();
const sectionKids = new Map<string, HTMLElement>();

const AGENT_BLOCK = "agent";
const OTHER_BLOCK = "other";
const projectBlockKey = (name: string) => `project:${name}`;

/**
 * Everything the tree draws, as one string, and the part of it each block
 * draws. Renders run while agents work. A rebuilt row loses its hover,
 * restarts its icons and costs a style pass, so a block is rebuilt only when
 * what it shows changes. Expanding one project rebuilds that project.
 */
function sidebarSig(): { all: string; parts: Map<string, string>; looks: Map<string, string[]> } {
  // Rows close over project, worktree and group objects. A new list brings new
  // objects, and some callers compare them by identity: the rows are rebuilt.
  if (S.projects !== lastProjects || S.groups !== lastGroups) {
    lastProjects = S.projects;
    lastGroups = S.groups;
    listGen++;
  }
  // The session rows each project draws: its own, its visitors' and its groups' members.
  const under = new Map<string, unknown[]>();
  const add = (name: string | null, row: unknown) => {
    const k = name === null ? OTHER_BLOCK : projectBlockKey(name);
    (under.get(k) ?? under.set(k, []).get(k)!).push(row);
  };
  // A state change alone rebuilds no block: applyLook writes it into the kept rows.
  // A folded row counts the states under it, so its block keeps them.
  const looksUnder = new Map<string, string[]>();
  const addLook = (name: string | null, l: string) => {
    const k = name === null ? OTHER_BLOCK : projectBlockKey(name);
    (looksUnder.get(k) ?? looksUnder.set(k, []).get(k)!).push(l);
  };
  const rowOf = new Map<string, unknown>();
  const rows = [...sessions.values()].map((s) => {
    const t = ownTheme(s);
    const at = place(s);
    const away = awayPlaces(s);
    const row = [
      s.id, s.started_at,
      taskTitle(s), agentName(s), agentKind(s), t ? ink(signatureColor(t)) : null,
      at?.worktree.path ?? null, away.map((a) => a.worktree.path),
    ];
    rowOf.set(s.id, row);
    add(at?.project.name ?? null, row);
    addLook(at?.project.name ?? null, look(s));
    for (const a of away) add(a.project.name, row);
    return [row, look(s)];
  });
  const groups = S.groups.map((g) => {
    const lead = sessions.get(g.focus ?? filledOf(g.layout)[0]);
    const row = [g.id, g.name, g.layout, g.cwd, groupColor(g), accent(lead ? place(lead)?.project : null)];
    const home = groupHome(g);
    const owner = home ? S.projects.find((p) => p.worktrees.includes(home))?.name ?? null : null;
    add(owner, [row, filledOf(g.layout).map((id) => rowOf.get(id) ?? null)]);
    return row;
  });
  const projects = S.projects.map((p) => [
    p.name, p.short, p.icon, p.fleet ?? null, p.error, p.default_branch, accent(p), selectedWorktree.get(p.name),
    p.worktrees.map((w) => [
      w.path, w.branch, w.is_main, w.locked, w.prunable, collapsed.has(w.path), removing.has(w.path), removeErrors.get(w.path),
      showsChanges(p) ? changesSig(w) : null, showsFiles(p) ? filesSig(w) : null,
    ]),
  ]);
  // The agent icons are inked against the applied app theme: a new theme re-inks them.
  // The watcher state changes with no session event when the grace ends.
  const sa = skiffAgentSession();
  const agent = skiffAgentFolder() ? [skiffAgentName(), sa?.id ?? null, skiffAgentChoice()?.id ?? null, sa ? skiffWatch(sa) : null, sa?.watch?.follows ?? null, ledgerNeeds(), sa ? [rowOf.get(sa.id), look(sa)] : null, faceSig(null)] : null;
  const base = [listGen, appliedTheme(), S.selection, S.renamingSession, S.renaming?.id ?? null, sa?.id ?? null];
  const parts = new Map<string, string>();
  parts.set(AGENT_BLOCK, JSON.stringify([base, agent]));
  parts.set(OTHER_BLOCK, JSON.stringify([base, under.get(OTHER_BLOCK) ?? null]));
  S.projects.forEach((p, i) => {
    const k = projectBlockKey(p.name);
    const open = isOpen(projectKey(p.name));
    // An open project's Skipper row is rebuilt alone (syncSkipperRow).
    const skipper = open ? !!projectSkipperOf(p.name) : skipperSig(p.name);
    parts.set(k, JSON.stringify([base, projects[i], open, p.name === S.selectedProject, under.get(k) ?? null, skipper, open ? null : looksUnder.get(k) ?? null]));
  });
  const all = JSON.stringify([
    listGen, appliedTheme(), S.selectedProject, folded.keys(), S.projects.map(shows), hiddenSig(),
    S.selection, S.renamingSession, S.renaming?.id ?? null, rows, groups, projects, agent,
    S.dataState, S.dataNote,
  ]);
  return { all, parts, looks: looksUnder };
}

/** What each fleet that hides its projects shows: its head's count and the rows still under it. */
function hiddenSig() {
  return treeSections().flatMap((sec) => {
    if (!sec.fleet || !projectsHidden(sec.fleet)) return [];
    const { shown, hidden } = hiddenSplit(sec.projects);
    return [[sec.fleet, hidden, shown.map((x) => x.id)]];
  });
}

/**
 * Puts `nodes` in `parent` in order. Kept nodes stay where they are: a node
 * taken out and put back loses its style and restarts its animations.
 * `before` runs first when anything moves.
 */
function setChildren(parent: HTMLElement, nodes: HTMLElement[], before: () => void) {
  if (parent.children.length === nodes.length && nodes.every((n, i) => parent.children[i] === n)) return;
  before();
  const want = new Set<Node>(nodes);
  for (const c of [...parent.children]) if (!want.has(c)) c.remove();
  nodes.forEach((n, i) => {
    if (parent.children[i] !== n) parent.insertBefore(n, parent.children[i] ?? null);
  });
}

export function renderSidebar() {
  const side = $<HTMLElement>("sidebar-scroll");
  if (treeDrag.active) return;
  renderWorkspaceStatus();
  syncSkiffAgent();
  renderViewToggle();
  if (skippersOn()) {
    // The tree draws whole again when it comes back.
    lastSig = "";
    const active = document.activeElement as HTMLElement | null;
    const key = active?.closest("#sidebar-scroll") ? active.dataset.session ?? active.dataset.key : undefined;
    renderSkippers(side);
    restoreFocus(side, key, null, []);
    return;
  }
  openToFocused();
  const sig = step("render.sidebar.sig", sidebarSig);
  if (lastSig === sig.all) return tally("sidebar.same");
  lastSig = sig.all;
  tally("sidebar.drawn");
  // Keep focus and the selected range in an inline control across re-renders,
  // so a render while a rename is open does not undo its select-all.
  const active = document.activeElement as HTMLElement | null;
  // The sidebar is rebuilt whenever sessions update. Keep focus on any list
  // row as well as inline rename fields, otherwise a selected session loses
  // its keyboard shortcuts after the next render.
  const activeKey = active?.dataset?.key ?? active?.dataset?.session ?? active?.dataset?.wt ?? active?.dataset?.group;
  const caret: Caret | null = active instanceof HTMLInputElement && active.selectionStart !== null
    ? [active.selectionStart, active.selectionEnd ?? active.selectionStart, active.selectionDirection ?? "none"]
    : null;
  // The rows after the focused one, then the rows before it, nearest first. If the
  // focused row goes (its session ended), the cursor moves to the next one left.
  const items = listItems();
  const at = active ? items.indexOf(active) : -1;
  const near = at < 0 ? [] : [...items.slice(at + 1), ...items.slice(0, at).reverse()].map(itemKey);
  // Where the view is, read before the first row comes or goes. A state
  // change alone moves no row: no read, and no layout forced for it.
  let anchor: ReturnType<typeof anchorOf> | undefined;
  const takeAnchor = () => {
    if (anchor === undefined) anchor = anchorOf(side, items);
  };

  // The block drawn last time when what it shows is the same, else a new one.
  const next = new Map<string, HTMLElement>();
  const sigs = new Map<string, string>();
  const keep = (key: string, sig: string, build: () => HTMLElement) => {
    const old = blocks.get(key);
    const el = old && blockSigs.get(key) === sig ? old : (tally("sidebar.block"), takeAnchor(), build());
    next.set(key, el);
    sigs.set(key, sig);
    return el;
  };
  const nodes: HTMLElement[] = [];
  const kidsNow = new Set<string>();
  if (skiffAgentFolder()) nodes.push(keep(AGENT_BLOCK, sig.parts.get(AGENT_BLOCK)!, skiffAgentRow));
  for (const sec of treeSections()) {
    // A fleet head holds its projects for drag and menu. Folded, it counts
    // what they wait on, work on and left unread.
    const fleetSkipper = sec.fleet ? fleetSkipperOf(sec.fleet) : null;
    // A fleet that hides its projects keeps its Skipper and the sessions that need a look.
    const split = sec.fleet && projectsHidden(sec.fleet) ? hiddenSplit(sec.projects) : null;
    // With none of them, the head is alone: no chevron and no fold.
    const alone = !!split && !fleetSkipper && !split.shown.length;
    const open = !sec.fleet || (!alone && isOpen(fleetKey(sec.fleet)));
    const headSig = JSON.stringify([
      listGen, sec.fleet, open, workspaceOpen(), sec.projects.map((p) => p.name), split?.hidden ?? null, alone,
      open ? null : sec.projects.map((p) => [sig.parts.get(projectBlockKey(p.name)), sig.looks.get(projectBlockKey(p.name)) ?? null]),
      open || !sec.fleet ? null : [fleetSkipperOf(sec.fleet)?.state ?? null, fleetSkipperOf(sec.fleet)?.unread ?? false],
    ]);
    nodes.push(keep(`head:${sec.fleet ?? ""}`, headSig, () => sectionHead(sec, split?.hidden ?? null, alone)));
    if (!open) continue;
    const sk = sec.fleet ?? "";
    kidsNow.add(sk);
    let kids = sectionKids.get(sk);
    if (!kids) {
      kids = h("div", "tree-kids");
      if (sec.fleet) kids.dataset.kids = fleetKey(sec.fleet);
      sectionKids.set(sk, kids);
    }
    // A fleet's Skipper is the first row under its head.
    const skipperRow = sec.fleet && fleetSkipper
      ? [keep(`skipper:${sec.fleet}`, JSON.stringify([listGen, skipperSig(fleetScope(sec.fleet))]), () => projectSkipperRow(fleetScope(sec.fleet!), fleetSkipper))]
      : [];
    const shownSig = split ? JSON.stringify([listGen, appliedTheme(), S.selection, S.renamingSession, split.shown.map((x) => [x.id, x.state, x.unread, x.exit_code, x.agent_exit, taskTitle(x), agentName(x), agentKind(x), place(x)?.project.name])]) : "";
    setChildren(kids, [...skipperRow, ...(split
      ? split.shown.length ? [keep(`shown:${sec.fleet}`, shownSig, () => shownBlock(split.shown))] : []
      : sec.projects.map((p) => {
        const k = projectBlockKey(p.name);
        return syncSkipperRow(keep(k, sig.parts.get(k)!, () => projectBlock(p)), p);
      }))], takeAnchor);
    nodes.push(kids);
  }
  // A folded fleet's rows start over when it opens: the fold left them shrunk.
  for (const k of [...sectionKids.keys()]) if (!kidsNow.has(k)) sectionKids.delete(k);
  const note = projectsNote();
  if (note) nodes.push(note);
  else if (S.projects.length && !S.projects.some(shows)) nodes.push(h("div", "note-row", t("sidebar.noProjectsHere")));
  // Sessions whose folder is in no project.
  if (worktreeLines(null).length) {
    nodes.push(keep(`${OTHER_BLOCK}-head`, "", otherHead));
    nodes.push(keep(OTHER_BLOCK, sig.parts.get(OTHER_BLOCK)!, () => sessionBlock(worktreeLines(null), DEFAULT_ACCENT)));
  }
  setChildren(side, nodes, takeAnchor);
  // Rows in kept blocks take the new state of their session.
  for (const row of side.querySelectorAll<HTMLElement>(".session-row[data-look]")) {
    const s = sessions.get(row.dataset.session!);
    if (s && row.dataset.look !== look(s)) {
      tally("sidebar.row");
      applyLook(row, s);
    }
  }
  blocks = next;
  blockSigs = sigs;
  if (anchor) keepAnchor(side, anchor);
  growOpened();
  restoreFocus(side, activeKey, caret, near);
}

/**
 * The first row in view and how far it sits from the list's top. WebKit has
 * no scroll anchoring: a row that comes or grows above the view would push
 * the rows the person reads down. At the top there is nothing to keep.
 */
function anchorOf(side: HTMLElement, items: HTMLElement[]): [key: string, dy: number] | null {
  if (!side.scrollTop) return null;
  const top = side.getBoundingClientRect().top;
  const el = items.find((x) => x.getBoundingClientRect().bottom > top);
  return el ? [itemKey(el), el.getBoundingClientRect().top - top] : null;
}

function keepAnchor(side: HTMLElement, anchor: [key: string, dy: number] | null) {
  if (!anchor) return;
  const el = listItems().find((x) => itemKey(x) === anchor[0]);
  if (!el) return;
  const moved = el.getBoundingClientRect().top - side.getBoundingClientRect().top - anchor[1];
  if (Math.abs(moved) >= 1) side.scrollTop += moved;
}

/** The head over sessions whose folder is in no project. */
function otherHead(): HTMLElement {
  const head = h("div", "tree-head");
  head.append(h("span", "tree-head-name", t("sidebar.other")));
  return head;
}

/**
 * The Skiff agent: one fixed row above the projects, with its name from
 * Settings, its agent's glyph and title, and its state mark. Its session is
 * left out of the tree below. With no session, a click or Enter starts one.
 */
function skiffAgentRow(): HTMLElement {
  const s = skiffAgentSession();
  const name = skiffAgentName();
  const row = h("button", "session-row skiff-agent-row" + (s ? ` st-${s.state}` : "") + (s && S.selection.includes(s.id) ? " selected" : ""));
  row.type = "button";
  row.style.setProperty("--pc", DEFAULT_ACCENT);
  if (!s) {
    const a = skiffAgentChoice();
    row.dataset.key = SKIFF_AGENT_KEY;
    row.setAttribute("aria-label", t("sidebar.agentOff", { name }));
    row.append(skipperFace(null, 24), launchIcon(a), h("span", "skiff-agent-name", name), h("span", "title", a ? t("sidebar.agentStart", { agent: a.id }) : t("sidebar.noAgentOn")));
    row.addEventListener("click", () => {
      // The row is current before the start: newSession shows the pane only if
      // the current row has not moved while the agent starts.
      keepRow(SKIFF_AGENT_KEY);
      openSkiffAgent();
    });
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      skiffAgentMenu(null, e.clientX, e.clientY);
    });
    return row;
  }
  row.dataset.session = s.id;
  // The agent icon shows the watcher: teal while it runs, and slowly turning
  // while the agent is idle and the watcher follows a pane with an open ledger
  // task, so the turn means "watching work". An older watcher does not say
  // what it follows: still. Red while none runs or the last line failed.
  const watch = skiffWatch(s);
  if (watch.state === "runs") row.classList.add("watch-on");
  if (watch.state === "runs" && s.state === "idle" && (s.watch?.follows?.length ?? 0) > 0) {
    row.classList.add("watch-turn");
    // The row is rebuilt often. Start the turn at the elapsed phase, not at its first frame.
    row.style.setProperty("--turn-delay", `${-(performance.now() % 8000)}ms`);
  }
  if (watch.state === "missing" || watch.state === "failed") row.classList.add("watch-error");
  if (watch.text) row.title = watch.text;
  const vars = { name, title: s.title ?? agentName(s), state: t(`state.${s.state}`), watch: watch.text };
  row.setAttribute("aria-label", watch.text ? t("sidebar.agentLabelWatch", vars) : t("sidebar.agentLabel", vars));
  // Its face, 24 px and centered on the row, then the agent icon with the watcher, and the skiff under way while it works.
  row.append(skipperFace(null, 24), agentIcon(s), ...underWay(s), h("span", "skiff-agent-name", name), h("span", "title", s.title?.replace(/^[^\p{L}\p{N}]+/u, "") ?? ""));
  // One amber count, only while ledger tasks need the person.
  const needs = ledgerNeeds();
  if (needs) {
    const c = h("span", "ledger-count", String(needs));
    c.title = tn("ledger.tasksNeedYou", needs);
    row.append(c);
  }
  row.append(unreadMark(row, s));
  row.addEventListener("mousedown", (e) => {
    if (e.button === 0) row.focus();
  });
  row.addEventListener("click", () => {
    clearSelection();
    openSkiffAgent();
  });
  row.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    rowMenu(s, e.clientX, e.clientY);
  });
  return row;
}

/** A Skipper row's skiff under way, the tree's working mark, while it works. */
const underWay = (s: SessionInfo) => (s.state === "working" ? [stateIcon(s)] : []);

/** What a project's or a fleet's Skipper row shows, for the block's signature. */
function skipperSig(scope: string) {
  const s = projectSkipperOf(scope);
  if (!s) return null;
  return [s.id, s.state, S.selection.includes(s.id), ledgerCounts(scope), ledgerWorkTitle(scope), s.watch ?? null, s.title ?? null, s.unread ?? false, faceSig(scope)];
}

/**
 * An open project's Skipper row changes with its state, its watcher and its
 * Ledger counts. A kept block rebuilds that row alone.
 */
function syncSkipperRow(block: HTMLElement, p: Project): HTMLElement {
  const s = projectSkipperOf(p.name);
  const old = block.querySelector<HTMLElement>(":scope > .tree-kids > .project-skipper-row");
  if (!s || !old) return block;
  const sig = JSON.stringify(skipperSig(p.name));
  if (block.dataset.skipper === sig) return block;
  block.dataset.skipper = sig;
  tally("sidebar.row");
  old.replaceWith(projectSkipperRow(p.name, s));
  return block;
}

/**
 * A project or fleet Skipper: the first row of its project block, or the
 * first row under its fleet head. Its agent icon, "Skipper", the title of
 * its work, and the amber and blue counts of the tasks it owns. Its session
 * is left out of the tree below.
 */
function projectSkipperRow(scope: string, s: SessionInfo): HTMLElement {
  const name = skipperWord();
  const row = h("button", "session-row project-skipper-row" + ` st-${s.state}` + (S.selection.includes(s.id) ? " selected" : ""));
  row.type = "button";
  row.dataset.session = s.id;
  // Its own watcher, as on the Skiff agent's row: teal while it runs, turning
  // while the Skipper is idle and the watcher follows a pane with an open
  // task of this project, red when its last line failed.
  const watch = projectWatch(s);
  if (watch.state === "runs") row.classList.add("watch-on");
  if (watch.state === "runs" && s.state === "idle" && (s.watch?.follows?.length ?? 0) > 0) {
    row.classList.add("watch-turn");
    row.style.setProperty("--turn-delay", `${-(performance.now() % 8000)}ms`);
  }
  if (watch.state === "failed") row.classList.add("watch-error");
  if (watch.text) row.title = watch.text;
  const title = ledgerWorkTitle(scope) ?? s.title?.replace(/^[^\p{L}\p{N}]+/u, "") ?? "";
  const { need, working } = ledgerCounts(scope);
  const vars = { scope: scopeName(scope), skipper: name, title: title || t(`state.${s.state}`), watch: watch.text };
  row.setAttribute("aria-label", watch.text ? t("sidebar.skipperLabelWatch", vars) : t("sidebar.skipperLabel", vars));
  row.append(h("span", "chev-space"), skipperFace(scope, 20), agentIcon(s), ...underWay(s), h("span", "skiff-agent-name", name), h("span", "title", title));
  const roll = rollCounts(need, working);
  if (roll) row.append(roll);
  row.append(unreadMark(row, s));
  row.addEventListener("mousedown", (e) => {
    if (e.button === 0) row.focus();
  });
  row.addEventListener("click", () => {
    clearSelection();
    revealSession(s.id);
    requestAnimationFrame(() => keepRow(s.id, null));
  });
  row.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    rowMenu(s, e.clientX, e.clientY);
  });
  return row;
}

/**
 * The sessions still under a fleet that hides its projects: each one that
 * waits, and the one on screen. Each row names its project.
 */
function shownBlock(shown: SessionInfo[]): HTMLElement {
  const block = h("div", "sessions-block fleet-shown");
  block.style.height = `calc(${shown.length * 20} * var(--u))`;
  for (const s of shown) {
    const p = place(s)!.project;
    block.appendChild(sessionRow(s, accent(p), { where: p.name }));
  }
  return block;
}

/**
 * A section head: "Projects" with its +, or a fleet that folds and opens.
 * A fleet that hides its projects counts its hidden sessions (`hidden`); a
 * click on the count shows the projects. `alone`: nothing under the head.
 */
function sectionHead(sec: Section, hidden: number | null = null, alone = false): HTMLElement {
  if (!sec.fleet) {
    const head = h("div", "tree-head");
    head.append(h("span", "tree-head-name", t("sidebar.projects")));
    // In a workspace, the + puts projects and fleets in it, or adds a project to Skiff.
    const add = button("tree-head-add", "", () => projectsPlus(add, () => void addProject.open()));
    add.title = workspaceOpen() ? t("sidebar.addToWorkspace") : t("sidebar.addProject");
    add.setAttribute("aria-label", add.title);
    if (workspaceOpen()) add.setAttribute("aria-haspopup", "menu");
    add.appendChild(plusIcon());
    head.appendChild(add);
    return head;
  }
  const name = sec.fleet;
  const key = fleetKey(name);
  const open = !alone && isOpen(key);
  const head = h("div", "tree-head fleet-head" + (alone ? " alone" : ""));
  const pick = button("fleet-pick", "", () => {
    if (!alone) toggleRow(key, render);
  });
  pick.dataset.key = key;
  if (!alone) {
    pick.dataset.fold = key;
    pick.setAttribute("aria-expanded", String(open));
  }
  pick.append(alone ? h("span", "chev-space") : chevron(open), h("span", "tree-head-name", name));
  const add = button("tree-head-add", "", () => void addProject.open(name));
  add.title = t("sidebar.addProjectTo", { name });
  add.setAttribute("aria-label", add.title);
  add.appendChild(plusIcon());
  // The ⋯ opens the fleet's menu, as on a project row: the same items as a right-click.
  const more = button("tree-head-add tree-head-more", "", () => {
    const r = more.getBoundingClientRect();
    fleetMenu({ name, projects: sec.projects }, r.right, r.top);
  });
  more.setAttribute("aria-label", t("sidebar.menu", { name }));
  more.setAttribute("aria-haspopup", "menu");
  more.appendChild(icon('<circle cx="5" cy="12" r="1"></circle><circle cx="12" cy="12" r="1"></circle><circle cx="19" cy="12" r="1"></circle>'));
  head.append(pick);
  if (hidden !== null) {
    const count = button("fleet-hidden", `· ${hidden}`, () => {
      setProjectsHidden(name, false);
      render();
    });
    count.setAttribute("aria-label", tn("sidebar.showHidden", hidden));
    head.append(count);
  }
  head.append(more, add);
  const roll = open || alone ? null : rollup(sec.projects, name);
  if (roll) pick.appendChild(roll);
  head.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    fleetMenu({ name, projects: sec.projects }, e.clientX, e.clientY);
  });
  head.addEventListener("mousedown", (e) => dragTree(e, head, sec.projects, name));
  return head;
}

/** A project: one 40px row, then its worktrees, groups and sessions while it is open. */
function projectBlock(p: Project): HTMLElement {
  const color = accent(p);
  const key = projectKey(p.name);
  const open = isOpen(key);
  const bare = bareWorktree(p);
  // Nothing under the project: no chevron, no fold, no hover.
  // changesBlock reads the worktree's changes the first time, so a project folded since the start still learns of them.
  const changes = bare && showsChanges(p) ? changesBlock(bare) : null;
  const skipper = projectSkipperOf(p.name);
  const empty = !!bare && !p.error && !worktreeLines(bare).length && !changes && !showsFiles(p) && !skipper;
  const block = h("div", "tree-project" + (p.name === S.selectedProject ? " selected" : ""));
  block.style.setProperty("--pc", color);

  const head = h("div", "project-row" + (empty ? " empty" : ""));
  head.dataset.project = p.name;
  head.addEventListener("mousedown", (e) => dragTree(e, block, [p], null));
  const pick = h("button", "project-pick");
  pick.type = "button";
  pick.dataset.key = pick.dataset.fold = key;
  pick.tabIndex = -1;
  if (!empty) pick.setAttribute("aria-expanded", String(open));
  const fold = h("span", "tree-fold");
  if (!empty) fold.appendChild(chevron(open));
  pick.append(fold, projectBadge(p), h("span", "project-name", p.name));
  if (p.error) pick.appendChild(h("span", "project-meta", t("sidebar.error")));
  const roll = open ? null : rollup([p]);
  if (roll) pick.appendChild(roll);
  // A click opens or folds the project, as on a fleet head. Its sessions open from their rows.
  if (!empty) pick.addEventListener("click", () => toggleRow(key, render));
  head.appendChild(pick);
  // One button: the ⋯ menu starts sessions too. A bare worktree's launch menu hangs off it.
  const more = button("wt-plus", "", () => {
    const r = more.getBoundingClientRect();
    projectMenu(p, r.right, r.top);
  });
  if (bare) more.dataset.wt = bare.path;
  more.setAttribute("aria-label", t("sidebar.menu", { name: p.name }));
  more.setAttribute("aria-haspopup", "menu");
  more.appendChild(icon('<circle cx="5" cy="12" r="1"></circle><circle cx="12" cy="12" r="1"></circle><circle cx="19" cy="12" r="1"></circle>'));
  head.appendChild(more);
  head.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    projectMenu(p, e.clientX, e.clientY);
  });
  block.appendChild(head);
  if (!open || empty) return block;

  const kids = h("div", "tree-kids");
  kids.dataset.kids = key;
  block.appendChild(kids);
  if (skipper) {
    kids.appendChild(projectSkipperRow(p.name, skipper));
    block.dataset.skipper = JSON.stringify(skipperSig(p.name));
  }
  if (p.error) kids.appendChild(errorRow(p.error));
  else if (p.worktrees.length === 0) kids.appendChild(h("div", "note-row", t("sidebar.noWorktrees")));
  else if (bare) {
    const body = h("div", "wt-body tree-bare");
    body.appendChild(sessionBlock(worktreeLines(bare), color));
    if (changes) body.appendChild(changes);
    if (showsFiles(p)) body.appendChild(filesBlock(bare));
    kids.appendChild(body);
  } else {
    const sel = selectedWorktree.get(p.name);
    for (const w of p.worktrees) kids.appendChild(worktreeBlock(p, w, w.path === sel, color));
  }
  return block;
}

type Caret = [start: number, end: number, dir: "forward" | "backward" | "none"];

/** Where the pointer last was over the list, or null when it left. */
let pointer: [number, number] | null = null;
const HOVERABLE = ".session-row, .wt-pick, .wt-plus, .wt-count, .group-pick, .file-row";
{
  const side = $<HTMLElement>("sidebar-scroll");
  side.addEventListener("mousemove", (e) => (pointer = [e.clientX, e.clientY]));
  side.addEventListener("mouseleave", () => (pointer = null));
  side.addEventListener("mouseout", (e) => (e.target as HTMLElement).closest?.(".hover")?.classList.remove("hover"));
}

/**
 * A rebuilt row has no :hover until the pointer moves, so the row under a
 * still pointer flashes on every render. The class stands in until it moves.
 */
function restoreHover() {
  if (!pointer) return;
  document.elementFromPoint(...pointer)?.closest(HOVERABLE)?.classList.add("hover");
}

function restoreFocus(side: HTMLElement, activeKey: string | undefined, caret: Caret | null, near: string[]) {
  restoreHover();
  // The row whose rename begins is now its name field, which takes the keys. It
  // has not gone: the rows near it must not become the current one.
  if (activeKey && activeKey === renamingKey()) return;
  if (activeKey) {
    const first = [...side.querySelectorAll<HTMLElement>("[data-key], [data-session], [data-wt], [data-group]")]
      .find((el) => (el.dataset.key ?? el.dataset.session ?? el.dataset.wt ?? el.dataset.group) === activeKey);
    // A Skipper in a group has two rows: the keys go back to the one they were on.
    const again = first?.dataset.session ? findRow(listItems(), activeKey) ?? first : first;
    const disabled = again instanceof HTMLButtonElement || again instanceof HTMLInputElement || again instanceof HTMLSelectElement
      ? again.disabled
      : false;
    if (again && !disabled) {
      // The list stays where the person scrolled it, away from the row or not.
      again.focus({ preventScroll: true });
      if (caret !== null && again instanceof HTMLInputElement) again.setSelectionRange(...caret);
    } else if (!again) {
      const keys = new Set(listItems().map(itemKey));
      const next = near.find((k) => keys.has(k));
      if (next) {
        keepRow(next);
        // The row that takes the place of the current one shows its preview, or the panes.
        startPreview(next, false);
      }
    }
  }
}

/** One color per group, by creation order, so a group keeps its color. */
const GROUP_COLORS = ["#9ec1ff", "#f28fd0", "#7ee0cb", "#e0c07e", "#ff9e7a", "#c3e88d"];

const groupColor = (g: Group) => ink(GROUP_COLORS[Math.max(0, S.groups.indexOf(g)) % GROUP_COLORS.length]);

function groupRow(g: Group, hasPrevious = false, hasNext = false): HTMLElement {
  const ids = filledOf(g.layout).filter((id) => sessions.has(id));
  const row = h("div", "group-row" + (hasPrevious ? " has-previous" : "") + (hasNext ? " has-next" : "") );
  row.dataset.group = g.id;
  row.style.setProperty("--gc", groupColor(g));
  const lead = sessions.get(g.focus ?? ids[0]);
  row.style.setProperty("--pc", accent(lead ? place(lead)?.project : null));
  if (S.renaming?.id === g.id) {
    row.classList.add("renaming");
    const input = groupRenameInput(g);
    row.append(glyph(g.layout), input);
    return row;
  }
  const pick = h("button", "group-pick");
  pick.dataset.group = g.id;
  pick.type = "button";
  pick.addEventListener("mousedown", (e) => {
    // The drag stops the default focus; the row still takes the keys.
    if (e.button === 0) pick.focus();
    dragGroup(e, g);
  });
  pick.append(glyph(g.layout), h("span", "group-name", g.name));
  pick.addEventListener("click", (e) => {
    if (e.ctrlKey || e.metaKey) return toggleSelectGroup(g);
    // detail counts clicks across the re-render the first click causes.
    if (e.detail >= 2) startRename(g);
    else if (g.id !== S.activeGroup || g.id !== S.groupPicked) showGroup(g.id, true);
  });
  pick.addEventListener("dblclick", () => startRename(g));
  pick.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    groupMenu(g, e.clientX, e.clientY);
  });
  row.appendChild(pick);
  return row;
}

function worktreeBlock(p: Project, w: Worktree, selected: boolean, color: string): HTMLElement {
  const block = h("div", "wt" + (selected ? " selected" : ""));
  block.style.setProperty("--pc", color);

  const lines = worktreeLines(w);
  // changesBlock reads the worktree's changes the first time, folded or not.
  const changes = showsChanges(p) ? changesBlock(w) : null;
  // Nothing under the worktree: no chevron and no fold. A click still selects it.
  const empty = !lines.length && !changes && !showsFiles(p);
  const open = !empty && !collapsed.has(w.path);

  const row = h("div", "wt-row");
  const pick = button("wt-pick", "", () =>
    empty
      ? selectWorktree(p, w)
      : animateFold(w.path, !open, () => {
          if (open) collapsed.add(w.path);
          else collapsed.delete(w.path);
          selectWorktree(p, w);
        }),
  );
  pick.dataset.wt = pick.dataset.fold = w.path;
  if (!empty) pick.setAttribute("aria-expanded", String(open));
  const lead = empty ? h("span", "chev-space") : chevron(open);
  pick.append(lead, branchIcon(), h("span", "branch", branchName(w)));
  // The main checkout shows its tag only while it is off its usual branch.
  const usual = p.default_branch ? w.branch === p.default_branch : ["main", "master"].includes(w.branch ?? "");
  if (w.is_main && !usual) pick.appendChild(h("span", "tag", t("sidebar.mainFolder")));
  if (w.locked) pick.appendChild(h("span", "tag", t("sidebar.locked")));
  if (w.prunable) pick.appendChild(h("span", "tag", t("sidebar.prunable")));
  if (removing.has(w.path)) pick.appendChild(h("span", "tag", t("sidebar.removing")));
  // The rows under an open worktree show the detail.
  const diff = showsChanges(p) && !open ? changeCounts(w) : null;
  if (diff) pick.appendChild(diff);
  row.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    worktreeMenu(p, w, e.clientX, e.clientY);
  });
  row.appendChild(pick);
  const plus = button("wt-plus", "", () => launchMenu.open(plus, p, w));
  plus.dataset.wt = w.path;
  plus.setAttribute("aria-label", t("sidebar.startIn", { name: branchName(w) }));
  plus.setAttribute("aria-haspopup", "menu");
  plus.appendChild(plusIcon());
  row.appendChild(plus);
  block.appendChild(row);

  if (open) {
    // No count line: the rows carry their own marks, and a line that comes
    // and goes would move the whole tree.
    const body = h("div", "wt-body");
    body.dataset.kids = w.path;
    body.appendChild(sessionBlock(lines, color));
    if (changes) body.appendChild(changes);
    if (showsFiles(p)) body.appendChild(filesBlock(w));
    block.appendChild(body);
  }

  const err = removeErrors.get(w.path);
  if (err) block.appendChild(errorRow(err));
  return block;
}

// Right-click on the empty part of the list: the + menu for the current worktree.
$<HTMLElement>("sidebar-scroll").addEventListener("contextmenu", (e) => {
  if (e.defaultPrevented || (e.target as Element).closest("button, input, .wt-count")) return;
  const at = currentWorktree();
  if (!at) return;
  e.preventDefault();
  launchMenu.open({ x: e.clientX, y: e.clientY }, at.p, at.w);
});
