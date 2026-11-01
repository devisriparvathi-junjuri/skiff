import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** A stand-in element: enough for the panel to build. */
class El {
  children: El[] = [];
  classList = { set: new Set<string>(), toggle(c: string, on: boolean) { on ? this.set.add(c) : this.set.delete(c); }, contains(c: string) { return this.set.has(c); } };
  id = "";
  textContent: string;
  listeners: Record<string, (() => void)[]> = {};
  tag: string;
  className: string;
  constructor(tag: string, className = "", text = "") {
    this.tag = tag;
    this.className = className;
    this.textContent = text;
  }
  append(...c: El[]) { this.children.push(...c); }
  replaceChildren(...c: El[]) { this.children = c; }
  insertBefore(c: El) { this.children.unshift(c); }
  setAttribute() {}
  addEventListener(k: string, f: () => void) { (this.listeners[k] ??= []).push(f); }
  remove() { main.children = main.children.filter((x) => x !== this); }
  all(): El[] { return [this, ...this.children.flatMap((c) => c.all())]; }
  querySelector(sel: string) { return this.all().find((e) => e.className.split(" ").includes(sel.slice(1))) ?? null; }
}
const main = new El("main");
const label = new El("span");

function panel() {
  let now = 0;
  let line!: (e: { payload: string }) => void;
  const calls: string[] = [];
  const mod = moduleFromSource(
    "../src/app/connectPanel.ts",
    {
      "@tauri-apps/api/event": { listen: async (_: string, f: typeof line) => void (line = f) },
      "../ui/dom": { $: () => label, h: (t: string, c?: string, x?: string) => new El(t, c, x), button: (c: string, x: string, f: () => void) => { const b = new El("button", c, x); b.addEventListener("click", f); return b; } },
      "../ui/runes": { rune: () => new El("svg") },
      "../platform/clipboard": { copyText: async (t: string) => void calls.push(`copy ${t}`) },
      "./hosts": { hostName: () => "p0p", checkShownHost: () => void calls.push("check") },
    },
    {
      Date: { now: () => now },
      window: { setInterval: () => 1, setTimeout: (f: () => void) => (calls.push("wait"), 2) },
      clearInterval() {},
      clearTimeout() {},
      document: { getElementById: (id: string) => (id === "main" ? main : main.all().find((e) => e.id === id) ?? null) },
      Promise,
    },
    "",
  );
  return { mod, calls, tick: (ms: number) => (now += ms), line: (t: string) => line({ payload: t }) };
}

test("the connecting panel shows after 1 s, keeps the newest lines, and fails with the cause and its buttons", async () => {
  const p = panel();
  p.mod.connectAttempt();
  await Promise.resolve();
  p.line("Connecting to p0p [10.0.0.4] port 22.");
  assert.equal(main.children.length, 0, "a fast connect shows nothing");
  assert.match(label.textContent, /^Connecting to p0p \[10.0.0.4\] port 22\. · 0 s$/);
  p.tick(1500);
  p.line("Authenticated to p0p ([10.0.0.4]:22) using \"publickey\".");
  const shown = main.all().find((e) => e.id === "connect-panel")!;
  assert.ok(shown, "after 1 s it shows");
  assert.ok(shown.all().some((e) => e.textContent === "Connecting to p0p"));
  assert.ok(shown.all().some((e) => e.className === "cp-line now" && e.all().some((c) => c.textContent.startsWith("Authenticated"))));
  p.mod.connectFailed("p0p did not answer in 15 s. Check the network.", 20_000);
  const failed = main.all().find((e) => e.id === "connect-panel")!;
  const words = failed.all().map((e) => e.textContent);
  assert.ok(words.includes("Cannot connect to p0p") && words.includes("Next try in 20 s"));
  assert.ok(words.includes("p0p did not answer in 15 s. Check the network."));
  assert.equal(label.textContent, "p0p needs attention");
  for (const b of ["Check…", "Copy output"]) for (const f of failed.all().find((e) => e.textContent === b)!.listeners.click) f();
  assert.equal(p.calls[0], "check");
  assert.match(p.calls[1], /^copy 0\.0  Connecting to p0p .*\n1\.5  Authenticated.*\np0p did not answer/);
  p.mod.connectDone();
  assert.equal(main.children.length, 0, "success: the panel goes");
});
