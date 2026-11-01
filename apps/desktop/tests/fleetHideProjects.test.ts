import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// The real tree.ts, sidebar.ts, menus.ts and commands.ts. Each module they
// import and the test does not give answers every call with an empty list.
class El {
  tag: string;
  className: string;
  textContent: string;
  type = "";
  children: El[] = [];
  dataset: Record<string, string> = {};
  attrs: Record<string, string> = {};
  style = { height: "", setProperty() {} };
  listeners = new Map<string, ((e?: unknown) => void)[]>();
  classList = {
    add: (c: string) => void (this.className += ` ${c}`),
    remove() {},
    toggle() {},
    contains: (c: string) => this.className.split(" ").includes(c),
  };
  constructor(tag: string, cls = "", text = "") {
    this.tag = tag;
    this.className = cls;
    this.textContent = text;
  }
  append(...kids: (El | string)[]) {
    for (const k of kids) this.children.push(typeof k === "string" ? new El("text", "", k) : k);
  }
  appendChild(k: El) {
    this.append(k);
    return k;
  }
  setAttribute(k: string, v: string) {
    this.attrs[k] = v;
  }
  hasAttribute(k: string) {
    return k in this.attrs;
  }
  addEventListener(ev: string, run: (e?: unknown) => void) {
    this.listeners.set(ev, [...(this.listeners.get(ev) ?? []), run]);
  }
  click() {
    for (const run of this.listeners.get("click") ?? []) run({ detail: 1 });
  }
  getBoundingClientRect() {
    return { right: 0, top: 0 };
  }
  all(): El[] {
    return [this, ...this.children.flatMap((c) => c.all())];
  }
  text(): string {
    return this.textContent + this.children.map((c) => c.text()).join("");
  }
}
const h = (tag: string, cls = "", text = "") => new El(tag, cls, text);
const button = (cls: string, text: string, run: () => void) => {
  const b = h("button", cls, text);
  b.addEventListener("click", run);
  return b;
};
const stub = new Proxy({}, { get: (_, k) => (k === "__esModule" ? false : () => []) });
const withStubs = (imports: Record<string, unknown>) => new Proxy(imports, { get: (t, k: string) => t[k] ?? stub });
const orStub = (mod: Record<string, unknown>) => new Proxy(mod, { get: (t, k: string) => t[k] ?? (stub as any)[k] });

