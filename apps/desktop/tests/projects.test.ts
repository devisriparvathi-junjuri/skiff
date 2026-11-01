import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real `loadProjects`: which calls it makes for each Host and what it
 *  shows. The Tauri side is a fake that answers as the selected Host's data
 *  plane would, or fails as it does when no skiff-host answers. */
function page(o: { remote: boolean; caps: string[]; fail?: string; capsFail?: string; dataPlane?: unknown; skiffAgent?: unknown; ledger?: unknown; S?: any }) {
  const S: any = o.S ?? { agents: [{ id: "a" }], projects: [], closedProjects: [], projectsError: null, selectedProject: null, dataState: "loading", dataNote: null };
  const calls: string[] = [];
  const projects = [
    { name: "web", path: "/srv/web", closed: false, files: true, theme: "lagoon", worktrees: [{ path: "/srv/web", branch: "main" }] },
    { name: "old", path: "/srv/old", closed: true, worktrees: [] },
  ];
  const mod = moduleFromSource(
    "../src/app/daemon.ts",
    {
      "./connectPanel": { connectAttempt() {}, connectDone() {}, connectFailed() {}, connectWait: async () => {} },
      "@tauri-apps/api/core": {
        invoke: async (cmd: string) => {
          calls.push(cmd);
          if (cmd === "list_projects") {
            if (o.fail) throw o.fail;
            return projects;
          }
        },
        Channel: class {},
      },
      "@tauri-apps/api/event": { listen: async () => {} },
      "./hosts": { isRemote: () => o.remote, hostName: () => "devbox" },
      "./render": { render() {}, scheduleRender() {} },
      "./state": { S, sessions: new Map(), removeErrors: new Map(), OTHER: "\u0000other" },
      "./hostCaps": {
        hostCan: (c: string) => o.caps.includes(c),
        hostInvoke: async () => [],
        loadHostCaps: async () => {
          calls.push("host_capabilities");
          if (o.capsFail) throw o.capsFail;
        },
      },
      "./dataPlane": o.dataPlane ?? {},
      "../terminal/peek": { refreshHostDiff() {} },
      "../diagnostics/stalls": { work() {} },
      "../workspace/files": { filesFrom: (l: any[]) => void calls.push(`files ${l.filter((p) => p.files).map((p) => p.name)}`), reloadFiles() {} },
      "../workspace/changes": { reloadChanges() {} },
      "../appearance/backdrop": { reloadBackdrops: () => void calls.push("backdrops") },
      "../appearance/themes": { projectThemesFrom: (l: any[]) => void calls.push(`themes ${l.filter((p) => p.theme).map((p) => `${p.name}=${p.theme}`)}`) },
      "./skiffAgent": o.skiffAgent ?? { loadSkiffAgent: async () => void calls.push("get_skiff_agent") },
      "../workspace/ledger": o.ledger ?? { loadLedger: async () => void calls.push("ledger_rows") },
      "./stateQueries": { place: () => null },
      "../workspace/skipperFace": { loadSkipperAvatars: async () => {} },
    },
    { window: { addEventListener() {} }, document: { hasFocus: () => true }, Promise, Map, Set, console, JSON, performance },
    "",
  );
  return { mod, S, calls };
}

test("a remote Host with a data plane shows its own projects, with their settings from its rows", async () => {
  const p = page({ remote: true, caps: ["projects"] });
  await p.mod.loadProjects();
  assert.deepEqual(p.calls, ["list_projects", "get_skiff_agent", "files web", "themes web=lagoon"]);
  assert.deepEqual(p.S.projects.map((x: any) => x.path), ["/srv/web"]);
  assert.deepEqual(p.S.closedProjects.map((x: any) => x.name), ["old"]);
  assert.equal(p.S.projectsError, null);
  assert.equal(p.S.dataState, "ready");
});

test("a remote Host without a data plane shows its sessions only: no project read at all", async () => {
  const p = page({ remote: true, caps: [] });
  p.S.projects = [{ name: "stale", worktrees: [] }];
  await p.mod.loadProjects();
  // Skipper's settings are still asked of the Host (review M6 P2); no project read.
  assert.deepEqual(p.calls, ["get_skiff_agent"]);
  assert.equal(p.S.projects.length, 0);
  assert.equal(p.S.projectsError, null);
  // Not "loading" forever: the list says it is out of reach, and the data
  // plane check fills in why.
  assert.equal(p.S.dataState, "unavailable");
});

