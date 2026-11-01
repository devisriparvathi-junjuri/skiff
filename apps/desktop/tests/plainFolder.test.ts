import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// A plain folder: one worktree, the folder, with no branch and no commit.
const folder = { name: "docs", path: "/home/me/docs", kind: "folder", default_branch: null, error: null, worktrees: [{ path: "/home/me/docs", branch: null, head: "", is_main: true, locked: false, prunable: false }] };
const repo = { name: "api", path: "/src/api", kind: "git", default_branch: "main", error: null, worktrees: [{ path: "/src/api", branch: "main", head: "abc", is_main: true, locked: false, prunable: false }] };

const stub = new Proxy({}, { get: (_, k) => (k === "__esModule" ? false : () => []) });
const withStubs = (imports: Record<string, unknown>) => new Proxy(imports, { get: (t, k: string) => t[k] ?? stub });
const orStub = (mod: Record<string, unknown>) => new Proxy(mod, { get: (t, k: string) => t[k] ?? (stub as any)[k] });

const model = moduleFromSource("../src/workspace/model.ts", {}, {}, "");

test("a plain folder's worktree shows the folder's name, a detached one stays detached", () => {
  assert.equal(model.isFolder(folder), true);
  assert.equal(model.isFolder(repo), false);
  // A project from an older daemon has no kind: git.
  assert.equal(model.isFolder({ ...repo, kind: undefined }), false);
  assert.equal(model.branchName(folder.worktrees[0]), "docs");
  assert.equal(model.branchName(repo.worktrees[0]), "main");
  assert.equal(model.branchName({ ...repo.worktrees[0], branch: null }), "(detached)");
});

test("a plain folder's only worktree is its bare worktree, with no branch to match", () => {
  const tree = moduleFromSource(
    "../src/workspace/tree.ts",
    withStubs({
      "./model": model,
      "../app/state": { S: { projects: [] }, sessions: new Map(), removing: new Set(), removeErrors: new Map() },
      "../app/stored": { storedSet: () => new Set() },
    }),
    { console, localStorage: { getItem: () => null, setItem() {} } },
    "",
  );
  assert.equal(tree.bareWorktree(folder), folder.worktrees[0]);
  assert.equal(tree.bareWorktree(repo), repo.worktrees[0]);
});

test("a plain folder shows no changes and never reads git for them", () => {
  const calls: string[] = [];
  const changes = moduleFromSource(
    "../src/workspace/changes.ts",
    withStubs({
      "./model": model,
      "@tauri-apps/api/core": { invoke: async (cmd: string) => (calls.push(cmd), []) },
      "../app/stored": { storedSet: () => new Set() },
    }),
    { console, localStorage: { getItem: () => null, setItem() {} } },
    "",
  );
  assert.equal(changes.showsChanges(folder), false);
  assert.equal(changes.showsChanges(repo), true);
  assert.deepEqual(calls, []);
});

function menuLabels(p: any) {
  const S: any = { projects: [p], skiffAgent: { folder: "/skipper" }, focused: null, selection: [], groups: [], closedProjects: [] };
  let items: any[] = [];
  const el = () => ({ append() {}, appendChild() {}, setAttribute() {}, addEventListener() {}, classList: { add() {} }, dataset: {} });
  // As the real launch entries: the Skipper first.
  const entries = (q: any) => [m.skipperStart(q)].filter(Boolean);
  const m: any = moduleFromSource(
    "../src/workspace/menus.ts",
    withStubs({
      "./model": orStub(model),
      "../app/skiffAgent": orStub({ skiffAgentFolder: () => "/skipper", fleetSkipperOf: () => null, projectSkipperOf: () => null }),
      "../app/state": { S, sessions: new Map(), removing: new Set(), removeErrors: new Map(), selectedWorktree: new Map() },
      "../app/stateQueries": orStub({ place: () => null }),
      "../ui/dom": { h: el, button: el, $: el },
      "../app/panels": { launchMenu: { entries } },
      "./tree": orStub({ bareWorktree: (q: any) => q.worktrees[0] }),
      "./changes": orStub({ showsChanges: () => p.kind !== "folder", hasChanges: () => false }),
      "../ui/contextMenu": { ctxMenu: { open: (_x: number, _y: number, _t: string, list: any[]) => (items = list) } },
    }),
    { console, localStorage: { getItem: () => null, setItem() {} } },
    "",
  );
  m.projectMenu(p, 0, 0);
  const sidebar = [...items.find((i) => i.label === "Sidebar").sub].map((i: any) => i.label);
  return { top: items.filter((i) => i.label).map((i) => i.label), sidebar };
}

test("a plain folder's project menu has no New worktree… and no Changes switch", () => {
  const plain = menuLabels(folder);
  assert.ok(!plain.top.includes("New worktree…"));
  assert.deepEqual(plain.sidebar, ["Files"]);
  assert.ok(plain.top.includes("Start project Skipper"));
  const git = menuLabels(repo);
  assert.ok(git.top.includes("New worktree…"));
  assert.deepEqual(git.sidebar, ["Changes", "Files"]);
});

test("Files follows the Host's row: on by default for a plain folder, and a stored choice wins", async () => {
  const calls: any[] = [];
  const files = moduleFromSource(
    "../src/workspace/files.ts",
    withStubs({
      "./model": model,
      "../app/hostCaps": { hostCan: () => true, hostInvoke: async (cap: string, cmd: string, args: any) => (calls.push([cap, cmd, args]), null) },
      "../app/stored": { storedSet: () => new Set() },
    }),
    { console, localStorage: { getItem: () => null, setItem() {} } },
    "",
  );
  // No `files` key: the Host sends the kind's default.
  files.filesFrom([{ ...folder, files: true }, repo]);
  assert.equal(files.showsFiles(folder), true);
  assert.equal(files.showsFiles(repo), false);
  // Turned off: the Host writes false, and its next rows say so.
  await files.setShowFiles(folder, false);
  assert.deepEqual(calls.map((c) => [c[0], c[1], c[2].on]), [["project_config", "set_project_files", false]]);
  assert.equal(files.showsFiles(folder), false);
  files.filesFrom([folder, { ...repo, files: true }]);
  assert.equal(files.showsFiles(folder), false);
  assert.equal(files.showsFiles(repo), true);
});

test("the launch menu starts in the branch, DETACHED with no branch, or a plain folder's name", () => {
  const launch = moduleFromSource("../src/ui/launchMenu.ts", withStubs({ "../workspace/model": model }), {}, "");
  assert.equal(launch.startIn(repo.worktrees[0]), "START IN MAIN");
  assert.equal(launch.startIn({ ...repo.worktrees[0], branch: null }), "START IN DETACHED");
  assert.equal(launch.startIn(folder.worktrees[0]), "START IN DOCS");
});