function app(host: string | null = null) {
  const projects = [
    { name: "web", path: "/harbor/web", fleet: "Harbor", worktrees: [{ path: "/harbor/web", branch: "main", is_main: true }] },
    { name: "api", path: "/harbor/api", fleet: "Harbor", worktrees: [{ path: "/harbor/api", branch: "main", is_main: true }] },
    { name: "site", path: "/dock/site", fleet: "Dock", worktrees: [{ path: "/dock/site", branch: "main", is_main: true }] },
  ];
  const S: any = { projects, focused: null, roveKey: null, selection: [], groups: [], closedProjects: [], renamingSession: null };
  const sessions = new Map<string, any>();
  for (const s of [
    { id: "skipper", title: "Skipper", skipper: "fleet:Harbor", state: "idle", cwd: "/skipper/fleets/Harbor" },
    { id: "web-1", title: "fix login", state: "working", cwd: "/harbor/web" },
    { id: "web-2", title: "approve rm", state: "waiting", cwd: "/harbor/web" },
    { id: "api-1", title: "add rate limit", state: "idle", cwd: "/harbor/api" },
    { id: "api-2", title: "shell", state: "idle", cwd: "/harbor/api" },
    { id: "site-1", title: "copy edits", state: "idle", cwd: "/dock/site" },
  ]) sessions.set(s.id, s);
  const place = (s: any) => {
    const project = projects.find((p) => s.cwd.startsWith(p.path));
    return project ? { project, worktree: project.worktrees[0] } : null;
  };
  const stored = new Map<string, string>();
  const globals = {
    localStorage: { getItem: (k: string) => stored.get(k) ?? null, setItem: (k: string, v: string) => stored.set(k, v) },
    requestAnimationFrame: (run: () => void) => run(),
    performance: { now: () => 0 },
    console,
  };
  const fleetSkipperOf = (f: string) => [...sessions.values()].find((s) => s.skipper === `fleet:${f}`);
  const state = { S, sessions, removing: new Set(), removeErrors: new Map(), collapsed: new Set(), selectedWorktree: new Map(), DEFAULT_ACCENT: "#888" };
  const stateQueries = { place, accent: () => "#888", awayPlaces: () => [] };
  const skiffAgent = orStub({ fleetSkipperOf, projectSkipperOf: () => null, fleetScope: (f: string) => `fleet:${f}`, skiffAgentFolder: () => null });
  const model = { isUnread: () => false, taskTitle: (s: any) => s.title, agentName: () => "claude", branchName: (w: any) => w.branch, isFolder: () => false, scopeName: (x: string) => x };
  const stored_ = moduleFromSource("../src/app/stored.ts", {}, globals, "");
  const tree = moduleFromSource("../src/workspace/tree.ts", withStubs({
    "../app/skiffAgent": skiffAgent, "./model": model, "../app/stored": stored_, "../app/state": state,
    "../app/stateQueries": stateQueries, "../ui/dom": { h }, "../ui/icons": { icon: () => h("svg") },
    "../app/hosts": { shownHost: () => host },
  }), globals, "");
  let rendered = 0;
  const show = (id: string) => {
    S.focused = id;
  };
  const view = { revealSession: show, selectWorktree() {}, refocusTerminal() {} };
  let menu: any[] = [];
  const ui = {
    "../ui/dom": { h, button, $: () => h("div") },
    "../ui/icons": { icon: () => h("svg"), chevron: () => h("svg", "chev"), plusIcon: () => h("svg"), branchIcon: () => h("svg") },
    "../ui/contextMenu": { ctxMenu: { open: (_x: number, _y: number, _t: string, items: any[]) => (menu = items) } },
    "../appearance/agentIcon": { agentIcon: () => h("span", "agent-icon"), agentKind: () => "claude" },
    "../appearance/stateIcon": { stateIcon: () => h("span", "mark") },
  };
  const sidebar = moduleFromSource("../src/workspace/sidebar.ts", withStubs({
    ...ui, "./tree": tree, "../app/state": state, "../app/stateQueries": stateQueries, "../app/skiffAgent": skiffAgent,
    "./model": model, "./view": view, "../app/renderRequest": { render: () => rendered++ }, "./profile": { workspaceOpen: () => false, shows: () => true },
  }), { ...globals, document: { activeElement: null } }, "\nexports.sectionHead = sectionHead; exports.shownBlock = shownBlock;");
  const menus = moduleFromSource("../src/workspace/menus.ts", withStubs({
    ...ui, "./tree": tree, "../app/state": state, "../app/stateQueries": stateQueries, "../app/skiffAgent": skiffAgent,
    "./model": model, "./view": view, "../app/render": { render: () => rendered++ },
  }), globals, "");
  let palette: () => any[] = () => [];
  moduleFromSource("../src/app/commands.ts", withStubs({
    "../ui/switcher": { createSwitcher: (items: () => any[]) => (palette = items) },
    "./state": state, "./stateQueries": orStub(stateQueries), "../workspace/model": model, "../workspace/view": view,
  }), globals, "");
  const harbor = () => ({ fleet: "Harbor", projects: projects.slice(0, 2) });
  // The fleet head as the tree draws it now, with the rows under it.
  const draw = (sec = harbor()) => {
    const split = tree.projectsHidden(sec.fleet) ? tree.hiddenSplit(sec.projects) : null;
    const alone = !!split && !fleetSkipperOf(sec.fleet) && !split.shown.length;
    const head: El = sidebar.sectionHead(sec, split?.hidden ?? null, alone);
    const shown: El | null = split?.shown.length ? sidebar.shownBlock(split.shown) : null;
    return { head, split, rows: shown ? shown.children.map((r) => r.dataset.session) : null };
  };
  const fleetMenu = (name: string) => {
    menus.fleetMenu({ name, projects: projects.filter((p) => p.fleet === name) }, 0, 0);
    return menu.find((m) => m.label === "Hide projects" || m.label === "Show projects");
  };
  return { S, sessions, tree, stored, draw, fleetMenu, palette: () => palette(), harbor, rendered: () => rendered };
}

