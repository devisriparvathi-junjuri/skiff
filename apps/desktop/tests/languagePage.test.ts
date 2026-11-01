import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// The real settings.ts. Settings › Language has the language picker;
// Settings › Appearance does not.
class El {
  className: string;
  textContent: string;
  type = "";
  id = "";
  hidden = false;
  children: El[] = [];
  dataset: Record<string, string> = {};
  style = {};
  listeners = new Map<string, (() => void)[]>();
  constructor(_tag: string, cls = "", text = "") {
    this.className = cls;
    this.textContent = text;
  }
  append(...kids: El[]) { this.children.push(...kids); }
  appendChild(k: El) { this.children.push(k); return k; }
  replaceChildren(...kids: El[]) { this.children = kids; }
  setAttribute() {}
  addEventListener(ev: string, run: () => void) { this.listeners.set(ev, [...(this.listeners.get(ev) ?? []), run]); }
  querySelector() { return null; }
  querySelectorAll() { return []; }
  contains() { return false; }
  all(): El[] { return [this, ...this.children.flatMap((c) => c.all())]; }
}
const h = (tag: string, cls = "", text = "") => new El(tag, cls, text);

function settingsPage() {
  const body = h("div");
  const invoke = async (cmd: string) => (cmd === "get_appearance" ? { language: null } : null);
  const mod = moduleFromSource("../src/appearance/settings.ts", {
    "@tauri-apps/api/core": { invoke },
    "../ui/dom": { h },
    "../ui/dialogParts": { escapeButton: () => h("button", "set-x") },
    "./themeCards": { themeGrid: () => h("div", "tc-grid") },
    "./hostSettings": { createHostsTab: () => ({ draw: () => h("div"), load: async () => {}, open() {}, focusServer() {} }) },
    "./diskUse": { diskState: () => ({}) },
    "./imagesPage": { imagesState: () => ({}) },
    "./daemonActivity": { activityState: () => ({}), stopActivity() {} },
    "./rowTable": { tableState: () => ({}) },
    "../terminal/images": { onImagesChanged() {} },
    "../app/hosts": { isRemote: () => false },
    "../app/hostCaps": { hostCan: () => true, hostInvoke: async () => null },
    "../app/state": { S: {} },
  }, {
    document: { getElementById: () => body, activeElement: null, body },
    window: { addEventListener() {} },
    navigator: { language: "en-US" },
    HTMLElement: El, HTMLInputElement: class {}, HTMLButtonElement: class {},
  }, "");
  const look = { themes: () => [], current: () => null, load: async () => {}, set() {} };
  const settings = mod.createSettings(() => {}, () => {}, look);
  const root = body.children[0];
  const picker = () => root.all().find((e) => e.className.includes("set-seg") && e.children.some((b) => b.textContent === "Español"));
  return { settings, root, picker };
}

test("Settings › Language shows the language picker, and Appearance does not", async () => {
  const s = settingsPage();
  await s.settings.open("language");
  const tabs = s.root.all().filter((e) => e.className.startsWith("set-tab")).map((e) => e.textContent);
  assert.deepEqual(tabs.slice(3, 6), ["Appearance", "Language", "Open with"]);
  assert.ok(s.root.all().some((e) => e.className === "set-h" && e.textContent === "Language"));
  const seg = s.picker();
  assert.ok(seg, "the Language page has the picker");
  assert.deepEqual(seg!.children.map((b) => b.textContent).slice(1), ["English", "Español", "简体中文"]);
  await s.settings.open("appearance");
  assert.ok(s.root.all().some((e) => e.className === "set-h" && e.textContent === "Appearance"));
  assert.equal(s.picker(), undefined, "Appearance has no language picker");
});
