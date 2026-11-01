import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// The real model.ts, stateQueries.ts, tree.ts, sidebarLines.ts and sidebar.ts draw
// the tree into a small fake DOM. Each other module answers every call with an empty list.
class El {
  tag: string;
  className: string;
  textContent: string;
  type = "";
  tabIndex = 0;
  scrollTop = 0;
  children: El[] = [];
  parent: El | null = null;
  dataset: Record<string, string> = {};
  style = { height: "", setProperty() {} };
  classList = { add: (c: string) => void (this.className += ` ${c}`), remove() {}, toggle() {}, contains: (c: string) => this.className.split(" ").includes(c) };
  constructor(tag: string, cls = "", text = "") {
    this.tag = tag;
    this.className = cls;
    this.textContent = text;
  }
  append(...kids: (El | string)[]) {
    for (const k of kids) this.appendChild(typeof k === "string" ? new El("text", "", k) : k);
  }
  appendChild(k: El) {
    k.remove();
    k.parent = this;
    this.children.push(k);
    return k;
  }
  remove() {
    if (this.parent) this.parent.children = this.parent.children.filter((c) => c !== this);
    this.parent = null;
  }
  insertBefore(k: El, ref: El | null) {
    k.remove();
    k.parent = this;
    const i = ref ? this.children.indexOf(ref) : -1;
    if (i < 0) this.children.push(k);
    else this.children.splice(i, 0, k);
  }
  replaceWith(k: El) {
    const p = this.parent!;
    p.children[p.children.indexOf(this)] = k;
    k.parent = p;
    this.parent = null;
  }
  get lastElementChild() {
    return this.children[this.children.length - 1] ?? null;
  }
  setAttribute() {}
  hasAttribute() {
    return false;
  }
  addEventListener() {}
  getBoundingClientRect() {
    return { right: 0, top: 0, bottom: 0 };
  }
  closest() {
    return null;
  }
  querySelector() {
    return null;
  }
  all(): El[] {
    return this.children.flatMap((c) => [c, ...c.all()]);
  }
  querySelectorAll(sel: string) {
    return sel.startsWith(".session-row[data-look]") ? this.all().filter((e) => e.className.startsWith("session-row") && e.dataset.look !== undefined) : [];
  }
  text(): string {
    return this.textContent + this.children.map((c) => c.text()).join("");
  }
}
const h = (tag: string, cls = "", text = "") => new El(tag, cls, text);
const stub = new Proxy({}, { get: (_, k) => (k === "__esModule" ? false : () => []) });
const withStubs = (imports: Record<string, unknown>) => new Proxy(imports, { get: (t, k: string) => t[k] ?? stub });

const wt = (path: string, branch: string, is_main = false) => ({ path, branch, is_main, locked: false, prunable: false });
const projectList = () => [
  { name: "web", short: "web", path: "/src/web", fleet: null, error: null, default_branch: "main", worktrees: [wt("/src/web", "main", true), wt("/src/web-wt/fix", "fix")] },
  { name: "api", short: "api", path: "/src/api", fleet: null, error: null, default_branch: "trunk", worktrees: [wt("/src/api", "trunk", true)] },
];
const session = (id: string, cwd: string, away: string[]) => ({
  id, cwd, role: "agent", command: "claude", name: "", label: `${id} task`, title: null, state: "idle",
  started_at: 0, last_output_at: 0, unread: false, exit_code: null, agent_exit: null,
  away: away.map((path) => ({ path, command: "cargo", at: 0 })),
});

