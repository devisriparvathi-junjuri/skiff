import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** The bell means one thing: a session waits, as the top bar's Waiting counts
 *  it. A project Skipper's tasks that need the person are an amber count on
 *  its row, as on the Skiff agent's row. A task never sent needs nobody when
 *  its pane goes. */

// --- The Skipper row, drawn by the real sidebar.ts and tree.ts into a small fake DOM.
class El {
  tag: string;
  className: string;
  textContent: string;
  type = "";
  title = "";
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

/** The tree with one project and its Skipper, idle; its tasks as `counts` gives them. */
function skipperRow(counts: { need: number; working: number }) {
  const S: any = {
    projects: [{ name: "web", short: "web", path: "/src/web", fleet: null, error: null, default_branch: "main", worktrees: [{ path: "/src/web", branch: "main", is_main: true, locked: false, prunable: false }] }],
    groups: [], focused: null, roveKey: null, selection: [], renamingSession: null, renaming: null, selectedProject: null, dataState: "ready", dataNote: null, closedProjects: [],
  };
  const skipper = {
    id: "sk", cwd: "/agent/projects/web", role: "agent", command: "claude", name: "web Skipper", label: "web Skipper", title: null, state: "idle",
    started_at: 0, last_output_at: 0, unread: false, exit_code: null, agent_exit: null, away: [], skipper: "web",
  };
  const sessions = new Map<string, any>([[skipper.id, skipper]]);
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
  const skiffAgent = withStubs({
    fleetSkipperOf: () => null, projectSkipperOf: (name: string) => (name === "web" ? skipper : null), fleetScope: (f: string) => `fleet:${f}`,
    skiffAgentFolder: () => null, skiffAgentSession: () => null, projectSkippers: () => [skipper], syncSkiffAgent() {},
    skipperWord: () => "Skipper", projectWatch: () => ({ state: "off", text: "" }),
  });
  const stored = moduleFromSource("../src/app/stored.ts", {}, globals, "");
  const icon = (paths: unknown) => h("svg", String(paths).includes("M6 8a6 6 0 0 1 12 0") ? "bell" : "");
  const tree = moduleFromSource("../src/workspace/tree.ts", withStubs({
    "../app/skiffAgent": skiffAgent, "./model": model, "../app/stored": stored, "../app/state": state, "../app/stateQueries": stateQueries,
    "../ui/dom": { h }, "../ui/icons": { icon }, "../app/hosts": { shownHost: () => null },
  }), globals, "");
  const view = withStubs({ groupHome: () => null });
  const lines = moduleFromSource("../src/workspace/sidebarLines.ts", withStubs({
    "./layout": layout, "./model": model, "../app/state": state, "../app/stateQueries": stateQueries, "./view": view, "../app/skiffAgent": skiffAgent,
  }), globals, "");
  const side = h("div", "sidebar-scroll");
  const sidebar = moduleFromSource("../src/workspace/sidebar.ts", withStubs({
    "./sidebarLines": lines, "./tree": tree, "./model": model, "./view": view, "../app/state": state, "../app/stateQueries": stateQueries, "../app/skiffAgent": skiffAgent,
    "./ledger": { ledgerCounts: () => counts, ledgerNeeds: () => 0, ledgerWorkTitle: () => "Review the branches" },
    "./layoutSlots": { filledOf: () => [] },
    "../appearance/agentIcon": { agentIcon: () => h("span", "agent-icon"), agentKind: () => "claude", launchIcon: () => h("span") },
    "../appearance/appTheme": { ink: (c: string) => c },
    "../appearance/themes": { appliedTheme: () => "dark", ownTheme: () => null, signatureColor: () => "#000" },
    "./skipperFace": { faceSig: () => null, skipperFace: () => h("span", "face") },
    "./changes": { showsChanges: () => false },
    "./files": { showsFiles: () => false },
    "../ui/dom": { $: () => side, button: (cls: string, text: string) => h("button", cls, text), h },
    "../ui/icons": { branchIcon: () => h("svg"), chevron: () => h("svg"), icon, plusIcon: () => h("svg") },
    "../app/keyboard": { findRow: () => null, itemKey: () => "", listItems: () => [] },
    "./profile": { renderWorkspaceStatus() {}, shows: () => true, workspaceOpen: () => false },
    "../app/hosts": { hostConnected: () => true, hostName: () => "", isRemote: () => false },
    "../diagnostics/stalls": { step: (_: string, run: () => unknown) => run(), tally() {} },
    "./skippersView": { renderSkippers() {}, renderViewToggle() {}, skippersOn: () => false },
    "../appearance/stateIcon": { stateIcon: () => h("span", "anchored") },
    "../ui/fold": { growOpened() {} },
    "../i18n": { t: (k: string) => k, tn: (k: string, n: number) => `${n} ${k}`, sentences: (s: string) => s },
  }), globals, "");
  sidebar.renderSidebar();
  const row = side.all().find((e) => e.className.includes("project-skipper-row"));
  assert.ok(row, "the project Skipper's row is drawn");
  const has = (cls: string) => row!.all().filter((e) => e.className.split(" ").includes(cls));
  return { row: row!, has, waiting: [...sessions.values()].filter((s) => s.state === "waiting").length };
}

test("a Skipper row's needs are a count, not a bell", () => {
  const r = skipperRow({ need: 2, working: 1 });
  // No session waits: no bell anywhere on the row, as the top bar's Waiting reads 0.
  assert.equal(r.waiting, 0);
  assert.deepEqual(r.has("bell"), []);
  assert.deepEqual(r.has("roll-waiting"), []);
  // The tasks that need the person: one amber count, with its tooltip.
  const [count] = r.has("ledger-count");
  assert.equal(count?.textContent, "2");
  assert.equal(count?.title, "2 ledger.tasksNeedYou");
  // The tasks an agent is on keep the blue count.
  assert.equal(r.has("roll-working").length, 1);
});

test("a Skipper row with no needs shows no amber count", () => {
  const r = skipperRow({ need: 0, working: 1 });
  assert.deepEqual(r.has("ledger-count"), []);
  assert.equal(r.has("roll-working").length, 1);
});

// --- The Ledger's own count: the real ledger.ts over two tasks of one project Skipper.
const skipper = { id: "sk", state: "idle", unread: false, skipper: "skiff", started_at: "1" };
const task = (uid: string, state: string) => ({
  id: uid.length, uid, kind: "task", parent: null, owner: "skiff", project: "skiff",
  pane_id: "gone-pane", pane_gen: null, task: "Review the branches.", sent_at: state === "pending" ? null : "x", title: null,
  state, updated_at: "x", closed_at: null, result_exists: false, bucket: state === "pending" ? "open" : "working", gone_at: null,
});

function ledger(rows: unknown[]) {
  return moduleFromSource(
    "../src/workspace/ledger.ts",
    {
      "@tauri-apps/api/core": { invoke: () => Promise.resolve() },
      "@tauri-apps/api/event": { listen: () => Promise.resolve(() => {}) },
      "../app/hostCaps": { hostInvoke: () => Promise.resolve(rows) },
      "../app/dataPlane": { dataEpoch: () => 0 },
      "../terminal/peek": { openLedgerResult() {}, openPath() {}, previewToken: () => 1 },
      "../app/renderRequest": { render() {} },
      "../app/state": { S: {}, sessions: new Map([[skipper.id, skipper]]) },
      "./model": { taskTitle: () => "", scopeName: (s: string) => s, scopeFleet: () => null },
      "./profile": { ledgerScope: () => null, ledgerAllWork: () => true, setLedgerAllWork() {}, workspaceName: () => "", workspaceOpen: () => false },
    },
    { Promise, JSON, Map, Set, document: { getElementById: () => null, addEventListener() {} } },
    "",
  );
}

test("a task never sent needs nobody when its pane goes; a sent one does", async () => {
  const never = task("p0000001", "pending");
  const sent = task("s0000001", "sent");
  const mod = ledger([never, sent]);
  await mod.loadLedger();
  // The panel still shows the pending row, as pending, not as a need.
  assert.equal(mod.taskState(never), "pending");
  assert.equal(mod.needWord(mod.taskState(never)), null);
  // The sent task lost its agent's pane: the person must look.
  assert.equal(mod.taskState(sent), "gone");
  assert.deepEqual({ ...mod.ledgerCounts("skiff") }, { need: 1, working: 0 });
});
