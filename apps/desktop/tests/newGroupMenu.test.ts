import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** The real menus module; the launch menu's layouts are two named fakes. */
function page(offered: string[]) {
  const shown: { title: string; entries: any[] }[] = [];
  const invoke = async (cmd: string) => {
    if (cmd === "host_capabilities") return offered;
    throw new Error(`unexpected ${cmd}`);
  };
  const core = { "@tauri-apps/api/core": { invoke } };
  const g = { Promise, Set, Map, Error, JSON };
  const hostCaps = moduleFromSource("../src/app/hostCaps.ts", core, g, "");
  const state = { S: { skiffAgent: null, agents: [], projects: [] }, sessions: new Map(), upsert: () => {}, selectedWorktree: new Map(), removeErrors: new Map(), removing: new Set() };
  const hosts = { isRemote: () => false, hostName: () => "This computer", checkShownHost: () => true };
  const skiffAgent = moduleFromSource("../src/app/skiffAgent.ts", { ...core, "./hostCaps": hostCaps, "./state": state, "./hosts": hosts, "./keys": { setSkiffAgentName: () => {} } }, g, "");
  const split: string[] = [];
  const groups = (_p: any, w: any) => ["Columns", "Grid"].map((name) => ({ label: name, run: () => split.push(`${w.path} ${name}`) }));
  const menus = moduleFromSource("../src/workspace/menus.ts", {
    ...core,
    "../app/hostCaps": hostCaps,
    "../app/skiffAgent": skiffAgent,
    "../app/state": state,
    "../app/hosts": hosts,
    "../app/keys": { keyLabel: () => "" },
    "../ui/contextMenu": { ctxMenu: { open: (_x: number, _y: number, title: string, entries: any[]) => shown.push({ title, entries }) } },
    "./changes": { showsChanges: () => false, hasChanges: () => false },
    "./files": { showsFiles: () => false },
    "./projectClose": { closeEntries: () => [] },
    // One worktree: its start choices, New group among them, come from the launch menu.
    "./tree": { bareWorktree: (p: any) => (p.worktrees.length === 1 ? p.worktrees[0] : null) },
    "../app/panels": {
      launchMenu: {
        entries: (p: any, w: any) => [{ head: "START IN MAIN" }, { label: "Shell" }, { label: "New group", sub: groups(p, w) }],
        groups,
        open: () => {},
      },
    },
    "./profile": { projectWorkspaceEntries: () => [] },
    "./model": { isFolder: () => false, branchName: (w: any) => w.branch },
  }, { ...g, document: { querySelectorAll: () => [] } }, "");
  return { menus, hostCaps, shown, split };
}

const LOCAL = ["session_inventory@1", "session_lifecycle@1", "session_groups@1", "projects@1", "project_config@1", "worktrees@1"];
const main = { path: "/w", branch: "main", is_main: true };
const feature = { path: "/w-feature", branch: "feature", is_main: false };
const top = (entries: any[]) => Array.from(entries).filter((e: any) => e.label);

test("a branch's menu has New group beside New session, with the layouts", async () => {
  const p = page(LOCAL);
  await p.hostCaps.loadHostCaps();
  p.menus.worktreeMenu({ name: "web", path: "/w", worktrees: [main, feature] }, feature, 0, 0);
  const entries = top(p.shown.at(-1)!.entries);
  assert.deepEqual(entries.slice(0, 2).map((e) => e.label), ["New session…", "New group"]);
  const group = entries[1];
  assert.equal(group.disabled, false);
  assert.deepEqual(group.sub.map((e: any) => e.label), ["Columns", "Grid"]);
  group.sub[1].run();
  assert.deepEqual(p.split, ["/w-feature Grid"]);
});

test("a project with several branches has New group at the top: the branch, then its layouts", async () => {
  const p = page(LOCAL);
  await p.hostCaps.loadHostCaps();
  p.menus.projectMenu({ name: "web", path: "/w", worktrees: [main, feature] }, 3, 4);
  const entries = top(p.shown.at(-1)!.entries);
  const group = entries.find((e) => e.label === "New group");
  assert.ok(group, entries.map((e) => e.label).join(", "));
  assert.ok(!entries.find((e) => e.label === "New session in")?.sub.some((e: any) => e.label === "New group"), "buried under New session");
  assert.deepEqual(group.sub.map((e: any) => e.label), ["main", "feature"]);
  group.sub[1].run();
  const layouts = p.shown.at(-1)!;
  assert.equal(layouts.title, "feature");
  assert.deepEqual(layouts.entries.map((e: any) => e.label), ["Columns", "Grid"]);
});

test("a project with one branch shows New group once, at the top, with its layouts", async () => {
  const p = page(LOCAL);
  await p.hostCaps.loadHostCaps();
  p.menus.projectMenu({ name: "web", path: "/w", worktrees: [main] }, 0, 0);
  const groups = top(p.shown.at(-1)!.entries).filter((e) => e.label === "New group");
  assert.equal(groups.length, 1);
  assert.deepEqual(groups[0].sub.map((e: any) => e.label), ["Columns", "Grid"]);
});

test("New group is off where New session is: a project in error, or a Host that starts no sessions", async () => {
  const p = page(LOCAL.filter((c) => c !== "session_lifecycle@1"));
  await p.hostCaps.loadHostCaps();
  p.menus.worktreeMenu({ name: "web", path: "/w", worktrees: [main] }, main, 0, 0);
  assert.equal(top(p.shown.at(-1)!.entries).find((e) => e.label === "New group").disabled, true);
  p.menus.projectMenu({ name: "web", path: "/w", worktrees: [main] }, 0, 0);
  assert.equal(top(p.shown.at(-1)!.entries).find((e) => e.label === "New group").disabled, true);
  const q = page(LOCAL);
  await q.hostCaps.loadHostCaps();
  q.menus.projectMenu({ name: "web", path: "/w", worktrees: [main, feature], error: "gone" }, 0, 0);
  assert.equal(top(q.shown.at(-1)!.entries).find((e) => e.label === "New group").disabled, true);
});
