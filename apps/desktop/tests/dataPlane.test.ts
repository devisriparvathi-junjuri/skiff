import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** The real dataPlane module over a fake `data_plane_status`, as Rust
 *  answers it: the page reads projects and Git again only for a new data
 *  connection, says why when it is lost, and waits while skiffd is down. */
function plane() {
  let status: any = null;
  const mod = moduleFromSource(
    "../src/app/dataPlane.ts",
    { "@tauri-apps/api/core": { invoke: async (cmd: string) => (cmd === "data_plane_status" ? status : undefined) } },
    { window: { setTimeout() {}, clearTimeout() {} }, Promise },
    "",
  );
  const seen: string[] = [];
  let fresh = true;
  /** What each reload answers: false, a read failed. */
  const answers: boolean[] = [];
  const hooks = {
    reload: async () => (seen.push(`reload@${mod.dataEpoch()}`), answers.shift() ?? true),
    down: (m: string | null) => void seen.push(`down:${m}@${mod.dataEpoch()}`),
    fresh: () => fresh,
  };
  const set = (s: Partial<Record<string, unknown>>) => {
    status = { ready: false, waiting: false, host_id: null, instance_id: null, connection: 0, capabilities: [], message: null, ...s };
  };
  const check = () => mod.checkDataPlane(hooks);
  return { mod, set, check, seen, answers, notFresh: () => (fresh = false) };
}

const ready = (connection: number, instance = "i1", host = "H") => ({ ready: true, host_id: host, instance_id: instance, connection, capabilities: ["projects@1", "git_status@1", "git_diff@1"] });

test("a first ready check after a good boot read reads nothing again; a restart reads everything again", async () => {
  const p = plane();
  p.set(ready(1));
  await p.check();
  assert.deepEqual(p.seen, []);
  // Polled again on the same connection: nothing.
  await p.check();
  assert.deepEqual(p.seen, []);
  // skiff-host restarted: a new connection and instance, a new epoch, one reload.
  p.set(ready(2, "i2"));
  await p.check();
  assert.deepEqual(p.seen, ["reload@1"]);
});

test("a service that starts after launch is read as soon as it answers, without a page reload", async () => {
  const p = plane();
  p.notFresh();
  p.set({ message: "skiff-host does not run on devbox, so its projects and Git are not shown." });
  await p.check();
  assert.deepEqual(p.seen, ["down:skiff-host does not run on devbox, so its projects and Git are not shown.@1"]);
  // The same cause again says nothing new.
  await p.check();
  assert.equal(p.seen.length, 1);
  p.set(ready(1));
  await p.check();
  assert.deepEqual(p.seen.at(-1), "reload@2");
});

test("while skiffd is down the data plane waits, and the link coming back reads again", async () => {
  const p = plane();
  p.set(ready(1));
  await p.check();
  p.set({ waiting: true });
  await p.check();
  assert.deepEqual(p.seen, [], "a session-plane outage is not a data-plane loss");
  // The forward came back with the link: a new data connection.
  p.set(ready(2));
  await p.check();
  assert.deepEqual(p.seen, ["reload@1"]);
});

test("each change of data connection or Host is a new epoch, so older replies are not shown", () => {
  const { mod } = plane();
  const base = { waiting: false, instance_id: "i", capabilities: [], message: null };
  const s = (o: any) => ({ ...base, ready: true, host_id: "H", connection: 1, ...o });
  assert.equal(mod.dataChange(s({}), s({})), null);
  assert.equal(mod.dataChange(s({}), s({ connection: 2 })), "reload");
  assert.equal(mod.dataChange(s({}), s({ host_id: "other" })), "reload");
  assert.equal(mod.dataChange(s({}), s({ ready: false, message: "lost" })), "down");
  assert.equal(mod.dataChange(s({ ready: false, message: "a" }), s({ ready: false, message: "b" })), "down");
  assert.equal(mod.dataChange(s({}), s({ ready: false, waiting: true })), null);
});

test("review M4 P2: a reload that failed is tried again on the same connection, until one finishes", async () => {
  const p = plane();
  p.notFresh();
  p.set(ready(1));
  // The first read fails (a project or capability error), the second finishes.
  p.answers.push(false, true);
  await p.check();
  assert.deepEqual(p.seen, ["reload@1"]);
  // The same live connection: the read is tried again, in the same epoch, then never again.
  for (let i = 0; i < 10; i++) await p.check();
  assert.deepEqual(p.seen, ["reload@1", "reload@1"]);
});
