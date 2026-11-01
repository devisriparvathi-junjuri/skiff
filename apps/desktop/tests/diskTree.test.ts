import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// The real diskTree.ts and diskUse.ts. The tree adds up the rule's verdicts:
// CAN FLUSH, STATE, the flush plan, and the dialog's REMOVE and KEEP.
const size = (n: number) => `${n} B`;
const T = moduleFromSource("../src/appearance/diskTree.ts", {}, {}, "");
// Values made in the module's own context: compare them as plain data.
const plain = (x: unknown) => JSON.parse(JSON.stringify(x));

const NOW = 10_000_000;
const v = (kind: string, why: string, extra: Record<string, unknown> = {}) => ({ kind, why, ...extra });
const file = (path: string, bytes: number, state: unknown, files = 1) => ({ name: path.split("/").pop(), path, bytes, files, mtime: 1, state });
const dir = (path: string, children: any[], extra: Record<string, unknown> = {}) => ({
  name: (path.split("/").pop() || "skiff-agent") + "/",
  path,
  dir: true,
  bytes: children.reduce((a, c) => a + c.bytes, 0),
  files: children.reduce((a, c) => a + c.files, 0),
  mtime: 1,
  children,
  ...extra,
});

/** A plan folder with a done task, a working task and plan.md; old dated files; a pinned image. */
function tree() {
  const done = dir("results/p1-ship/a1-review", [file("results/p1-ship/a1-review/result.md", 100, v("stale", "done")), dir("results/p1-ship/a1-review/worktree", [], { state: v("stale", "done"), bytes: 900, files: 400 })], {
    work: { uid: "a1", kind: "task", title: "Review" },
  });
  const working = dir("results/p1-ship/b1-fix", [file("results/p1-ship/b1-fix/result.md", 50, v("use", "working", { label: "Fix" }))], { work: { uid: "b1", kind: "task", title: "Fix" } });
  const p = dir("results/p1-ship", [file("results/p1-ship/plan.md", 10, v("use", "plan-open", { label: "Ship" })), done, working], { work: { uid: "p1", kind: "plan", title: "Ship" } });
  const results = dir("results", [p, file("results/2026-10-01-old.md", 30, v("stale", "no-folder")), file("results/new.md", 7, v("held", "changed", { changed: NOW - 3 * 3600 }))], { frame: true });
  const cache = dir("cache", [file("cache/a.png", 5, v("prot", "pinned")), file("cache/b.png", 6, v("stale", "image")), file("cache/images.json", 1, v("prot", "skiff-file"))], { frame: true });
  return dir("", [file("ledger.db", 2, v("prot", "skiff-file")), results, cache], { frame: true });
}

test("CAN FLUSH adds up the stale files, and STATE says Mixed with the files that stay", () => {
  const t = tree();
  assert.equal(T.canFlush(t), 100 + 900 + 30 + 6);
  assert.equal(T.canFlush(T.find(t, "results/p1-ship")), 1000);
  const p = T.rowState(T.find(t, "results/p1-ship"), NOW);
  assert.deepEqual(plain([p.kind, p.text, p.label]), ["mixed", "Mixed · 2 files stay", "Ship"]);
  const a = T.rowState(T.find(t, "results/p1-ship/a1-review"), NOW);
  assert.deepEqual(plain([a.kind, a.text, a.label]), ["stale", "Stale · done", "Review"]);
  assert.equal(T.rowState(T.find(t, "results/p1-ship/b1-fix"), NOW).text, "In use · working");
  assert.equal(T.rowState(T.find(t, "results/new.md"), NOW).text, "Held · changed 3 h ago");
  assert.equal(T.rowState(T.find(t, "cache/a.png"), NOW).text, "Protected · pinned");
});

test("a flush plan takes the largest stale parts, never Skiff's folders or the work in use", () => {
  const t = tree();
  const { go, keep } = T.plan(t);
  assert.deepEqual(plain(go.map((n: any) => n.path)), ["results/p1-ship/a1-review", "results/2026-10-01-old.md", "cache/b.png"]);
  assert.deepEqual(plain(keep.map((n: any) => n.path)), ["ledger.db", "results/p1-ship/plan.md", "results/p1-ship/b1-fix/result.md", "results/new.md", "cache/a.png", "cache/images.json"]);
  // The expected item carries the count's size, files and newest change.
  assert.deepEqual(plain(T.expected(go[0])), { path: "results/p1-ship/a1-review", bytes: 1000, files: 401, mtime: 1 });
  const gs = T.groups(keep, t);
  // One line for each reason, the largest first.
  assert.deepEqual(plain(gs.map((g: any) => [g.name, g.files, g.bytes])), [
    ["In use · working", 1, 50],
    ["In use · plan open", 1, 10],
    ["Held · changed in the last 24 h", 1, 7],
    ["Protected · pinned", 1, 5],
    ["Protected · Skiff file", 2, 3],
  ]);
});

