import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** The real New worktree over a fake Host and a fake prompt. */
function page(branches: unknown) {
  const sent: any[] = [];
  const invoke = async (cmd: string, args: any) => {
    if (cmd === "host_capabilities") return ["projects@1", "worktrees@1"];
    if (cmd === "list_branches") {
      if (!branches) throw new Error("unknown cmd");
      return branches;
    }
    if (cmd === "add_worktree") return sent.push(args), { path: "/w-worktrees/x" };
    throw new Error(`unexpected ${cmd}`);
  };
  const el = (tag: string) => ({ tag, hidden: false, value: "", kids: [] as any[], append(...k: any[]) { this.kids.push(...k); }, setAttribute() {} });
  let prompt: any;
  const core = { "@tauri-apps/api/core": { invoke } };
  const g = { Promise, Set, Map, Error, JSON };
  const hostCaps = moduleFromSource("../src/app/hostCaps.ts", core, g, "");
  const menus = moduleFromSource("../src/workspace/menus.ts", {
    ...core,
    "../app/hostCaps": hostCaps,
    "../app/state": { S: {}, sessions: new Map(), selectedWorktree: new Map(), removeErrors: new Map(), removing: new Set() },
    "../app/daemon": { loadProjects: async () => {}, newSession: () => {} },
    "../ui/confirm": { promptAction: (o: any) => (prompt = o), confirmAction: () => {} },
    "../ui/dom": { h: (tag: string, _c: string, text?: string) => Object.assign(el(tag), { textContent: text }) },
  }, g, "");
  return { menus, sent, prompt: () => prompt };
}

/** The page's values come from another realm: compare plain copies. */
const plain = (v: unknown) => JSON.parse(JSON.stringify(v));

const LIST = {
  head: "main",
  branches: [
    { name: "main", worktree: true },
    { name: "old" },
    { name: "origin/main", remote: true },
    { name: "origin/far", remote: true },
  ],
};

test("New worktree: a listed branch is checked out, with no base", async () => {
  const p = page(LIST);
  await p.menus.newWorktree({ name: "web" });
  const o = p.prompt();
  assert.deepEqual(plain(o.options), ["old", "far"], "no branch with a worktree, a remote's by its own name");
  assert.equal(o.optionsHead, "Branches without a worktree");
  const from = o.after[0];
  o.onInput("far");
  assert.ok(from.hidden, "From has no job for an existing branch");
  await o.submit("far");
  assert.deepEqual(plain(p.sent.at(-1)), { project: "web", branch: "far", base: null, checkout: true });
});

test("New worktree: a new name starts from the From branch", async () => {
  const p = page(LIST);
  await p.menus.newWorktree({ name: "web" });
  const o = p.prompt();
  const [from] = o.after;
  const select = from.kids[1];
  assert.deepEqual(plain(select.kids.map((k: any) => k.value)), ["main", "old", "origin/main", "origin/far"]);
  assert.equal(select.value, "main");
  o.onInput("feat/new");
  assert.ok(!from.hidden);
  await o.submit("feat/new");
  assert.deepEqual(plain(p.sent.at(-1)), { project: "web", branch: "feat/new", base: null, checkout: false }, "the folder's branch is the default");
  select.value = "origin/main";
  await o.submit("feat/two");
  assert.deepEqual(plain(p.sent.at(-1)), { project: "web", branch: "feat/two", base: "origin/main", checkout: false });
});

test("New worktree on a Host without branch_list: the name field alone, as before", async () => {
  const p = page(null);
  await p.menus.newWorktree({ name: "web" });
  const o = p.prompt();
  assert.equal(o.options, undefined);
  assert.ok(o.after[0].hidden);
  await o.submit("feat/x");
  assert.deepEqual(plain(p.sent.at(-1)), { project: "web", branch: "feat/x", base: null, checkout: false });
});
