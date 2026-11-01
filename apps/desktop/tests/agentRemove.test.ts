import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// The real rowTable.ts, as Settings › Agents uses it: a row that something
// starts has its − off and one plain line under it. Any other row, default
// or custom, removes.
class El {
  children: El[] = [];
  listeners = new Map<string, () => void>();
  dataset: Record<string, string> = {};
  classList = { add: (c: string) => (this.className += ` ${c}`), toggle() {} };
  disabled = false;
  type = "";
  title = "";
  tag: string;
  className: string;
  textContent: string;
  constructor(tag: string, className = "", textContent = "") {
    this.tag = tag;
    this.className = className;
    this.textContent = textContent;
  }
  append(...kids: El[]) { this.children.push(...kids); }
  appendChild(k: El) { this.children.push(k); return k; }
  setAttribute() {}
  addEventListener(ev: string, run: () => void) { this.listeners.set(ev, run); }
  all(): El[] { return [this, ...this.children.flatMap((c) => c.all())]; }
}

function table(rows: { id: string; used_by: string | null }[]) {
  const removed: string[] = [];
  const m = moduleFromSource(
    "../src/appearance/rowTable.ts",
    { "../ui/dom": { h: (tag: string, cls = "", text = "") => new El(tag, cls, text) }, "../ui/runes": { rune: () => new El("svg") } },
    { console, document: { querySelector: () => null }, CSS: { escape: (s: string) => s }, HTMLInputElement: class {} },
    "",
  );
  const t: El = m.rowTable(
    {
      grid: "g-agents",
      words: m.tableWords("agent"),
      labelledBy: "x",
      rows,
      id: (a: any) => a.id,
      cols: [{ key: "name", head: "AGENT", name: "name", kind: "key", get: (a: any) => a.id }],
      kept: (a: any) => a.used_by,
      edit: async () => null,
      remove: async (a: any) => (removed.push(a.id), null),
    },
    m.tableState(),
    () => {},
  );
  const minus = (id: string) => t.all().find((e) => e.className.includes("c-remove") && t.all().some((r) => r.dataset.id === id && r.all().includes(e)))!;
  // What keeps the − off shows in the STATUS cell. No line goes under the row.
  const lines = t.all().filter((e) => e.className === "c-kept").map((e) => e.textContent);
  assert.equal(t.children.some((e) => !e.className.startsWith("set-row") && e.className !== "set-hint"), false);
  return { minus, lines, removed };
}

test("a used agent keeps its − off and shows what uses it", async () => {
  const { minus, lines, removed } = table([
    { id: "claude", used_by: "Skipper +2" },
    { id: "hermes", used_by: null },
    { id: "aider", used_by: null },
  ]);
  assert.equal(minus("claude").disabled, true);
  assert.deepEqual([...lines], ["· Skipper +2"]);
  // A default (hermes) and a custom agent (aider) both remove.
  assert.equal(minus("hermes").disabled, false);
  minus("hermes").listeners.get("click")!();
  minus("aider").listeners.get("click")!();
  await new Promise((r) => setImmediate(r));
  assert.deepEqual(removed, ["hermes", "aider"]);
});
