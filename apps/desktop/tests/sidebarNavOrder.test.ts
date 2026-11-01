import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// A row of the tree: the kinds listItems() picks up, with the dataset the sidebar gives them.
interface Row {
  label: string;
  classList: { contains(c: string): boolean; toggle(): void };
  dataset: Record<string, string>;
  tabIndex: number;
  focus(): void;
}

function row(label: string, cls: string, dataset: Record<string, string>): Row {
  return { label, classList: { contains: (c) => c === cls, toggle() {} }, dataset, tabIndex: -1, focus() {} };
}

// The tree in the bug report's video (migrate-karbonfm): the project Skipper's own
// row, and the same Skipper session again as a line of the "claude + zsh" group.
const SKIPPER = "skipper-1";
const tree = (): Row[] => [
  row("migrate-karbonfm", "project-pick", { key: "project:migrate-karbonfm" }),
  row("Skipper (own row)", "session-row", { session: SKIPPER }),
  row("master", "wt-pick", { wt: "/m" }),
  row("E0 Spikes", "group-pick", { group: "g-e0" }),
  row("S-01", "session-row", { session: "s01", inGroup: "g-e0" }),
  row("S-03 money cents", "session-row", { session: "s03", inGroup: "g-e0" }),
  row("S-02 invoice PDF", "session-row", { session: "s02", inGroup: "g-e0" }),
  row("claude + zsh", "group-pick", { group: "g-cz" }),
  row("Skipper (in claude + zsh)", "session-row", { session: SKIPPER, inGroup: "g-cz" }),
  row("Run Dev", "session-row", { session: "run", inGroup: "g-cz" }),
  row("Demo promo video", "group-pick", { group: "g-demo" }),
  row("Promo video", "session-row", { session: "promo", inGroup: "g-demo" }),
  row("I1 Foundation", "group-pick", { group: "g-i1" }),
  row("I1-A DB foundation", "session-row", { session: "i1a", inGroup: "g-i1" }),
  row("I1-B session guard", "session-row", { session: "i1b", inGroup: "g-i1" }),
  row("fix/customers-new-bu", "wt-pick", { wt: "/fix" }),
];

function keyboard(rows: Row[]) {
  const S: any = { roveKey: null, roveAt: null, focused: null, groupPicked: null, activeGroup: null, selection: [] };
  const target = () => ({ addEventListener() {} });
  const globals = {
    window: target(),
    document: {
      body: { classList: { add() {}, remove() {}, toggle() {} } },
      querySelector: () => null,
      querySelectorAll: (sel: string) => (sel.startsWith("#sidebar-scroll .fleet-pick") ? rows : []),
    },
    requestAnimationFrame: (run: () => void) => run(),
  };
  const noMenu = { close() {} };
  // As the real view does: showing a session gives the keys to its pane and makes it
  // the current row; picking a group makes the group's row the current one.
  const view = {
    revealSession(id: string) {
      S.groupPicked = null;
      S.roveKey = id;
      S.focused = id;
    },
    showGroup(id: string, pick = false) {
      S.activeGroup = id;
      if (pick) {
        S.groupPicked = id;
        S.roveKey = id;
      }
    },
    selectProject() {},
  };
  const mod = moduleFromSource("../src/app/keyboard.ts", {
    "./state": { S, sessions: new Map(), collapsed: new Set() },
    "./keys": { isMac: true, actionFor: () => null },
    "../ui/dom": { $: target, modalOpen: () => false },
    "../ui/contextMenu": { ctxMenu: noMenu },
    "./panels": { launchMenu: noMenu },
    "../workspace/view": view,
    "../workspace/rowActs": { rowActs: () => undefined },
    "../terminal/peek": { startPreview() {}, previewOnScreen: () => false },
  }, globals, "");
  return { S, mod };
}

test("Cmd+Shift+Down visits every row in the order the tree shows, past a Skipper that is also in a group", () => {
  const rows = tree();
  const { S, mod } = keyboard(rows);
  S.roveKey = "project:migrate-karbonfm";
  const seen: string[] = [];
  for (let i = 1; i < rows.length; i++) {
    mod.stepList(1);
    seen.push(mod.currentRow().label);
  }
  assert.deepEqual(seen, rows.slice(1).map((r) => r.label));
});

test("Cmd+Shift+Up visits every row in the order the tree shows, past a Skipper that is also in a group", () => {
  const rows = tree();
  const { S, mod } = keyboard(rows);
  S.roveKey = "/fix";
  const seen: string[] = [];
  for (let i = rows.length - 2; i >= 0; i--) {
    mod.stepList(-1);
    seen.push(mod.currentRow().label);
  }
  assert.deepEqual(seen, rows.slice(0, -1).reverse().map((r) => r.label));
});

test("a Skipper shown in its pane, not reached through the list, stands on its own row", () => {
  const rows = tree();
  const { S, mod } = keyboard(rows);
  S.focused = SKIPPER;
  assert.equal(mod.currentRow().label, "Skipper (own row)");
  // After the keys stepped onto its line in the group, that line stays the current one.
  S.roveKey = "run";
  mod.stepList(-1);
  assert.equal(mod.currentRow().label, "Skipper (in claude + zsh)");
});
