import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real daemon module's remote recovery. The Host, its link and the
 *  page around it are fakes; the Host's agent list can change between
 *  connections, as it does when its daemon restarts with a new config. */
function recovery() {
  const S: any = { agents: [{ id: "agent", command: "agent --old", enabled: true }], groups: [] };
  const sessions = new Map();
  const host = { agents: [{ id: "agent", command: "agent --new", enabled: true }], fail: "" };
  const calls: string[] = [];
  const created: any[] = [];
  let agentsDuringConnect: unknown = null;
  const hostInvoke = async (_cap: string, cmd: string, args: any) => {
    calls.push(cmd);
    if (cmd === host.fail) throw new Error(`${cmd} failed`);
    if (cmd === "list_sessions") return [];
    if (cmd === "list_agents") return host.agents.map((a) => ({ ...a }));
    if (cmd === "create_session") {
      created.push(args.spec);
      return { id: "s1" };
    }
  };
  const el = { querySelector: () => ({}), replaceChildren() {} };
  const mod = moduleFromSource(
    "../src/app/daemon.ts",
    {
      "./connectPanel": { connectAttempt() {}, connectDone() {}, connectFailed() {}, connectWait: async () => {} },
      "@tauri-apps/api/core": {
        invoke: async () => {
          agentsDuringConnect = [...S.agents.map((a: any) => a.command)];
          return { connected: true };
        },
        Channel: class {},
      },
      "@tauri-apps/api/event": { listen: async () => {} },
      "../ui/dom": { $: () => el },
      "./daemonRestart": { daemonBadge() {} },
      "./skiffAgent": { noteDaemon() {} },
      "./hosts": { isRemote: () => true, hostName: () => "devbox" },
      "./render": { render() {}, scheduleRender() {} },
      "./state": { S, sessions, born: new Set(), gone: new Set(), upsert: (s: any) => sessions.set(s.id, s) },
      "./seen": { quietSeen() {} },
      "../terminal/terminalRuntime": { muteInput() {}, reattachPanes() {} },
      "./groups": { loadGroups: async () => {} },
      "../terminal/peek": { previewToken: () => "" },
      "../terminal/terminalState": { panes: new Map() },
      "../workspace/view": { showSingle() {} },
      "../appearance/agentNames": { agentCallsign: () => "Ada" },
      "./hostCaps": { loadHostCaps: async () => {}, hostCan: () => true, hostInvoke },
    },
    { window: { addEventListener() {} }, document: { hasFocus: () => true }, Promise, Map, Set, console },
    "",
  );
  return { mod, S, sessions, host, calls, created, agentsDuringConnect: () => agentsDuringConnect };
}

/** A remote New session menu opened before the link dropped, on the real
 *  daemon, stateQueries and remoteLaunch modules. `link.up` is the shown
 *  Host's link as hosts.ts reports it; `errors` holds what the page showed. */
function openMenu(r: ReturnType<typeof recovery>) {
  const link = { up: true };
  const errors: string[] = [];
  const queries = moduleFromSource(
    "../src/app/stateQueries.ts",
    { "./state": { S: r.S, sessions: r.sessions } },
    { Map, Set },
    "",
  );
  const menus: any[][] = [];
  const launch = moduleFromSource(
    "../src/workspace/remoteLaunch.ts",
    {
      "../ui/contextMenu": { ctxMenu: { open: (_x: number, _y: number, _t: string, entries: any[]) => menus.push(entries) } },
      "../ui/alerts": { showError: (e: unknown) => void errors.push(String(e)) },
      "../app/daemon": r.mod,
      "../app/hosts": { hostName: () => "devbox", hostConnected: () => link.up },
      "../app/state": { S: r.S, sessions: r.sessions },
      "../app/stateQueries": queries,
      "../appearance/agentIcon": { launchIcon: () => ({}) },
    },
    { Set, Map, Promise },
    "",
  );
  launch.openRemoteNewSession(0, 0);
  // Home folder › Shell, agent: the callbacks the open menu keeps.
  const [shell, agent] = menus[0].at(-2).sub;
  assert.equal(agent.label, "agent");
  return { link, errors, shell, agent };
}

/** The link drops and recovery runs, as hosts.ts drives it. */
async function dropAndRecover(r: ReturnType<typeof recovery>, link: { up: boolean }) {
  link.up = false;
  await r.mod.reconnectRemote();
  link.up = true;
}

test("a launch menu left open across a recovery starts the command the Host lists now", async () => {
  const r = recovery();
  const m = openMenu(r);
  await dropAndRecover(r, m.link);
  m.agent.run();
  m.shell.run();
  await flush();
  assert.deepEqual(m.errors, []);
  assert.equal(r.created[0].command, "agent");
  assert.deepEqual([...r.created[0].args], ["--new"]);
  assert.equal(r.created[1].role, "shell");
  assert.equal(r.created[1].command, null);
});

test("a launch menu left open starts nothing while the link is down", async () => {
  const r = recovery();
  const m = openMenu(r);
  m.link.up = false;
  m.agent.run();
  m.shell.run();
  await flush();
  assert.deepEqual(r.created, []);
  assert.equal(m.errors.length, 2);
  assert.match(m.errors[0], /devbox is not connected/);
});

test("a launch menu left open starts nothing after a failed recovery", async () => {
  const r = recovery();
  const m = openMenu(r);
  r.host.fail = "list_agents";
  m.link.up = false;
  await assert.rejects(r.mod.reconnectRemote(), /list_agents failed/);
  m.agent.run();
  await flush();
  assert.equal(r.created.length, 0);
  // Even with the link marked up, no agent from before is left to start.
  m.link.up = true;
  m.agent.run();
  await flush();
  assert.equal(r.created.length, 0);
  assert.match(m.errors.at(-1)!, /devbox no longer offers agent/);
});

for (const [what, change] of [
  ["removed", (agents: any[]) => agents.splice(0)],
  ["turned off", (agents: any[]) => (agents[0].enabled = false)],
] as const) {
  test(`a launch menu left open does not start an agent the Host has ${what}`, async () => {
    const r = recovery();
    const m = openMenu(r);
    change(r.host.agents);
    await dropAndRecover(r, m.link);
    m.agent.run();
    await flush();
    assert.deepEqual(r.created, []);
    assert.match(m.errors[0], /devbox no longer offers agent/);
  });
}

test("a reconnect reads the Host's agents again, and a launch runs the new command", async () => {
  const r = recovery();
  await r.mod.reconnectRemote();
  // While the link was down, the old commands were no longer offered.
  assert.deepEqual(r.agentsDuringConnect(), []);
  assert.equal(r.calls.filter((c) => c === "list_agents").length, 1);
  assert.deepEqual([...r.S.agents.map((a: any) => a.command)], ["agent --new"]);
  // The launch menu passes the listed command to newSession.
  await r.mod.newSession("/srv", r.S.agents[0].command, "agent", "agent");
  await flush();
  assert.equal(r.created[0].command, "agent");
  assert.deepEqual([...r.created[0].args], ["--new"]);
});

test("a recovery that cannot read the agents fails, and offers none from before", async () => {
  const r = recovery();
  r.host.fail = "list_agents";
  await assert.rejects(r.mod.reconnectRemote(), /list_agents failed/);
  assert.deepEqual([...r.S.agents], []);
});
