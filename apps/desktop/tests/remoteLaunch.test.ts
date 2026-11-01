import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real remoteLaunch module. The Host's sessions and groups, its agents
 *  and the start call are fakes; menus and dialogs are recorded. */
function launch(o: { sessions?: { id: string; cwd: string }[]; groups?: { cwd?: string | null }[]; agents?: { id: string; command: string }[] } = {}) {
  const started: unknown[][] = [];
  const menus: { title: string; entries: any[] }[] = [];
  const pickers: any[] = [];
  let refocused = 0;
  const choices = [null, ...(o.agents ?? [])];
  const mod = moduleFromSource(
    "../src/workspace/remoteLaunch.ts",
    {
      "../ui/contextMenu": { ctxMenu: { open: (_x: number, _y: number, title: string, entries: any[]) => menus.push({ title, entries }) } },
      "./folderPicker": { openFolderPicker: (p: any) => pickers.push(p) },
      "../ui/alerts": { showError(e: unknown) { throw e; } },
      "../app/daemon": { newSession: async (...args: unknown[]) => void started.push(args) },
      "../app/hosts": { hostName: () => "devbox", hostConnected: () => true },
      "../app/state": { S: { groups: o.groups ?? [], projects: [] }, sessions: new Map((o.sessions ?? []).map((s) => [s.id, s])) },
      "../app/stateQueries": { launchChoices: () => choices, launchDigit: (i: number) => String(i + 1) },
      "../appearance/agentIcon": { launchIcon: () => ({}) },
      "./model": { basename: (p: string) => p.split("/").pop() },
      "./view": { refocusTerminal: () => refocused++ },
    },
    { Set, Map, Promise },
    "",
  );
  return { mod, started, menus, pickers, refocused: () => refocused };
}

test("the folders are the ones the Host reported, once each, the asked one first", () => {
  const { mod } = launch();
  const list = [{ cwd: "/srv/b" }, { cwd: "/srv/a" }, { cwd: "/srv/b" }];
  assert.deepEqual([...mod.hostFolders(list, [{ cwd: "/srv/c" }, { cwd: null }])], ["/srv/a", "/srv/b", "/srv/c"]);
  assert.deepEqual([...mod.hostFolders(list, [], "/srv/b")], ["/srv/b", "/srv/a"]);
});

test("worktrees and Recent are listed once each, under the folder asked first", () => {
  const { mod } = launch();
  const projects = [{ name: "web", worktrees: [{ path: "/srv/web", branch: "main", is_main: true }, { path: "/srv/web-worktrees/fix", branch: "fix", is_main: false }] }];
  assert.deepEqual([...mod.hostWorktrees(projects).map((w: any) => w.label)], ["web", "web › fix"]);
  assert.deepEqual([...mod.recentFolders(["/srv/a", "/srv/web"], ["/srv/a", "/srv/b"], new Set(["/srv/web"]))], ["/srv/a", "/srv/b"]);
});

test("New session on a remote Host starts the Host's agent in a Host folder", async () => {
  const h = launch({
    sessions: [{ id: "s1", cwd: "/srv/web" }, { id: "s2", cwd: "/srv/api" }],
    groups: [{ cwd: "/srv/ops" }],
    agents: [{ id: "claude", command: "claude --host-side" }],
  });
  h.mod.openRemoteNewSession(10, 20, "/srv/web");
  const menu = h.menus[0];
  assert.equal(menu.title, "New session");
  assert.equal(menu.entries[0].head, "FOLDER ON DEVBOX");
  assert.deepEqual([...menu.entries.slice(1).map((e: any) => e.head ?? e.label ?? "—")], ["/srv/web", "RECENT", "/srv/api", "/srv/ops", "—", "Home folder", "Choose a folder…"]);
  // Shell, then the Host's agents, under each folder.
  const web = menu.entries[1].sub;
  assert.deepEqual([...web.map((e: any) => [e.label, e.hint])], [["Shell", "1"], ["claude", "2"]]);
  web[1].run();
  menu.entries[6].sub[0].run();
  await flush();
  assert.deepEqual(h.started, [
    ["/srv/web", "claude --host-side", "claude", "agent"],
    [null, null, "", "shell"],
  ]);
});

test("Choose a folder… opens the picker, then the start menu; Cancel starts nothing and gives the keys back", async () => {
  const h = launch({ agents: [{ id: "codex", command: "codex" }] });
  h.mod.openRemoteNewSession(0, 0);
  h.menus[0].entries.at(-1).run();
  const p = h.pickers[0];
  assert.equal(p.action, "Start here");
  await p.pick("/srv/λ");
  p.onClose(true);
  assert.equal(h.menus[1].entries[0].head, "NEW IN Λ");
  h.menus[1].entries[2].run();
  await flush();
  assert.deepEqual(h.started, [["/srv/λ", "codex", "codex", "agent"]]);

  const c = launch();
  c.mod.openRemoteNewSession(0, 0);
  c.menus[0].entries.at(-1).run();
  c.pickers[0].onClose(false);
  assert.equal(c.menus.length, 1);
  assert.equal(c.refocused(), 1);
  assert.deepEqual(c.started, []);
});
