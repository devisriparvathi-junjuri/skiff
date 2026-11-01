import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// Cmd+Shift+R on a row: the row becomes its name field, and the sidebar draws again.
// The highlight must stay on that row, not move to the row below it.

interface El {
  label: string;
  dataset: Record<string, string>;
  marks: Set<string>;
  classList: { contains(c: string): boolean; toggle(c: string, on?: boolean): void; add(c: string): void };
  tabIndex: number;
  focused: number;
  focus(): void;
}

function el(label: string, dataset: Record<string, string>, cls: string[] = []): El {
  const marks = new Set(cls);
  const e: El = {
    label,
    dataset,
    marks,
    classList: {
      contains: (c) => marks.has(c),
      toggle: (c, on) => void ((on ?? !marks.has(c)) ? marks.add(c) : marks.delete(c)),
      add: (c) => void marks.add(c),
    },
    tabIndex: -1,
    focused: 0,
    focus: () => void e.focused++,
  };
  return e;
}

const itemKey = (x: El) => x.dataset.key ?? x.dataset.session ?? x.dataset.wt ?? x.dataset.group ?? "";

function sidebar(S: any, items: El[], all: El[]) {
  const calls = { keepRow: [] as string[], preview: [] as string[] };
  const target = () => ({ addEventListener() {} });
  const mod = moduleFromSource("../src/workspace/sidebar.ts", {
    "../ui/dom": { $: target, h: () => ({}), button: () => ({}) },
    "../app/state": { S, sessions: new Map(), collapsed: new Set(), removing: new Set(), removeErrors: new Map() },
    "../app/keyboard": {
      itemKey,
      listItems: () => items,
      findRow: (rows: El[], key: string) => rows.find((x) => itemKey(x) === key),
    },
    "./selection": { keepRow: (k: string) => calls.keepRow.push(k) },
    "../terminal/peek": { startPreview: (k: string) => calls.preview.push(k) },
    "./rename": { renamingKey: () => S.renamingSession ?? S.renaming?.id ?? null },
  }, {
    document: { addEventListener() {}, elementFromPoint: () => null },
    window: target(),
    HTMLButtonElement: class {},
    HTMLInputElement: class {},
    HTMLSelectElement: class {},
  }, "\nexports.restoreFocus = restoreFocus;");
  const side = { querySelectorAll: () => all };
  return { calls, restore: (key: string, near: string[]) => mod.restoreFocus(side, key, null, near) };
}

test("the re-render that opens a session's rename keeps the current row on it", () => {
  // The bug report's tree: "Build App" in rename, the worktree row below it.
  const S: any = { renamingSession: "build", renaming: null };
  const main = el("main", { wt: "/main" });
  const field = el("Build App (name field)", { key: "session-rename" });
  const next = el("fix/sidebar-nav-or…", { wt: "/fix" });
  const { calls, restore } = sidebar(S, [main, next], [main, field, next]);
  restore("build", ["/fix", "/main"]);
  assert.deepEqual(calls.keepRow, []);
  assert.deepEqual(calls.preview, []);
  assert.equal(next.focused, 0);
});

test("the rename of a Skipper's line in a group does not send the keys to its own row", () => {
  const S: any = { renamingSession: "skipper", renaming: null };
  const own = el("Skipper (own row)", { session: "skipper" });
  const field = el("Skipper in group (name field)", { key: "session-rename" });
  const run = el("Run Dev", { session: "run", inGroup: "g" });
  const { calls, restore } = sidebar(S, [own, run], [own, field, run]);
  restore("skipper", ["run", "skipper"]);
  assert.equal(own.focused, 0);
  assert.deepEqual(calls.keepRow, []);
});

test("a row whose session ended still gives the current row to the next one", () => {
  const S: any = { renamingSession: null, renaming: null };
  const main = el("main", { wt: "/main" });
  const next = el("fix", { wt: "/fix" });
  const { calls, restore } = sidebar(S, [main, next], [main, next]);
  restore("gone", ["/fix", "/main"]);
  assert.deepEqual(calls.keepRow, ["/fix"]);
});

test("the row in rename carries the current-row highlight, no other row does", () => {
  const S: any = { roveKey: "build", roveAt: null, focused: "other", groupPicked: null, selection: [] };
  const main = el("main", { wt: "/main" }, ["wt-pick"]);
  const other = el("Other", { session: "other" }, ["session-row"]);
  const field = el("Build App (rename row)", {}, ["session-row", "renaming"]);
  const items = [main, other];
  const target = () => ({ addEventListener() {} });
  const mod = moduleFromSource("../src/app/keyboard.ts", {
    "./state": { S, sessions: new Map(), collapsed: new Set() },
    "./keys": { isMac: true, actionFor: () => null },
    "../ui/dom": { $: target, modalOpen: () => false },
    "../ui/contextMenu": { ctxMenu: { close() {} } },
    "./panels": { launchMenu: { close() {} } },
    "../terminal/peek": { startPreview() {}, previewOnScreen: () => false },
  }, {
    window: target(),
    document: {
      body: { classList: { add() {}, remove() {}, toggle() {} } },
      querySelector: (sel: string) => (sel === "#sidebar-scroll .renaming" ? field : null),
      querySelectorAll: (sel: string) => (sel.startsWith("#sidebar-scroll .fleet-pick") ? items : []),
    },
    requestAnimationFrame: (run: () => void) => run(),
  }, "");
  mod.markCurrent();
  assert.ok(field.marks.has("current"));
  assert.deepEqual(items.filter((x) => x.marks.has("current")).map((x) => x.label), []);
});