function app() {
  const S: any = { projects: projectList(), groups: [], focused: null, roveKey: null, selection: [], renamingSession: null, renaming: null, selectedProject: null, dataState: "ready", dataNote: null, closedProjects: [] };
  const sessions = new Map<string, any>();
  for (const s of [
    // Works in api too. Its own worktree, a folder in no project and a repeat of api do not count.
    session("w1", "/src/web/app", ["/src/api", "/src/web", "/elsewhere", "/src/api/"]),
    session("a1", "/src/api", ["/src/web-wt/fix"]),
    // Its own worktree, and a folder inside it, are not away.
    session("f1", "/src/web-wt/fix/sub", ["/src/web-wt/fix/deep", "/src/web-wt/fix"]),
    session("o1", "/tmp/x", ["/src/web"]),
  ]) sessions.set(s.id, s);
  const tallies = new Map<string, number>();
  const globals = {
    localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
    requestAnimationFrame: (run: () => void) => run(), performance: { now: () => 0 }, console,
    HTMLInputElement: class {}, HTMLButtonElement: class {}, HTMLSelectElement: class {},
    document: { activeElement: null, elementFromPoint: () => null },
  };
  const state = { S, sessions, removing: new Set(), removeErrors: new Map(), collapsed: new Set(), selectedWorktree: new Map(), DEFAULT_ACCENT: "#888", MAX_PANES: 4 };
  const model = moduleFromSource("../src/workspace/model.ts", {}, globals, "");
  const layout = { sessionsOf: () => [], leaf: (id: string) => ({ type: "leaf", id }) };
  const stateQueries = moduleFromSource("../src/app/stateQueries.ts", withStubs({
    "./state": state, "../workspace/model": model, "../workspace/layout": layout, "../appearance/appTheme": { ink: (c: string) => c },
  }), globals, "");
  const skiffAgent = withStubs({ fleetSkipperOf: () => null, projectSkipperOf: () => null, fleetScope: (f: string) => `fleet:${f}`, skiffAgentFolder: () => null, skiffAgentSession: () => null, projectSkippers: () => [], syncSkiffAgent() {} });
  const stored = moduleFromSource("../src/app/stored.ts", {}, globals, "");
  const tree = moduleFromSource("../src/workspace/tree.ts", withStubs({
    "../app/skiffAgent": skiffAgent, "./model": model, "../app/stored": stored, "../app/state": state, "../app/stateQueries": stateQueries,
    "../ui/dom": { h }, "../ui/icons": { icon: () => h("svg") }, "../app/hosts": { shownHost: () => null },
  }), globals, "");
  const view = withStubs({ groupHome: () => null });
  const lines = moduleFromSource("../src/workspace/sidebarLines.ts", withStubs({
    "./layout": layout, "./model": model, "../app/state": state, "../app/stateQueries": stateQueries, "./view": view, "../app/skiffAgent": skiffAgent,
  }), globals, "");
  const side = h("div", "sidebar-scroll");
  const sidebar = moduleFromSource("../src/workspace/sidebar.ts", withStubs({
    "./sidebarLines": lines, "./tree": tree, "./model": model, "./view": view, "../app/state": state, "../app/stateQueries": stateQueries, "../app/skiffAgent": skiffAgent,
    "./layoutSlots": { filledOf: () => [] },
    "../appearance/agentIcon": { agentIcon: () => h("span", "agent-icon"), agentKind: () => "claude" },
    "../appearance/appTheme": { ink: (c: string) => c },
    "../appearance/themes": { appliedTheme: () => "dark", ownTheme: () => null, signatureColor: () => "#000" },
    "./skipperFace": { faceSig: () => null },
    "./changes": { showsChanges: () => false },
    "./files": { showsFiles: () => false },
    "../ui/dom": { $: () => side, button: (cls: string, text: string) => h("button", cls, text), h },
    "../ui/icons": { branchIcon: () => h("svg"), chevron: () => h("svg"), icon: () => h("svg"), plusIcon: () => h("svg") },
    "../app/keyboard": { findRow: () => null, itemKey: () => "", listItems: () => [] },
    "./profile": { renderWorkspaceStatus() {}, shows: () => true, workspaceOpen: () => false },
    "../app/hosts": { hostConnected: () => true, hostName: () => "", isRemote: () => false },
    "../diagnostics/stalls": { step: (_: string, run: () => unknown) => run(), tally: (k: string) => tallies.set(k, (tallies.get(k) ?? 0) + 1) },
    "./skippersView": { renderSkippers() {}, renderViewToggle() {}, skippersOn: () => false },
    "../appearance/stateIcon": { stateIcon: () => h("span", "mark") },
    "../ui/fold": { growOpened() {} },
  }), globals, "");
  // The tree as a person reads it: one line per head, project, worktree and session row.
  const rows = () => side.all()
    .filter((e) => /^(tree-head-name|project-name|branch|session-row)/.test(e.className))
    .map((e) => (e.className.startsWith("session-row") ? `  ${e.className.includes("pointer-row") ? "pointer" : "session"}: ${e.text()}` : e.text()));
  const draw = () => {
    tallies.clear();
    sidebar.renderSidebar();
    return tallies.get("sidebar.drawn") ? "drawn" : "same";
  };
  return { S, sessions, draw, rows };
}