test("this Mac reads its projects and their settings through the data plane, as a remote Host does", async () => {
  const p = page({ remote: false, caps: ["projects"] });
  await p.mod.loadProjects();
  assert.ok(p.calls.includes("list_projects"));
  // Project settings come with the Host's rows.
  for (const c of ["files web", "themes web=lagoon", "get_skiff_agent"]) assert.ok(p.calls.includes(c), c);
  for (const c of ["files_projects", "hidden_changes_projects", "project_themes"]) assert.ok(!p.calls.includes(c), c);
});

test("this Mac without its service still asks, and the cause shows; nothing else stands in", async () => {
  const p = page({ remote: false, caps: [], fail: "The projects service did not start on this computer, so Skiff cannot show the projects. It starts the service again on its own." });
  await p.mod.loadProjects();
  assert.ok(p.calls.includes("list_projects"));
  assert.equal(p.S.projects.length, 0);
  assert.match(p.S.projectsError, /projects service did not start/);
  assert.equal(p.S.dataState, "unavailable");
  assert.match(p.S.dataNote, /projects service did not start/);
});

test("review M4 P2: a failed project or capability read on a live data connection is read again by the next check", async () => {
  // The real dataPlane module over one live connection, and the real page hooks.
  const live = (o: any) => {
    const dataPlane = moduleFromSource(
      "../src/app/dataPlane.ts",
      { "@tauri-apps/api/core": { invoke: async () => ({ ready: true, waiting: false, host_id: "H", instance_id: "i", connection: 1, capabilities: [], message: null }) } },
      { window: { setTimeout() {}, clearTimeout() {} }, Promise },
      "",
    );
    let hooks: any;
    const p = page(Object.assign(o, { remote: true, caps: ["projects"], dataPlane: { watchDataPlane: (h: unknown) => (hooks = h) } }));
    p.mod.followDataPlane();
    const count = (cmd: string) => p.calls.filter((c) => c === cmd).length;
    return { p, o, count, check: () => dataPlane.checkDataPlane(hooks) };
  };
  for (const [what, cmd] of [["fail", "list_projects"], ["capsFail", "host_capabilities"]]) {
    const o: any = { [what]: `the ${cmd} read failed` };
    const l = live(o);
    // The first ready check reads, and that read fails.
    await l.check();
    assert.equal(l.count(cmd), 1, what);
    // Repaired: the next check on the same connection reads again, once.
    o[what] = undefined;
    for (let i = 0; i < 10; i++) await l.check();
    assert.equal(l.count(cmd), 2, what);
    assert.equal(l.p.S.dataState, "ready", what);
    // Each recovery reads the background images again too (review M5 P2).
    assert.equal(l.count("backdrops"), 2, what);
  }
});

/** The real skiffAgent and ledger modules on one fake Host whose capabilities can change. */
function hostModules(caps: string[], S: any, sessions: Map<string, any>, dataPlane: any) {
  const asked: string[] = [];
  const hostInvoke = (cap: string, cmd: string) => {
    if (!caps.includes(cap)) return Promise.reject(new Error(`This Host does not offer ${cap}.`));
    asked.push(cmd);
    if (cmd === "get_skiff_agent") return Promise.resolve({ name: "Mate", agent: "claude", enabled: true, folder: "/home/me/skiff-agent", continue_on_restore: false, tasks: [] });
    return Promise.resolve([{ id: 1, uid: "a1b2c3d4", kind: "task", parent: null, pane_id: null, pane_gen: null, project: "", task: "Notes.", sent_at: "x", title: null, state: "done", updated_at: "x", closed_at: null, result_exists: false }]);
  };
  const skiffAgent = moduleFromSource(
    "../src/app/skiffAgent.ts",
    {
      "@tauri-apps/api/core": { invoke: (cmd: string) => (asked.push(`invoke ${cmd}`), Promise.resolve(null)) },
      "./hostCaps": { hostInvoke },
      "./hosts": { isRemote: () => true, hostName: () => "devbox" },
      "./state": { S, sessions, upsert() {} },
      "./keys": { setSkiffAgentName() {} },
      "./renderRequest": { render() {} },
    },
    { Promise, JSON, Map, Set, Date, window: { setTimeout: () => 0 }, clearTimeout() {} },
    "",
  );
  const ledger = moduleFromSource(
    "../src/workspace/ledger.ts",
    {
      "@tauri-apps/api/event": { listen: () => Promise.resolve(() => {}) },
      "../app/hostCaps": { hostInvoke },
      "../app/dataPlane": dataPlane,
      "../app/renderRequest": { render() {} },
      "../app/state": { S, sessions },
      "../app/skiffAgent": skiffAgent,
    },
    { Promise, JSON, Map, Set, document: { getElementById: () => null, addEventListener() {} } },
    "\nexports.rowsNow = () => rows; exports.errorNow = () => error;",
  );
  return { skiffAgent, ledger, asked };
}

