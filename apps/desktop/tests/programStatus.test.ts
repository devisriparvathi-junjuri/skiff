import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** The real module over a Host that offers `program_status@1`, or not. */
function status(offered: boolean) {
  const m = moduleFromSource("../src/workspace/programStatus.ts", {}, {}, "");
  m.offerReports(offered);
  return m;
}

const session = (state: string, unread: boolean, program_status: unknown) =>
  ({ id: "s1", state, unread, program_status }) as any;

test("a waiting program says what it waits for and asks, as plain text", () => {
  const m = status(true);
  const s = session("waiting", false, { state: "blocked", kind: "permission", msg: "Allow Edit to <b>hello.txt</b>?" });
  assert.equal(m.statusNote(s), "asks for permission: Allow Edit to <b>hello.txt</b>?");
  assert.equal(m.stateLabel(s), "waiting, asks for permission: Allow Edit to <b>hello.txt</b>?");
  assert.equal(m.statusNote(session("waiting", false, { state: "blocked", kind: "auth" })), "asks to sign in");
});

test("a turn's end says done or failed until the pane is read", () => {
  const m = status(true);
  assert.equal(m.statusNote(session("idle", true, { state: "done", msg: "Turn complete in 2.4s." })), "finished: Turn complete in 2.4s.");
  const failed = session("idle", true, { state: "error", msg: "boom", ended: true });
  assert.equal(m.reportedFailed(failed), true);
  assert.equal(m.statusNote(failed), "failed: boom");
  // Read: an idle pane shows nothing more.
  assert.equal(m.reportedFailed(session("idle", false, { state: "error" })), false);
  assert.equal(m.statusNote(session("idle", false, { state: "done" })), "");
});

test("work shows its progress or a long silence, not its passing messages", () => {
  const m = status(true);
  assert.equal(m.statusNote(session("working", false, { state: "working", msg: "Thinking" })), "");
  assert.equal(m.statusNote(session("working", false, { state: "working", progress: 40 })), "40% done");
  assert.equal(m.progressOf(session("working", false, { state: "working", progress: 40 })), 40);
  assert.equal(m.statusNote(session("working", false, { state: "working", stalled: true })), "no output for 5 minutes");
  const kids = [{ id: "s1", state: "working" }, { id: "s2", state: "blocked" }];
  assert.equal(m.childrenNote(session("working", false, { state: "working", children: kids })), "1 subtask waits");
  assert.equal(m.childrenNote(session("working", false, { state: "working", children: kids.slice(0, 1) })), "1 subtask");
});

test("a Host without program_status@1 shows nothing of it", () => {
  const m = status(false);
  const s = session("waiting", false, { state: "blocked", kind: "permission", msg: "x" });
  assert.equal(m.statusNote(s), "");
  assert.equal(m.stateLabel(s), "waiting");
  assert.equal(m.progressOf(session("working", false, { state: "working", progress: 5 })), null);
});