test("rows sort by CAN FLUSH, then name; a closed folder hides its rows; the idle reason names the last change", () => {
  const t = tree();
  const rows = T.visibleRows(t, new Set(["", "results"])).map((r: any) => `${r.depth}:${r.node.path}`);
  assert.deepEqual(plain(rows), ["0:", "1:results", "2:results/p1-ship", "2:results/2026-10-01-old.md", "2:results/new.md", "1:cache", "1:ledger.db"]);
  assert.equal(T.idleReason(T.find(t, "results/new.md"), NOW), "Changed 3 h ago");
  assert.equal(T.idleReason(T.find(t, "ledger.db"), NOW), "Nothing to flush");
});

// A small DOM for the page.
class El {
  tag: string;
  className: string;
  textContent: string;
  innerHTML = "";
  type = "";
  disabled = false;
  tabIndex = 0;
  id = "";
  children: El[] = [];
  dataset: Record<string, string> = {};
  style: Record<string, string> = {};
  attrs: Record<string, string> = {};
  listeners = new Map<string, ((e: any) => void)[]>();
  constructor(tag: string, cls = "", text = "") {
    this.tag = tag;
    this.className = cls;
    this.textContent = text;
  }
  append(...k: El[]) { this.children.push(...k); }
  appendChild(k: El) { this.children.push(k); return k; }
  setAttribute(k: string, val: string) { this.attrs[k] = val; }
  addEventListener(ev: string, run: (e: any) => void) { this.listeners.set(ev, [...(this.listeners.get(ev) ?? []), run]); }
  fire(ev: string, e: any = {}) { for (const run of this.listeners.get(ev) ?? []) run({ preventDefault() {}, stopPropagation() {}, ...e }); }
  all(): El[] { return [this, ...this.children.flatMap((c) => c.all())]; }
  text(): string { return [this.textContent, ...this.children.map((c) => c.text())].filter(Boolean).join(" "); }
  focus() {}
}

