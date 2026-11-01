import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real Changes section logic for one Host: which diff a row shows, who
 *  decides the section shows, and that a Git read from an older data
 *  connection is never stored. */
function section(o: { remote: boolean; caps: string[] }) {
  let epoch = 0;
  const calls: string[] = [];
  const pending: ((v: unknown) => void)[] = [];
  const mod = moduleFromSource(
    "../src/workspace/changes.ts",
    {
      "@tauri-apps/api/core": {
        invoke: (cmd: string, args: any) => {
          calls.push(`${cmd} ${args?.path ?? ""}`);
          return new Promise((resolve) => pending.push(resolve));
        },
      },
      "../app/hosts": { isRemote: () => o.remote },
      "../app/hostCaps": {
        hostCan: (c: string) => o.caps.includes(c),
        hostInvoke: async (cap: string, cmd: string, args: any) => {
          if (!o.caps.includes(cap)) throw new Error(`no ${cap}`);
          calls.push(`${cap} ${cmd} ${args.project} ${args.on}`);
        },
      },
      "../ui/alerts": { showError: (e: unknown) => calls.push(`error ${e}`) },
      "../app/dataPlane": { dataEpoch: () => epoch },
      "./model": { branchName: (w: any) => w.branch, isFolder: (p: any) => p.kind === "folder" },
      "../app/stored": { storedSet: () => new Set() },
      "../app/render": { render() {}, scheduleRender() {} },
      "../app/state": { collapsed: new Set() },
    },
    { Promise, JSON, Map, Set },
    "\nexports.diffPreview = diffPreview; exports.load = load;",
  );
  return { mod, calls, pending, bump: () => epoch++ };
}

const W = { path: "/srv/web", branch: "main" };

test("a remote Host's change row shows its git's text in the viewer, never a command here", () => {
  const p = section({ remote: true, caps: ["git_status", "git_diff"] });
  assert.equal(p.mod.diffPreview(W, "a.txt").kind, "host-diff");
  assert.equal(p.mod.diffPreview(W, "a.txt").file, "a.txt");
  assert.equal(p.mod.diffPreview(W).wait, true);
  // A Host without git_diff: the list shows, with no diff at all.
  assert.equal(section({ remote: true, caps: ["git_status"] }).mod.diffPreview(W, "a.txt"), undefined);
  // This Mac keeps its own diff command.
  assert.equal(section({ remote: false, caps: [] }).mod.diffPreview(W, "a.txt").kind, "diff");
});

test("the Host's projects.toml decides the Changes section, on This Mac as on a remote Host", async () => {
  for (const remote of [true, false]) {
    const p = section({ remote, caps: ["project_config"] });
    assert.equal(p.mod.showsChanges({ name: "web", changes: false }), false);
    assert.equal(p.mod.showsChanges({ name: "web", changes: null }), true);
    assert.equal(p.mod.showsChanges({ name: "web" }), true);
    // The change goes to the Host (`project_config`), and the row follows it.
    const web: any = { name: "web" };
    await p.mod.setShowChanges(web, false);
    assert.deepEqual(p.calls, ["project_config set_project_changes web false"]);
    assert.equal(p.mod.showsChanges(web), false);
  }
  // A Host that takes no project change: refused before any call, row unchanged.
  const p = section({ remote: true, caps: [] });
  const web: any = { name: "web" };
  await p.mod.setShowChanges(web, false);
  assert.deepEqual(p.calls, ["error Error: no project_config"]);
  assert.equal(p.mod.showsChanges(web), true);
});

test("a Git read answered after the data connection changed is dropped and asked again", async () => {
  const p = section({ remote: true, caps: ["git_status"] });
  p.mod.load(W.path);
  assert.deepEqual(p.calls, ["git_changes /srv/web"]);
  // skiff-host restarted while the read was out.
  p.bump();
  p.pending[0]({ changes: [{ path: "stale.txt", status: "M", added: 1, removed: 0 }] });
  await flush();
  assert.equal(p.mod.hasChanges(W), false, "the stale list was stored");
  assert.deepEqual(p.calls, ["git_changes /srv/web", "git_changes /srv/web"]);
  p.pending[1]({ changes: [{ path: "a.txt", status: "M", added: 1, removed: 0 }] });
  await flush();
  assert.equal(p.mod.hasChanges(W), true);
});
