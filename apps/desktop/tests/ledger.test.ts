import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** M6: the real Ledger panel and Skipper row logic for one Host. Rows and
 *  results come from the Host's `ledger` capability, settings from its
 *  `skipper` capability; nothing names or opens a path here. A remote Host
 *  starts its own Skipper and watcher, only through `skipper_run` (#166 R7). */

function ledger(caps: string[], reply?: () => Promise<unknown>, epoch = () => 0) {
  const calls: string[] = [];
  const shown: string[] = [];
  const mod = moduleFromSource(
    "../src/workspace/ledger.ts",
    {
      "@tauri-apps/api/core": { invoke: (cmd: string) => calls.push(`invoke ${cmd}`) },
      "@tauri-apps/api/event": { listen: () => Promise.resolve(() => {}) },
      "../app/hostCaps": {
        hostInvoke: (cap: string, cmd: string, args: unknown) => {
          if (!caps.includes(cap)) return Promise.reject(new Error(`This Host does not offer ${cap}.`));
          calls.push(`${cap} ${cmd} ${JSON.stringify(args ?? null)}`);
          return reply ? reply() : Promise.resolve([ROW]);
        },
      },
      "../app/dataPlane": { dataEpoch: () => epoch() },
      "../terminal/peek": {
        openLedgerResult: (uid: string, title: string) => shown.push(`${uid} ${title}`),
        openPath: (p: string) => shown.push(`path ${p}`),
        previewToken: () => 1,
      },
      "../app/renderRequest": { render() {} },
      "../app/state": { S: {}, sessions: new Map() },
      "./model": { taskTitle: () => "" },
    },
    { Promise, JSON, Map, Set, document: { getElementById: () => null, addEventListener() {} } },
    "\nexports.rowsNow = () => rows; exports.errorNow = () => error; exports.taskActs = taskActs; exports.result = result;",
  );
  return { mod, calls, shown };
}

const ROW = {
  id: 1, uid: "a1b2c3d4", kind: "task", parent: null, pane_id: null, pane_gen: null, project: "", task: "Write the notes. Then stop.",
  sent_at: "x", title: null, state: "done", updated_at: "x", closed_at: null, result_exists: true,
};

test("the panel reads its rows from the selected Host's ledger, never this Mac's file", async () => {
  const t = ledger(["ledger"]);
  await t.mod.loadLedger();
  assert.deepEqual(t.calls, ["ledger ledger_rows null"]);
  assert.equal(t.mod.rowsNow().length, 1);
  assert.equal(t.mod.errorNow(), null);
});

test("a Host without the ledger shows why, with no rows and no local fallback", async () => {
  const t = ledger([]);
  await t.mod.loadLedger();
  assert.deepEqual(t.calls, []);
  assert.equal(t.mod.rowsNow().length, 0);
  assert.match(String(t.mod.errorNow()), /does not offer ledger/);
});

test("a result opens in the page's viewer by its row's uid; no path reaches an opener", () => {
  const t = ledger(["ledger"]);
  t.mod.result(ROW)();
  assert.deepEqual(t.shown, ["a1b2c3d4 Write the notes"]);
  // A result the Host cannot show gives no action at all.
  assert.equal(t.mod.result({ ...ROW, result_exists: false }), null);
  // A done task's click is its result.
  const acts = t.mod.taskActs({ r: ROW, st: "done", pane: null, where: "" }, false);
  acts.click();
  assert.equal(t.shown.length, 2);
  assert.ok(!t.shown.some((s) => s.startsWith("path ")));
});

function skipper(remote: boolean, clock = { now: 0 }, caps = ["skipper", "skipper_run"]) {
  const calls: string[] = [];
  const errors: string[] = [];
  const S: any = { skiffAgent: { enabled: true, folder: "/home/me/skiff-agent", name: null, continue_on_restore: true }, agents: [] };
  const coordinator = { id: "s1", coordinator: true, state: "idle", resume: true, input_gen: 0 };
  const sessions = new Map<string, any>([["s1", coordinator]]);
  const mod = moduleFromSource(
    "../src/app/skiffAgent.ts",
    {
      "@tauri-apps/api/core": { invoke: (cmd: string) => (calls.push(`invoke ${cmd}`), Promise.resolve(null)) },
      "./hostCaps": {
        hostCan: (cap: string) => caps.includes(cap),
        hostInvoke: (cap: string, cmd: string) => (calls.push(`${cap} ${cmd}`), Promise.resolve(cmd === "get_skiff_agent" ? { ...S.skiffAgent, tasks: [] } : null)),
      },
      "./hosts": { isRemote: () => remote, hostName: () => "devbox", checkShownHost() {} },
      "./state": { S, sessions, upsert() {} },
      "./keys": { setSkiffAgentName() {} },
      "./renderRequest": { render() {} },
      "../ui/alerts": { showError: (e: unknown) => errors.push(String(e)) },
      "../appearance/agentIcon": { agentKind: () => "shell" },
      "../workspace/selection": { keepRow() {} },
      "../workspace/view": { revealSession() {} },
      "../terminal/peek": { openPath: (p: string) => calls.push(`open ${p}`) },
    },
    { Promise, JSON, Map, Set, Date: { now: () => clock.now }, window: { setTimeout: () => 0 }, clearTimeout() {}, requestAnimationFrame: (f: () => void) => f() },
    "",
  );
  return { mod, calls, errors, S, sessions, coordinator };
}

test("Skipper's settings come from the Host on This Mac and on a remote Host", async () => {
  for (const remote of [false, true]) {
    const t = skipper(remote);
    await t.mod.loadSkiffAgent();
    assert.deepEqual(t.calls, ["skipper get_skiff_agent"]);
  }
});

test("a remote Host starts its own Skipper, restore and watcher, only through skipper_run", async () => {
  const t = skipper(true);
  t.S.agents = [{ id: "claude", installed: true, enabled: true }];
  t.mod.syncSkiffAgent();
  await flush();
  // The restored shell continues there; the watcher is the Host's to keep.
  assert.deepEqual(t.calls, ["skipper_run open_skiff_agent"]);
  t.mod.startSkiffWatch();
  t.mod.openSkiffAgent();
  await flush();
  assert.deepEqual(t.calls.slice(1), ["skipper_run start_skiff_watch", "skipper_run open_skiff_agent"]);
  assert.deepEqual(t.calls.filter((c) => c.startsWith("invoke")), [], "nothing of This Mac's");
  // No session there: the row says so, and its Start Admiral starts it.
  t.sessions.clear();
  t.mod.openSkiffAgent();
  assert.equal(t.errors.at(-1), "Admiral does not run on devbox.");
  // The status names the Host, never that this app tries.
  const text = t.mod.watchText("missing", t.coordinator, "Skipper", null);
  assert.equal(text, "No watcher runs for Skipper on devbox. Skipper does not hear when its panes finish.");
});

test("a remote Host without skipper_run is asked for nothing, and the row says to update it", async () => {
  const t = skipper(true, { now: 0 }, ["skipper"]);
  t.S.agents = [{ id: "claude", installed: true, enabled: true }];
  t.sessions.clear();
  t.mod.syncSkiffAgent();
  t.mod.openSkiffAgent();
  await flush();
  assert.deepEqual(t.calls, []);
  assert.match(t.errors.at(-1) ?? "", /devbox cannot start Admiral\. Its Skiff is too old\./);
});

test("This Mac keeps starting its own watcher and restoring Skipper", async () => {
  const t = skipper(false);
  t.mod.syncSkiffAgent();
  await flush();
  assert.ok(t.calls.includes("invoke start_skiff_watch"));
  assert.ok(t.calls.includes("invoke open_skiff_agent"));
  assert.match(t.mod.watchText("missing", t.coordinator, "Skipper", null), /tries to start/);
});

/** A reply the test settles by hand. */
function later<T>() {
  let settle!: { ok: (v: T) => void; fail: (e: unknown) => void };
  const p = new Promise<T>((ok, fail) => (settle = { ok, fail }));
  return { p, ...settle };
}

test("review M6 P2: only the latest ledger read shows, whichever reply comes last", async () => {
  const OLD = { ...ROW, uid: "0ld00000" };
  const NEW = { ...ROW, uid: "new00000" };
  // An older success after a newer one, and an older failure after a newer success.
  for (const older of [(r: ReturnType<typeof later>) => r.ok([OLD]), (r: ReturnType<typeof later>) => r.fail(new Error("read failed"))]) {
    const replies = [later<unknown>(), later<unknown>()];
    let n = 0;
    const t = ledger(["ledger"], () => replies[n++].p);
    const first = t.mod.loadLedger();
    const second = t.mod.loadLedger();
    replies[1].ok([NEW]);
    await second;
    older(replies[0]);
    await first;
    assert.deepEqual(t.mod.rowsNow().map((r: any) => r.uid), ["new00000"]);
    assert.equal(t.mod.errorNow(), null);
  }
});

test("review M6 P2: a ledger reply from an older data connection does not show", async () => {
  let epoch = 1;
  const reply = later<unknown>();
  const t = ledger(["ledger"], () => reply.p, () => epoch);
  const read = t.mod.loadLedger();
  epoch = 2;
  reply.ok([ROW]);
  await read;
  assert.equal(t.mod.rowsNow().length, 0, "the old connection's rows");
  // The new connection's own read shows.
  const fresh = ledger(["ledger"], undefined, () => epoch);
  await fresh.mod.loadLedger();
  assert.equal(fresh.mod.rowsNow().length, 1);
});

test("review M6 P2: a remote Host's missing watcher shows as missing once the grace ends, and this app starts none", async () => {
  const clock = { now: 0 };
  const t = skipper(true, clock);
  t.coordinator.watch = undefined;
  t.mod.noteDaemon({ connected: true, protocol: 40 });
  t.mod.syncSkiffAgent();
  assert.equal(t.mod.skiffWatch(t.coordinator).state, "starting");
  for (const at of [60_000, 120_000]) {
    clock.now = at;
    t.mod.syncSkiffAgent();
    const w = t.mod.skiffWatch(t.coordinator);
    assert.equal(w.state, "missing", `${at / 1000} s`);
    assert.match(w.text, /^No watcher runs for Admiral on devbox\./);
  }
  await flush();
  assert.deepEqual(t.calls.filter((c) => c.startsWith("invoke") || c.endsWith("start_skiff_watch")), [], "no start_skiff_watch, here or there");
  // A reconnect starts a new grace.
  t.mod.noteDaemon({ connected: true, protocol: 40 });
  assert.equal(t.mod.skiffWatch(t.coordinator).state, "starting");
});