test("the page draws the tree, and Flush asks with REMOVE and KEEP, then sends what the count showed", async () => {
  const dialogs: any[] = [];
  const calls: [string, any][] = [];
  const usage = { shown: "~/skiff-agent", tree: tree(), now: NOW, daemon: true, ledger_error: null, unreadable: 0, cache_days: 30 };
  const P = moduleFromSource(
    "../src/appearance/diskUse.ts",
    {
      "@tauri-apps/api/core": {
        invoke: async (cmd: string, args: any) => {
          calls.push([cmd, args]);
          return cmd === "flush_disk" ? { files: 401, bytes: 1000, kept: 0, rows: 1, errors: [] } : usage;
        },
      },
      "../ui/dom": { h: (tag: string, cls = "", text = "") => new El(tag, cls, text) },
      "../ui/dialogParts": { actionDialog: (o: any) => dialogs.push(o) },
      "../ui/runes": { rune: () => new El("svg") },
      "./skiffFolderDialog": { size },
      "./diskTree": T,
    },
    { document: { querySelector: () => null }, CSS: { escape: (s: string) => s } },
    "",
  );
  const s = P.diskState();
  s.usage = usage;
  let draws = 0;
  const parts: El[] = P.diskUseSection(s, () => draws++, () => {}, { local: true, peek: () => {} });
  const rows = parts[1].all().filter((e) => e.className.includes("set-disk-row"));
  assert.deepEqual(plain(rows.map((r) => r.dataset.path)), ["", "results", "results/p1-ship", "results/2026-10-01-old.md", "results/new.md", "cache", "ledger.db"]);
  const plan = rows.find((r) => r.dataset.path === "results/p1-ship")!;
  assert.match(plan.text(), /Mixed · 2 files stay/);
  const flush = plan.all().find((e) => e.tag === "button" && e.className === "set-rm")!;
  assert.equal(flush.textContent, "Flush 1000 B…");
  // A row with nothing to flush says why, and has no button.
  const held = rows.find((r) => r.dataset.path === "results/new.md")!;
  assert.ok(held.text().includes("Changed 3 h ago") && !held.all().some((e) => e.className === "set-rm"));

  flush.fire("click");
  assert.equal(dialogs.length, 1);
  const d = dialogs[0];
  assert.equal(d.title, "Remove 401 files (1000 B)?");
  const words = d.content.map((c: El) => c.text()).join(" ");
  assert.match(words, /Remove 401 files · 1000 B/);
  assert.match(words, /Keep 2 files · 60 B/);
  assert.match(words, /A job that writes here again later can't be predicted\./);
  await d.submit();
  assert.deepEqual(plain(calls[0]), ["flush_disk", { items: [{ path: "results/p1-ship/a1-review", bytes: 1000, files: 401, mtime: 1 }] }]);
  assert.equal(s.removed, "Removed 401 files, 1000 B. 1 ledger row now shows “Result flushed”.");
});

/** The page with `answer` for each command; `calls` gets each command. */
function page(answer: (cmd: string) => unknown, local = true) {
  const calls: [string, any][] = [];
  const peeks: [string, string, string][] = [];
  const P = moduleFromSource(
    "../src/appearance/diskUse.ts",
    {
      "@tauri-apps/api/core": {
        invoke: async (cmd: string, args: any) => {
          calls.push([cmd, args]);
          const out = answer(cmd);
          if (out instanceof Error) throw out;
          return out;
        },
      },
      "../ui/dom": { h: (tag: string, cls = "", text = "") => new El(tag, cls, text) },
      "../ui/dialogParts": { actionDialog: () => {} },
      "../ui/runes": { rune: () => new El("svg") },
      "./skiffFolderDialog": { size },
      "./diskTree": T,
    },
    { document: { querySelector: () => null }, CSS: { escape: (s: string) => s } },
    "",
  );
  const s = P.diskState();
  s.usage = { shown: "~/skiff-agent", tree: tree(), now: NOW, daemon: true, ledger_error: null, unreadable: 0, cache_days: 30 };
  const draw = () => {
    const parts: El[] = P.diskUseSection(s, draw, () => {}, { local, peek: (script: string, cwd: string, name: string) => peeks.push([script, cwd, name]) });
    return parts[1].all().filter((e) => e.className.includes("set-disk-row"));
  };
  const row = (path: string) => draw().find((r) => r.dataset.path === path)!;
  return { s, calls, peeks, row };
}

const tick = () => new Promise((r) => setImmediate(r));
const reveal = (r: El) => r.all().find((e) => e.dataset.key?.startsWith("reveal:"));

test("a click on a file row opens it through disk_open, as Open with says; a folder row's click only moves the row", async () => {
  const p = page((cmd) => (cmd === "disk_open" ? { kind: "run", script: "glow result.md", cwd: "/home/u/skiff-agent/results" } : null));
  p.row("results/new.md").fire("click");
  await tick();
  assert.deepEqual(plain(p.calls), [["disk_open", { path: "results/new.md" }]]);
  assert.deepEqual(p.peeks, [["glow result.md", "/home/u/skiff-agent/results", "new.md"]]);
  assert.equal(p.s.cur, "results/new.md");
  p.row("results").fire("click");
  await tick();
  assert.equal(p.calls.length, 1);
  assert.equal(p.s.cur, "results");

  // No app, or a path the native check refuses: one error line, and no peek.
  const q = page((cmd) => (cmd === "disk_open" ? { kind: "no_app", message: "No app opens new.md." } : null));
  q.row("results/new.md").fire("click");
  await tick();
  assert.equal(q.s.error, "No app opens new.md.");
  const r = page(() => new Error("results/out is not in the agent folder"));
  r.row("results/new.md").fire("click");
  await tick();
  assert.equal(r.s.error, "Error: results/out is not in the agent folder");
  assert.equal(r.peeks.length, 0);
});

test("a folder row has Show in file manager, which sends the tree's path to disk_reveal; a file row has none", async () => {
  const p = page(() => null);
  assert.equal(reveal(p.row("results/new.md")), undefined);
  const b = reveal(p.row("results/p1-ship"))!;
  assert.equal(b.attrs["aria-label"], "Show in file manager");
  b.fire("click");
  await tick();
  assert.deepEqual(plain(p.calls), [["disk_reveal", { path: "results/p1-ship" }]]);
  // The agent folder's own row shows the folder itself.
  reveal(p.row(""))!.fire("click");
  await tick();
  assert.deepEqual(plain(p.calls[1]), ["disk_reveal", { path: "" }]);

  // A remote Host is shown: this computer's openers are off.
  const remote = page(() => null, false);
  assert.equal(reveal(remote.row("results")), undefined);
  remote.row("results/new.md").fire("click");
  await tick();
  assert.equal(remote.calls.length, 0);
});