const TREE = [
  "Projects",
  "web",
  "main",
  "  session: w1 task→ trunk",
  "  pointer: o1 taskfrom x",
  "fix",
  "  session: f1 task",
  "  pointer: a1 taskfrom trunk",
  "api",
  "  session: a1 task→ fix",
  "  pointer: w1 taskfrom main",
  "Other",
  "  session: o1 task→ main",
];

test("the tree shows each session's away worktrees and the visitors they bring", () => {
  const a = app();
  assert.equal(a.draw(), "drawn");
  assert.deepEqual(a.rows(), TREE);
});

test("the same away lists, sent again, draw nothing; a changed one draws again", () => {
  const a = app();
  a.draw();
  // skiffd sends a new list with each event, here with the same worktrees.
  for (const s of a.sessions.values()) s.away = s.away.map((x: any) => ({ ...x, at: 1 }));
  assert.equal(a.draw(), "same");
  assert.equal(a.draw(), "same");
  assert.deepEqual(a.rows(), TREE);

  // a1 left fix: its tag and the pointer under fix go.
  a.sessions.get("a1").away = [];
  assert.equal(a.draw(), "drawn");
  assert.deepEqual(a.rows(), TREE.filter((r) => r !== "  pointer: a1 taskfrom trunk").map((r) => (r === "  session: a1 task→ fix" ? "  session: a1 task" : r)));

  // w1 starts work in fix.
  a.sessions.get("w1").away = [{ path: "/src/web-wt/fix", command: "cargo", at: 2 }];
  assert.equal(a.draw(), "drawn");
  assert.deepEqual(a.rows(), [
    "Projects",
    "web",
    "main",
    "  session: w1 task→ fix",
    "  pointer: o1 taskfrom x",
    "fix",
    "  session: f1 task",
    "  pointer: w1 taskfrom main",
    "api",
    "  session: a1 task",
    "Other",
    "  session: o1 task→ main",
  ]);
});

test("a new project list places the away worktrees again", () => {
  const a = app();
  a.draw();
  // api closes: w1 no longer works in a project worktree there, and a1 goes to Other.
  a.S.projects = projectList().filter((p) => p.name !== "api");
  assert.equal(a.draw(), "drawn");
  assert.deepEqual(a.rows(), [
    "Projects",
    "web",
    "main",
    "  session: w1 task",
    "  pointer: o1 taskfrom x",
    "fix",
    "  session: f1 task",
    "  pointer: a1 taskfrom api",
    "Other",
    "  session: a1 task→ fix",
    "  session: o1 task→ main",
  ]);
  // api comes back: the list places w1's away worktree in it again.
  a.S.projects = projectList();
  assert.equal(a.draw(), "drawn");
  assert.deepEqual(a.rows(), TREE);
});

test("a session that moves to another folder places its away worktrees from there", () => {
  const a = app();
  a.draw();
  // f1 moves to main: fix, where its process ran, is now away.
  a.sessions.get("f1").cwd = "/src/web";
  assert.equal(a.draw(), "drawn");
  assert.deepEqual(a.rows(), [
    "Projects",
    "web",
    "main",
    "  session: f1 task→ fix",
    "  session: w1 task→ trunk",
    "  pointer: o1 taskfrom x",
    "fix",
    "  pointer: a1 taskfrom trunk",
    "  pointer: f1 taskfrom main",
    "api",
    "  session: a1 task→ fix",
    "  pointer: w1 taskfrom main",
    "Other",
    "  session: o1 task→ main",
  ]);
});
