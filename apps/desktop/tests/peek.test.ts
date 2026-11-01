import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** Review M6 P2: the real peek module's ledger result viewer across a new
 *  data connection. A result keeps its own uid and title, is read again from
 *  the new connection, and is never replaced by the row's own preview. */

class El {
  cls: string;
  text: string;
  kids: El[] = [];
  parent: El | null = null;
  hidden = false;
  tabIndex = 0;
  constructor(_tag: string, cls = "", text = "") {
    this.cls = cls;
    this.text = text;
  }
  get classList() {
    return { contains: (c: string) => this.cls.split(" ").includes(c), add() {} };
  }
  append(...kids: (El | string)[]) {
    for (const k of kids) {
      if (typeof k === "string") this.text += k;
      else {
        k.parent = this;
        this.kids.push(k);
      }
    }
  }
  appendChild(k: El) {
    this.append(k);
    return k;
  }
  setAttribute() {}
  addEventListener() {}
  focus() {}
  contains(n: unknown): boolean {
    for (let x = n as El | null; x; x = x.parent) if (x === this) return true;
    return false;
  }
  remove() {
    if (this.parent) this.parent.kids = this.parent.kids.filter((k) => k !== this);
    this.parent = null;
  }
  querySelector(sel: string): El | null {
    const want = sel.split(",").map((s) => s.trim().split(".").pop()!);
    for (const k of this.kids) {
      if (want.some((c) => k.classList.contains(c))) return k;
      const r = k.querySelector(sel);
      if (r) return r;
    }
    return null;
  }
  replaceWith(n: El) {
    const p = this.parent!;
    p.kids[p.kids.indexOf(this)] = n;
    n.parent = p;
    this.parent = null;
  }
  get all(): string {
    return [this.text, ...this.kids.map((k) => k.all)].join(" ");
  }
}

function viewer() {
  const main = new El("main");
  const document = { activeElement: null as unknown, addEventListener() {} };
  const h = (tag: string, cls = "", text = "") => new El(tag, cls, text);
  const reads: { cmd: string; args: any; settle: { ok: (v: unknown) => void; fail: (e: unknown) => void } }[] = [];
  let epoch = 1;
  // The current sidebar row: the coordinator's (no preview), or a Host file's.
  const rows: Record<string, unknown> = {
    coordinator: undefined,
    file: { kind: "host-file", path: "web/notes.md", wt: "/srv/web", file: "notes.md", copy: false },
  };
  let row = "coordinator";
  const mod = moduleFromSource(
    "../src/terminal/peek.ts",
    {
      "@tauri-apps/api/core": { invoke: () => Promise.resolve(null), Channel: class {} },
      "../ui/dom": { $: () => main, h, button: (cls: string, text: string) => h("button", cls, text) },
      "../ui/icons": { icon: () => h("svg") },
      "../ui/alerts": { showError() {} },
      "../app/keyboard": { currentRow: () => ({ key: row }), itemKey: (r: { key: string }) => r.key, appKey: () => null },
      "../app/renderRequest": { render() {} },
      "../app/state": { S: {}, sessions: new Map() },
      "../workspace/rowActs": { rowActs: (key: string) => ({ preview: rows[key] }) },
      "./terminalState": { panes: new Map() },
      "./terminal": { fitShown() {} },
      "../app/hostCaps": {
        hostInvoke: (_cap: string, cmd: string, args: unknown) =>
          new Promise((ok, fail) => reads.push({ cmd, args: JSON.parse(JSON.stringify(args)), settle: { ok, fail } })),
      },
      "../app/dataPlane": { dataEpoch: () => epoch },
      "../app/hosts": { hostName: () => "devbox" },
      "./diffView": { diffState: (text: string) => h("div", "diff-view-body", text) },
      "./fileView": { fileBody: (f: { text: string }) => h("div", "diff-view-body", f.text) },
    },
    { Promise, Map, Set, JSON, document, window: { setTimeout: () => 0 }, clearTimeout() {}, URL },
    "",
  );
  const text = (f: string) => ({ content: "text", text: f, truncated: false });
  return {
    mod, reads,
    card: () => main.kids.at(-1)?.all ?? "",
    newConnection: () => void epoch++,
    on: (key: string) => void (row = key),
    text,
  };
}

test("review M6 P2: a shown ledger result is read again on a new data connection, by its own uid", async () => {
  for (const over of ["coordinator", "file"]) {
    const v = viewer();
    v.on(over);
    if (over === "file") {
      // The file row's own preview shows first; the result opens over it.
      v.mod.startPreview("file", false);
      v.reads.shift()!.settle.ok(v.text("file body"));
      await flush();
    }
    v.mod.openLedgerResult("a1b2c3d4", "Write the notes");
    assert.deepEqual(v.reads.map((r) => [r.cmd, r.args]), [["ledger_result", { uid: "a1b2c3d4" }]]);
    v.reads.shift()!.settle.ok(v.text("old result"));
    await flush();
    assert.match(v.card(), /Result: Write the notes.*old result/s, over);
    // A new data connection: the dispatcher reads the result again, not the row's file.
    v.newConnection();
    v.mod.refreshHostDiff();
    assert.deepEqual(v.reads.map((r) => [r.cmd, r.args]), [["ledger_result", { uid: "a1b2c3d4" }]], over);
    v.reads.shift()!.settle.ok(v.text("new result"));
    await flush();
    assert.match(v.card(), /Result: Write the notes.*new result/s, over);
    assert.doesNotMatch(v.card(), /old result|file body/, over);
  }
});

test("review M6 P2: a pending ledger result from an older connection never shows; the new read does", async () => {
  const v = viewer();
  v.mod.openLedgerResult("a1b2c3d4", "Write the notes");
  const stale = v.reads.shift()!;
  v.newConnection();
  v.mod.refreshHostDiff();
  const fresh = v.reads.shift()!;
  assert.deepEqual(fresh.args, { uid: "a1b2c3d4" });
  stale.settle.ok(v.text("old result"));
  await flush();
  assert.doesNotMatch(v.card(), /old result/);
  fresh.settle.ok(v.text("new result"));
  await flush();
  assert.match(v.card(), /new result/);
  // Nothing else was asked: no path, no opener.
  assert.equal(v.reads.length, 0);
});

test("a row's own preview after a result is read again as the row's, not as the result", async () => {
  const v = viewer();
  v.on("coordinator");
  v.mod.openLedgerResult("a1b2c3d4", "Write the notes");
  v.reads.shift()!.settle.ok(v.text("result"));
  await flush();
  v.on("file");
  v.mod.startPreview("file", false);
  v.reads.shift()!.settle.ok(v.text("file body"));
  await flush();
  v.newConnection();
  v.mod.refreshHostDiff();
  assert.deepEqual(v.reads.map((r) => r.cmd), ["read_file"]);
});
