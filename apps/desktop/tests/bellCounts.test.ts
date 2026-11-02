import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** The bell means one thing: a session waits, as the top bar's Waiting counts
 *  it. A project Skipper's tasks that need the person are an amber count on
 *  its row, as on the Skiff agent's row. A task never sent needs nobody when
 *  its pane goes. */

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
