import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real Files tree logic for one Host: every folder from the Host's
 *  `files` capability, which preview and open each row gets, and that a
 *  listing from an older data connection is never stored. */
/** Just enough of an element for the tree's rows and notes. */
function el(tag: string, cls = "", text = ""): any {
  const kids: any[] = [];
  return {
    tag, className: cls, textContent: text, dataset: {} as Record<string, string>, kids,
    style: { setProperty() {} }, setAttribute() {}, addEventListener() {},
    appendChild: (c: any) => kids.push(c), append: (...c: any[]) => kids.push(...c),
  };
}

function tree(o: { remote: boolean; caps: string[] }) {
  let epoch = 0;
  const calls: string[] = [];
  const pending: ((v: unknown) => void)[] = [];
  const acts = new Map<string, any>();
  const opened: string[] = [];
  const mod = moduleFromSource(
    "../src/workspace/files.ts",
    {
      "@tauri-apps/api/core": { invoke: (cmd: string) => calls.push(`invoke ${cmd}`) },
      "../platform/fileActions": { open: (p: string) => opened.push(`open ${p}`), reveal() {}, copy() {} },
      "./rowActs": { setRowActs: (k: string, a: unknown) => acts.set(k, a) },
      "../app/hosts": { isRemote: () => o.remote },
      "../app/hostCaps": {
        hostCan: (c: string) => o.caps.includes(c),
        hostInvoke: (cap: string, cmd: string, args: any) => {
          if (!o.caps.includes(cap)) return Promise.reject(new Error(`no ${cap}`));
          calls.push(`${cap} ${cmd} ${args.worktree} ${JSON.stringify(args.dir)}`);
          return new Promise((resolve) => pending.push(resolve));
        },
      },
      "../app/dataPlane": { dataEpoch: () => epoch },
      "../terminal/peek": { openHostCopy: (w: string, f: string) => opened.push(`copy ${w} ${f}`) },
      "../app/stored": { storedSet: () => new Set() },
      "../app/render": { render() {} },
      "../ui/alerts": { showError() {} },
      "../ui/dom": { h: el },
      "../ui/icons": { chevron: () => el("span", "chevron") },
    },
    { Promise, JSON, Map, Set },
    "\nexports.load = load; exports.listings = listings; exports.filePreview = filePreview; exports.folder = folder; exports.store = store;",
  );
  return { mod, calls, pending, acts, opened, bump: () => epoch++ };
}

const W = "/srv/web";

test("This Mac and a remote Host both list folders through the Host, by worktree and relative folder", async () => {
  for (const remote of [false, true]) {
    const t = tree({ remote, caps: ["files"] });
    t.mod.load(W, W);
    t.mod.load(W, `${W}/src/deep`);
    assert.deepEqual(t.calls, [`files list_files ${W} ""`, `files list_files ${W} "src/deep"`]);
    // Never this Mac's own folder read.
    assert.ok(!t.calls.some((c) => c.startsWith("invoke")));
    t.pending[0]({ entries: [{ name: "a", kind: "file" }], truncated: false });
    await flush();
    assert.deepEqual(JSON.parse(JSON.stringify(t.mod.listings.get(W))), { entries: [{ name: "a", kind: "file" }], truncated: false });
  }
  // The section needs the Host's setting and a Host that lists files.
  const p = { name: "web" };
  assert.equal(tree({ remote: true, caps: [] }).mod.showsFiles(p), false);
});

test("a listing answered after the data connection changed is dropped and asked again", async () => {
  const t = tree({ remote: true, caps: ["files"] });
  t.mod.load(W, W);
  t.bump();
  t.pending[0]({ entries: [{ name: "stale", kind: "file" }], truncated: false });
  await flush();
  assert.equal(t.mod.listings.has(W), false);
  assert.equal(t.calls.length, 2, "asked again on the new connection");
});

test("a remote Host's file previews in the viewer, and Open makes a copy; its paths never reach an opener", () => {
  const t = tree({ remote: true, caps: ["files"] });
  const file = t.mod.filePreview({ name: "a.rs", kind: "file" }, W, `${W}/src/a.rs`);
  assert.deepEqual(JSON.parse(JSON.stringify(file.preview)), { kind: "host-file", wt: W, file: "src/a.rs", path: `${W}/src/a.rs`, copy: true });
  file.open();
  assert.deepEqual(t.opened, [`copy ${W} src/a.rs`]);
  // A link or a FIFO: shown, never opened or copied.
  for (const kind of ["link", "other"]) {
    const x = t.mod.filePreview({ name: "x", kind }, W, `${W}/x`);
    assert.equal(x.preview.copy, false);
    assert.equal(x.open, undefined);
  }
  // This Mac keeps Open with on the Host's own path, which is this Mac's.
  const local = tree({ remote: false, caps: ["files"] });
  const mine = local.mod.filePreview({ name: "a.rs", kind: "file" }, W, `${W}/a.rs`);
  assert.equal(mine.preview.kind, "file");
  mine.open();
  assert.deepEqual(local.opened, [`open ${W}/a.rs`]);
  // A FIFO or a link on This Mac is shown by the Host, never followed,
  // opened or copied here: a link to a FIFO cannot block a local reader.
  for (const kind of ["other", "link"]) {
    const x = local.mod.filePreview({ name: "p", kind }, W, `${W}/p`);
    assert.deepEqual(JSON.parse(JSON.stringify(x.preview)), { kind: "host-file", wt: W, file: "p", path: `${W}/p`, copy: false });
    assert.equal(x.open, undefined);
  }
  assert.deepEqual(local.opened, [`open ${W}/a.rs`], "nothing else reached this Mac's opener");
});

test("a cut listing with no entry shown is never Empty, and its folder still folds", () => {
  const t = tree({ remote: false, caps: ["files"] });
  const notes = (dir: string) => {
    const into = el("div");
    t.mod.folder(into, W, dir, 0);
    return into.kids.map((k: any) => `${k.className}:${k.textContent}`);
  };
  t.mod.store(`${W}/big`, { entries: [], truncated: true });
  assert.deepEqual(notes(`${W}/big`), ["file-note:No entries show. The folder has too many to list."]);
  t.mod.store(`${W}/none`, { entries: [], truncated: false });
  assert.deepEqual(notes(`${W}/none`), ["file-note:Empty"]);
  // In the parent: the cut folder keeps its fold, the empty one has none.
  t.mod.store(W, { entries: [{ name: "big", kind: "dir" }, { name: "none", kind: "dir" }], truncated: false });
  const into = el("div");
  t.mod.folder(into, W, W, 0);
  const folds = Object.fromEntries(into.kids.map((r: any) => [r.dataset.key, r.dataset.fold !== undefined]));
  assert.deepEqual(folds, { [`file:${W}/big`]: true, [`file:${W}/none`]: false });
  assert.ok(t.acts.get(`file:${W}/big`).fold, "Space and Right open it");
});
