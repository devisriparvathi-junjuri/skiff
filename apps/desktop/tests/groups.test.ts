import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real groups module over a fake Host that, as skiffd does, makes a new
 *  group for every save with an empty id. The first create's reply can be
 *  held back and then lost. */
function groups(o: { loseFirstReply?: boolean } = {}) {
  const host: any[] = [];
  const sent: string[] = [];
  const errors: unknown[] = [];
  let loseReply: ((e: Error) => void) | null = null;
  const S: any = { groups: [], activeGroup: null, groupPicked: null, roveKey: null, renaming: null, focused: null, single: null };
  const hostInvoke = async (_cap: string, cmd: string, args: any) => {
    if (cmd === "list_groups") return host.map((g) => ({ ...g }));
    if (cmd !== "save_group") return;
    sent.push(args.group.id);
    let saved = host.find((g) => g.id === args.group.id);
    if (!saved) host.push((saved = { ...args.group, id: `g${host.length + 1}` }));
    else Object.assign(saved, args.group);
    if (o.loseFirstReply && host.length === 1 && sent.length === 1) return new Promise((_ok, fail) => (loseReply = fail));
    return { ...saved };
  };
  const mod = moduleFromSource(
    "../src/app/groups.ts",
    {
      "../workspace/layoutSlots": { PRESETS: [], isSlot: () => false },
      "../workspace/layout": { shapeName: () => null, sessionsOf: (l: any) => [l.session] },
      "../workspace/model": { agentName: () => "agent" },
      "../ui/alerts": { showError: (e: unknown) => errors.push(e) },
      "./render": { render() {} },
      "./state": { S, sessions: new Map() },
      "./stateQueries": { activeGroupObj: () => S.groups.find((g: any) => g.id === S.activeGroup), shownIds: () => [] },
      "../workspace/view": { inBackground() {}, unfocus() {} },
      "./hostCaps": { hostCan: () => true, hostInvoke },
    },
    { Promise, WeakSet, Map, Set, console },
    "",
  );
  return { mod, S, host, sent, errors, lose: (e: Error) => loseReply?.(e) };
}

const pending = () => ({ id: "tmp-1", name: "one", layout: { kind: "pane", session: "s1" }, focus: "s1", cwd: "/srv" });

test("a queued save never repeats a create whose reply was lost", async () => {
  const h = groups({ loseFirstReply: true });
  const g = pending();
  h.S.groups.push(g);
  h.S.activeGroup = g.id;
  // Created, then renamed before the Host answered.
  h.mod.saveGroup(g);
  g.name = "renamed";
  h.mod.saveGroup(g);
  await flush();
  h.lose(new Error("the link dropped"));
  for (let i = 0; i < 10; i++) await flush();
  // One create went out; the queued save did not make a second group.
  assert.deepEqual(h.sent, [""]);
  assert.deepEqual(h.host.map((x) => x.id), ["g1"]);
  assert.equal(h.errors.length, 1);
  // The page shows what the Host holds, not a group of its own.
  assert.deepEqual(h.S.groups.map((x: any) => x.id), ["g1"]);
  assert.equal(h.S.activeGroup, null);
});

test("a create that answers gives later saves its id", async () => {
  const h = groups();
  const g = pending();
  h.S.groups.push(g);
  h.mod.saveGroup(g);
  h.mod.saveGroup(g);
  for (let i = 0; i < 10; i++) await flush();
  assert.deepEqual(h.sent, ["", "g1"]);
  assert.equal(g.id, "g1");
  assert.equal(h.host.length, 1);
});