test("Hide projects hides a fleet's projects; waiting and on-screen sessions stay; the count shows them again", () => {
  const a = app();
  // Shown by default: the item offers to hide, with no check, and the head has no count.
  let item = a.fleetMenu("Harbor");
  assert.equal(item.label, "Hide projects");
  assert.equal(item.icon, "senses-eye-off");
  assert.equal(a.draw().head.all().find((e) => e.className === "fleet-hidden"), undefined);

  item.run();
  assert.equal(a.tree.projectsHidden("Harbor"), true);
  item = a.fleetMenu("Harbor");
  assert.equal(item.label, "Show projects");
  assert.equal(item.icon, "senses-eye");
  assert.match(a.stored.get("skiff.tree.hidden") ?? "", /fleet:Harbor/);
  assert.deepEqual([...a.tree.visibleProjects().map((p: any) => p.name)], ["site"]);

  // The waiting session stays under the head. The Skipper has its own row and is not counted.
  let d = a.draw();
  assert.deepEqual(d.rows, ["web-2"]);
  const count = d.head.all().find((e) => e.className === "fleet-hidden")!;
  assert.equal(count.textContent, "· 3");
  assert.equal(d.head.all().find((e) => e.className === "fleet-pick")!.hasAttribute("aria-expanded"), true);

  // The session on screen stays too.
  a.S.focused = "api-1";
  d = a.draw();
  assert.deepEqual(d.rows, ["web-2", "api-1"]);
  assert.equal(d.head.all().find((e) => e.className === "fleet-hidden")!.textContent, "· 2");

  // A click on the count shows the projects.
  const before = a.rendered();
  d.head.all().find((e) => e.className === "fleet-hidden")!.click();
  assert.equal(a.tree.projectsHidden("Harbor"), false);
  assert.equal(a.rendered(), before + 1);
  assert.equal(a.draw().split, null);
  assert.equal(a.fleetMenu("Harbor").label, "Hide projects");
});

test("a session picked in the palette shows alone under its hidden fleet", () => {
  const a = app();
  a.fleetMenu("Harbor").run();
  assert.deepEqual(a.draw().rows, ["web-2"]);
  const found = a.palette().find((i) => i.kind === "session" && i.primary.includes("add rate limit"));
  found.run();
  assert.equal(a.S.focused, "api-1");
  // The projects stay hidden; the picked session is an exception.
  assert.equal(a.tree.projectsHidden("Harbor"), true);
  assert.deepEqual(a.draw().rows, ["web-2", "api-1"]);
  // A worktree picked in the palette has no row while hidden: the projects show.
  const w = a.palette().find((i) => i.kind === "worktree" && i.secondary === "web");
  assert.ok(w);
  a.tree.worktreeRowKey(a.harbor().projects[0], a.harbor().projects[0].worktrees[0]);
  assert.equal(a.tree.projectsHidden("Harbor"), false);
});

test("a fleet with no Skipper and nothing to show keeps its head alone, with no chevron or fold", () => {
  const a = app();
  const dock = { fleet: "Dock", projects: a.S.projects.filter((p: any) => p.fleet === "Dock") };
  a.fleetMenu("Dock").run();
  let d = a.draw(dock);
  assert.equal(d.rows, null);
  const pick = d.head.all().find((e) => e.className === "fleet-pick")!;
  assert.equal(pick.hasAttribute("aria-expanded"), false);
  assert.equal(pick.dataset.fold, undefined);
  assert.equal(d.head.all().some((e) => e.className === "chev"), false);
  assert.equal(d.head.all().find((e) => e.className === "fleet-hidden")!.textContent, "· 1");
  // A session that starts to wait shows under the head, and the head folds again.
  a.sessions.get("site-1").state = "waiting";
  d = a.draw(dock);
  assert.deepEqual(d.rows, ["site-1"]);
  assert.equal(d.head.all().find((e) => e.className === "fleet-pick")!.hasAttribute("aria-expanded"), true);
  assert.equal(d.head.all().find((e) => e.className === "fleet-hidden")!.textContent, "· 0");
});

test("a fleet on a remote Host hides the same way, with its own stored choice", () => {
  const a = app("box");
  a.fleetMenu("Harbor").run();
  assert.equal(a.tree.projectsHidden("Harbor"), true);
  assert.deepEqual(a.draw().rows, ["web-2"]);
  assert.equal(a.draw().head.all().find((e) => e.className === "fleet-hidden")!.textContent, "· 3");
  assert.deepEqual(JSON.parse(a.stored.get("skiff.tree.hidden")!), ["host:box/fleet:Harbor"]);
  a.draw().head.all().find((e) => e.className === "fleet-hidden")!.click();
  assert.equal(a.tree.projectsHidden("Harbor"), false);
});
