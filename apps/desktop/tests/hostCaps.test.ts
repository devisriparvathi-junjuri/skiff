import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** The real hostCaps module over a fake `invoke` that records each call. */
function caps(offered: string[] | (() => Promise<string[]>)) {
  const calls: [string, unknown][] = [];
  const invoke = async (command: string, args?: unknown) => {
    calls.push([command, args]);
    if (command === "host_capabilities") return typeof offered === "function" ? offered() : offered;
    return `${command} done`;
  };
  const mod = moduleFromSource("../src/app/hostCaps.ts", { "@tauri-apps/api/core": { invoke } }, { Promise, Set, Error }, "");
  return { mod, calls };
}

const LOCAL = ["connection_status@1", "session_screen@1", "session_inventory@1", "session_updates@1", "session_terminal@1", "session_lifecycle@1", "session_groups@1"];

test("a call the selected Host offers goes through with its exact arguments", async () => {
  const { mod, calls } = caps(LOCAL);
  // The first call asks the Host what it offers, then sends.
  assert.equal(await mod.hostInvoke("session_lifecycle", "kill_session", { session: "s1" }), "kill_session done");
  assert.deepEqual(calls, [["host_capabilities", undefined], ["kill_session", { session: "s1" }]]);
  for (const cap of ["session_inventory", "session_screen", "session_updates", "session_terminal", "session_lifecycle", "session_groups"]) {
    assert.equal(mod.hostCan(cap), true, cap);
  }
});

test("a capability the Host lacks, or offers at another major, is refused before any call", async () => {
  const { mod, calls } = caps(["session_terminal@1", "session_lifecycle@2"]);
  await mod.loadHostCaps();
  calls.length = 0;
  assert.equal(mod.hostCan("session_terminal"), true);
  assert.equal(mod.hostCan("session_lifecycle"), false);
  assert.equal(mod.hostCan("session_groups"), false);
  await assert.rejects(mod.hostInvoke("session_lifecycle", "create_session", { spec: {} }), /This Host does not offer new, ended or renamed sessions\./);
  await assert.rejects(mod.hostInvoke("session_groups", "list_groups"), /This Host does not offer groups\./);
  assert.deepEqual(calls, []);
});

test("keystrokes keep their order across the first answer, and later calls send at once", async () => {
  let answer!: (list: string[]) => void;
  const { mod, calls } = caps(() => new Promise((r) => (answer = r)));
  // Before the first answer every surface stays as it is.
  assert.equal(mod.hostCan("session_terminal"), true);
  const typed = ["a", "b", "c"].map((data) => mod.hostInvoke("session_terminal", "pty_write", { session: "s", data }));
  assert.deepEqual(calls.map(([c]) => c), ["host_capabilities"]);
  answer(LOCAL);
  await Promise.all(typed);
  assert.deepEqual(calls.slice(1).map(([, a]) => (a as { data: string }).data), ["a", "b", "c"]);
  // Known capabilities: the call is made synchronously, in call order.
  void mod.hostInvoke("session_terminal", "pty_write", { session: "s", data: "d" });
  assert.deepEqual(calls.at(-1), ["pty_write", { session: "s", data: "d" }]);
});

test("a reload asks again: a Host that stops offering a capability loses its surfaces", async () => {
  let offered = LOCAL;
  const { mod } = caps(async () => offered);
  await mod.loadHostCaps();
  assert.equal(mod.hostCan("session_groups"), true);
  offered = LOCAL.filter((c) => c !== "session_groups@1");
  await mod.loadHostCaps();
  assert.equal(mod.hostCan("session_groups"), false);
});

test("M3: the agent list has its own capability; without it nothing is asked", async () => {
  const { mod, calls } = caps([...LOCAL, "session_agents@1"]);
  assert.equal(await mod.hostInvoke("session_agents", "list_agents"), "list_agents done");
  const old = caps(["session_terminal@1", "session_inventory@1"]);
  await old.mod.loadHostCaps();
  old.calls.length = 0;
  assert.equal(old.mod.hostCan("session_agents"), false);
  await assert.rejects(old.mod.hostInvoke("session_agents", "list_agents"), /This Host does not offer an agent list\./);
  assert.deepEqual(old.calls, []);
  assert.ok(calls.some(([c]) => c === "list_agents"));
});
