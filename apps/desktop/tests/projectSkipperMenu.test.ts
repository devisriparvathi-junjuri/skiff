import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { moduleFromSource } from "./workspaceHarness.ts";

/** What this app adds for This Mac (connection.rs LOCAL_ADAPTER), read from the source. */
const LOCAL_ADAPTER = [...readFileSync(new URL("../src-tauri/src/connection.rs", import.meta.url), "utf8")
  .match(/const LOCAL_ADAPTER[^=]*= \[([^\]]*)\]/)![1]
  .matchAll(/"([^"]+)"/g)].map((m) => m[1]);

/** An old skiffd and its skiff-host: neither knows skipper_run. */
const OLD_HOST = ["session_inventory@1", "session_screen@1", "session_updates@1", "session_terminal@1", "session_lifecycle@1", "session_groups@1",
  "projects@1", "git_status@1", "project_config@1", "worktrees@1", "files@1", "ledger@1", "skipper@1"];

/** The real menus, hostCaps and skiffAgent modules over a fake `invoke`. */
function page() {
  const S: any = { skiffAgent: null, agents: [], projects: [] };
  const sessions = new Map();
  const shown: any[] = [];
  const invoke = async (cmd: string) => {
    if (cmd === "host_capabilities") return [...OLD_HOST, ...LOCAL_ADAPTER.filter((c) => !OLD_HOST.includes(c))];
    // get_skiff_agent on This Mac reads projects.toml in the app, not skiff-host.
    if (cmd === "get_skiff_agent") return { name: null, agent: null, folder: "/home/me/skiff-agent", continue_on_restore: false, tasks: [] };
    throw new Error(`unexpected ${cmd}`);
  };
  const core = { "@tauri-apps/api/core": { invoke } };
  const g = { Promise, Set, Map, Error, JSON };
  const hostCaps = moduleFromSource("../src/app/hostCaps.ts", core, g, "");
  const state = { S, sessions, upsert: () => {}, selectedWorktree: new Map(), removeErrors: new Map(), removing: new Set() };
  const hosts = { isRemote: () => false, hostName: () => "This computer", checkShownHost: () => true };
  const skiffAgent = moduleFromSource("../src/app/skiffAgent.ts", { ...core, "./hostCaps": hostCaps, "./state": state, "./hosts": hosts, "./keys": { setSkiffAgentName: () => {} } }, g, "");
  const menus = moduleFromSource("../src/workspace/menus.ts", {
    ...core,
    "../app/hostCaps": hostCaps,
    "../app/skiffAgent": skiffAgent,
    "../app/state": state,
    "../app/hosts": hosts,
    "../app/keys": { keyLabel: () => "" },
    "../ui/contextMenu": { ctxMenu: { open: (_x: number, _y: number, _k: string, entries: any[]) => shown.push(entries) } },
    "./changes": { showsChanges: () => false },
    "./files": { showsFiles: () => false },
    "./projectClose": { closeEntries: () => [] },
    // One worktree: its start choices come from the launch menu.
    "./tree": { bareWorktree: (p: any) => p.worktrees[0] },
    // The launch menu lists the Skipper first, from menus.skipperStart, then Shell and the agents.
    "../app/panels": { launchMenu: { entries: (p: any) => [menus.skipperStart(p), { label: "Shell" }].filter(Boolean) } },
    "./profile": { projectWorkspaceEntries: () => [] },
    "./model": { isFolder: (p: any) => p.kind === "folder" },
  }, { ...g, document: { querySelectorAll: () => [] } }, "");
  const labels = () => {
    menus.projectMenu({ name: "web", path: "/w", worktrees: [{ path: "/w", is_main: true }] }, 0, 0);
    return shown.at(-1).map((e: any) => e.label).filter(Boolean);
  };
  return { skiffAgent, hostCaps, labels };
}

test("This Mac with an old skiffd: the project menu offers Start project Skipper", async () => {
  const p = page();
  await p.hostCaps.loadHostCaps();
  await p.skiffAgent.loadSkiffAgent();
  assert.ok(p.hostCaps.hostCan("skipper_run"), "the app starts This Mac's project Skippers itself");
  assert.ok(p.labels().includes("Start project Skipper"), p.labels().join(", "));
});

test("Skippers are always on: the entry needs no setting, and stays after a projects reload", async () => {
  const p = page();
  await p.hostCaps.loadHostCaps();
  await p.skiffAgent.loadSkiffAgent();
  assert.ok(p.labels().includes("Start project Skipper"));
  // set_skiff_agent writes projects.toml and emits skiff:projects-changed; loadProjects reads it again.
  await p.skiffAgent.loadSkiffAgent();
  assert.ok(p.labels().includes("Start project Skipper"));
});

test("fleet Skippers need skipper_run@2: This Mac has it, an older Host does not", async () => {
  const caps = (list: string[]) => {
    const invoke = async (cmd: string) => {
      if (cmd === "host_capabilities") return list;
      throw new Error(`unexpected ${cmd}`);
    };
    return moduleFromSource("../src/app/hostCaps.ts", { "@tauri-apps/api/core": { invoke } }, { Promise, Set, Map, Error, JSON }, "");
  };
  const mac = caps([...OLD_HOST, ...LOCAL_ADAPTER]);
  await mac.loadHostCaps();
  assert.ok(mac.hostCan("skipper_fleets") && mac.hostCan("skipper_run"));
  const old = caps([...OLD_HOST, "skipper_run@1"]);
  await old.loadHostCaps();
  assert.ok(old.hostCan("skipper_run") && !old.hostCan("skipper_fleets"));
  await assert.rejects(old.hostInvoke("skipper_fleets", "start_fleet_skipper", { fleet: "Harbor" }), /does not offer fleet Skippers/);
});
