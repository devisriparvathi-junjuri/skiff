import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

// The real skiffAgent.ts and daemon.ts, with the view reduced to one shown pane.
// `remote`: the shown Host is a remote one that offers fleet Skippers.
function app(projects: { name: string; path: string; fleet: string | null }[], remote: boolean) {
  const S: any = { projects, skiffAgent: { folder: "/skipper" }, focused: null, single: null, groups: [], selection: [] };
  const sessions = new Map<string, any>();
  const upsert = (s: any) => sessions.set(s.id, { ...sessions.get(s.id), ...s });
  const calls: Record<string, (args: any) => unknown> = {};
  const invoke = (cmd: string, args: any) => Promise.resolve(calls[cmd]?.(args));
  const shownIds = () => (S.single ? [S.single] : []);
  const show = (id: string) => {
    S.single = id;
    S.focused = id;
  };
  const place = (s: any) => {
    const project = projects.find((p) => s.cwd.startsWith(p.path));
    return project ? { project, worktree: { path: project.path } } : null;
  };
  const globals = {
    requestAnimationFrame: (run: () => void) => run(),
    document: { hasFocus: () => true, addEventListener() {} },
    window: { addEventListener() {} },
    console,
  };
  const agent = moduleFromSource("../src/app/skiffAgent.ts", {
    "@tauri-apps/api/core": { invoke },
    "./state": { S, sessions, upsert },
    "./stateQueries": { place },
    "./hostCaps": { hostCan: () => true, hostInvoke: (_cap: string, cmd: string, args: any) => invoke(cmd, args) },
    "./hosts": { isRemote: () => remote },
    "../workspace/selection": { keepRow() {} },
    "../workspace/view": { revealSession: show },
    "../ui/dialogParts": { actionDialog: (o: any) => (o.submit(), o.onClose()) },
    "../ui/dom": { h: () => ({ append() {} }) },
    "../ui/alerts": { showError: (e: unknown) => assert.fail(String(e)) },
  }, globals, "");
  const daemon = moduleFromSource("../src/app/daemon.ts", {
    "@tauri-apps/api/core": { invoke },
    "@tauri-apps/api/event": { listen: () => Promise.resolve() },
    "./state": { S, sessions, upsert, born: [], gone: new Set() },
    "./stateQueries": { shownIds, place, activeGroupObj: () => null },
    "./skiffAgent": agent,
    "../workspace/view": {
      inBackground: (run: () => void) => run(),
      revealSession: show,
      focusPane: show,
      // The old fallback: the next session row, which is unrelated here.
      rowNear: () => "other",
      unfocus() {
        S.focused = null;
      },
    },
    "../workspace/layout": { sessionsOf: () => [] },
    "../terminal/terminalState": { panes: new Map() },
    "../terminal/peek": { peekExited: () => false },
    "../terminal/terminal": { ptySized() {} },
    "./daemonRestart": { restartIfIdle() {} },
    "./render": { render() {}, scheduleRender() {} },
  }, globals, "");
  const add = (s: any) => daemon.onEvent({ event: "session_created", session: { state: "idle", ...s } });
  const end = (id: string) => daemon.onEvent({ event: "session_removed", session: id });
  return { S, sessions, calls, agent, add, end, show };
}

const harbor = () => [
  { name: "web", path: "/harbor/web", fleet: "Harbor" as string | null },
  { name: "api", path: "/harbor/api", fleet: null as string | null },
  { name: "gesher", path: "/gesher", fleet: null as string | null },
];

for (const remote of [false, true]) {
  const on = remote ? "a remote Host" : "this computer";

  test(`Start fleet Skipper over a selected project Skipper selects the fleet Skipper, on ${on}`, async () => {
    const a = app(harbor(), remote);
    a.add({ id: "web-skipper", name: "web Skipper", skipper: "web", cwd: "/harbor/web" });
    a.add({ id: "other", name: "gesher", cwd: "/gesher" });
    a.show("web-skipper");
    let started!: (id: string) => void;
    a.calls.fleet_takeover_preview = () => [{ project: "web", session: "web-skipper", tasks: [], handoffs: [] }];
    a.calls.start_fleet_skipper = () => new Promise((resolve) => (started = resolve));
    const done = a.agent.startFleetSkipper("Harbor");
    await flush();
    // skiffd adds the fleet Skipper, then retires the project Skipper, which ends
    // at once: the start is still waiting for the new agent's input box.
    a.add({ id: "fleet-skipper", name: "Harbor Skipper", skipper: "fleet:Harbor", cwd: "/skipper/fleets/Harbor" });
    assert.equal(a.S.focused, "fleet-skipper");
    Object.assign(a.sessions.get("web-skipper"), { skipper: null, retiring: true });
    a.end("web-skipper");
    assert.equal(a.S.focused, "fleet-skipper");
    started("fleet-skipper");
    await done;
    assert.equal(a.S.single, "fleet-skipper");
  });

  test(`a selected retiring project Skipper that ends later gives the keys to the fleet Skipper, on ${on}`, async () => {
    const a = app(harbor(), remote);
    a.add({ id: "fleet-skipper", name: "Harbor Skipper", skipper: "fleet:Harbor", cwd: "/skipper/fleets/Harbor" });
    a.add({ id: "other", name: "gesher", cwd: "/gesher" });
    // Taken over from a terminal: the app knows it by its project's fleet.
    a.add({ id: "web-skipper", name: "web Skipper", retiring: true, state: "waiting", cwd: "/harbor/web" });
    a.show("web-skipper");
    a.end("web-skipper");
    assert.equal(a.S.focused, "fleet-skipper");

    // Add to fleet…: the project is not in the fleet yet when its Skipper ends.
    a.add({ id: "api-skipper", name: "api Skipper", skipper: "api", state: "working", cwd: "/harbor/api" });
    a.calls.fleet_takeover_preview = () => [{ project: "api", session: "api-skipper", tasks: [], handoffs: [] }];
    a.calls.fleet_takeover = () => undefined;
    a.show("api-skipper");
    assert.equal(await a.agent.joinFleet("api", "Harbor"), true);
    assert.equal(a.S.focused, "fleet-skipper");
    // The person goes back to the retiring one; its turn ends later.
    Object.assign(a.sessions.get("api-skipper"), { skipper: null, retiring: true });
    a.show("api-skipper");
    a.end("api-skipper");
    assert.equal(a.S.focused, "fleet-skipper");

    // Any other session that ends keeps the old fallback.
    a.show("other");
    a.add({ id: "shell", name: "shell", cwd: "/gesher" });
    a.show("shell");
    a.end("shell");
    assert.equal(a.S.focused, "other");
  });
}