test("review M6 P2: a remote Host's Skipper settings load at boot and recovery, with or without projects", async () => {
  for (const projects of [true, false]) {
    const caps = projects ? ["projects", "skipper"] : ["skipper"];
    const S: any = { agents: [{ id: "a" }], projects: [], closedProjects: [], projectsError: null, selectedProject: null, dataState: "loading", dataNote: null, skiffAgent: null };
    const sessions = new Map<string, any>([["s1", { id: "s1", coordinator: true, state: "idle" }]]);
    const m = hostModules(caps, S, sessions, { dataEpoch: () => 0 });
    // Boot: main.ts calls loadProjects.
    const p = page({ remote: true, caps, S, skiffAgent: m.skiffAgent, ledger: m.ledger });
    await p.mod.loadProjects();
    assert.deepEqual(m.asked, ["get_skiff_agent"], `projects ${projects}`);
    assert.equal(m.skiffAgent.skiffAgentSession()?.id, "s1", "the marked coordinator is Skipper");
    assert.equal(m.skiffAgent.skiffAgentName(), "Mate");
    // Recovery: a new data connection reads the Host's settings again.
    let hooks: any;
    const q = page({ remote: true, caps, S, skiffAgent: m.skiffAgent, ledger: m.ledger, dataPlane: { watchDataPlane: (h: unknown) => (hooks = h) } });
    q.mod.followDataPlane();
    await hooks.reload();
    assert.equal(m.asked.filter((c) => c === "get_skiff_agent").length, 2, `projects ${projects}`);
    // Nothing starts on the remote Host from here.
    assert.ok(!m.asked.some((c) => c.startsWith("invoke")));
  }
});

test("review M6 P2: a ledger refused at startup is read again when the capability comes late, the ledger unchanged", async () => {
  const caps = ["projects"];
  const S: any = { agents: [{ id: "a" }], projects: [], closedProjects: [], projectsError: null, selectedProject: null, dataState: "loading", dataNote: null, skiffAgent: null };
  let connection = 1;
  const dataPlane = moduleFromSource(
    "../src/app/dataPlane.ts",
    { "@tauri-apps/api/core": { invoke: async () => ({ ready: true, waiting: false, host_id: "H", instance_id: "i", connection, capabilities: caps, message: null }) } },
    { window: { setTimeout() {}, clearTimeout() {} }, Promise },
    "",
  );
  const m = hostModules(caps, S, new Map(), dataPlane);
  // Startup: main.ts's startLedger reads once, and the Host refuses.
  m.ledger.startLedger();
  await flush();
  assert.match(String(m.ledger.errorNow()), /does not offer ledger/);
  let hooks: any;
  const p = page({ remote: true, caps, S, skiffAgent: m.skiffAgent, ledger: m.ledger, dataPlane: { watchDataPlane: (h: unknown) => (hooks = h) } });
  p.mod.followDataPlane();
  await dataPlane.checkDataPlane(hooks);
  // The service restarts with the ledger: a new data connection, the same ledger file.
  caps.push("ledger", "skipper");
  connection = 2;
  await dataPlane.checkDataPlane(hooks);
  assert.equal(m.asked.filter((c) => c === "ledger_rows").length, 1, "the ledger read again");
  assert.equal(m.ledger.errorNow(), null, "the startup error is gone");
  assert.equal(m.ledger.rowsNow().length, 1);
});
