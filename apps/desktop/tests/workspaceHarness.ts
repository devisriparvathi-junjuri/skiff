// Execute the real UI modules without a webview, filesystem writes or a daemon.
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

class Element {
  tag: string;
  className = "";
  textContent = "";
  type = "";
  value = "";
  checked = false;
  disabled = false;
  indeterminate = false;
  children: Element[] = [];
  dataset: Record<string, string> = {};
  parent: Element | null = null;
  listeners = new Map<string, (() => void)[]>();
  document: { activeElement: Element | null };

  constructor(tag: string, document: { activeElement: Element | null }) {
    this.tag = tag;
    this.document = document;
  }
  append(...children: Element[]) {
    for (const child of children) {
      this.children.push(child);
      child.parent = this;
    }
  }
  setAttribute() {}
  removeAttribute() {}
  addEventListener(event: string, run: () => void) {
    this.listeners.set(event, [...(this.listeners.get(event) ?? []), run]);
  }
  fire(event: string) {
    for (const run of this.listeners.get(event) ?? []) run();
  }
  click() { this.fire("click"); }
  focus() { this.document.activeElement = this; }
  select() {}
  remove() {
    if (this.parent) this.parent.children = this.parent.children.filter((child) => child !== this);
    this.parent = null;
  }
  get isConnected(): boolean { return this.tag === "body" || !!this.parent?.isConnected; }
  querySelectorAll() { return []; }
}

export function moduleFromSource(path: string, imports: Record<string, unknown>, globals: Record<string, unknown>, expose: string) {
  const source = readFileSync(new URL(path, import.meta.url), "utf8") + expose;
  const code = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const context = { exports: {} as any, require: (name: string) => (Object.hasOwn(imports, name) ? imports[name] : words(name) ?? imports[name] ?? {}), ...globals };
  vm.runInNewContext(code, context, { filename: path });
  return context.exports;
}

// The UI words: the real English catalog, unless a test passes its own.
let catalog: { en: any; i18n: any } | undefined;
function words(name: string) {
  if (!/(^|\/)i18n(\/en)?$/.test(name)) return undefined;
  if (!catalog) {
    const en = moduleFromSource("../src/i18n/en.ts", {}, {}, "");
    const es = moduleFromSource("../src/i18n/es.ts", {}, {}, "");
    const zh = moduleFromSource("../src/i18n/zh.ts", {}, {}, "");
    catalog = { en, i18n: moduleFromSource("../src/i18n/index.ts", { "./en": en, "./es": es, "./zh": zh }, { navigator: { language: "en-US" } }, "") };
  }
  return name.endsWith("/en") ? catalog.en : catalog.i18n;
}

export const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

export function workspaceHarness() {
  const document = {
    activeElement: null as Element | null,
    getElementById: () => null,
    querySelector: () => null,
    body: null as unknown as Element,
  };
  const h = (tag: string, cls = "", text = "") => {
    const el = new Element(tag, document);
    el.className = cls;
    el.textContent = text;
    return el;
  };
  document.body = h("body");
  const events = new Map<string, () => void>();
  const files = new Map<string, any>();
  const stored = new Map<string, string>();
  const S = {
    projects: [{ name: "A", path: "/a", fleet: "F" as string | null }, { name: "B", path: "/b", fleet: null as string | null }],
    closedProjects: [], selectedProject: null as string | null,
  };
  const calls = { approvedExits: 0, saves: 0, guarded: false, failSave: false };
  const window = {
    onCloseRequested: async (run: (e: { preventDefault(): void }) => void) => {
      events.set("window-close", () => run({ preventDefault() {} }));
      return () => {};
    },
  };
  let profile: any;
  const invoke = async (command: string, args: any) => {
    switch (command) {
      case "guard_quit": calls.guarded = events.has("skiff:quit-requested"); return;
      case "quit_app": calls.approvedExits++; return;
      case "workspace_launch_file": return null;
      case "workspace_read": {
        const value = files.get(args.path);
        if (!value) throw new Error("file missing");
        return structuredClone(value);
      }
      case "workspace_write":
        calls.saves++;
        if (calls.failSave) throw new Error("disk full");
        files.set(args.path, { path: args.path, profile: structuredClone(args.profile), revision: "saved" });
        return "saved";
      default: throw new Error(`Unexpected command: ${command}`);
    }
  };
  const imports = {
    "@tauri-apps/api/core": { invoke },
    "@tauri-apps/api/event": { listen: async (name: string, run: () => void) => { events.set(name, run); return () => {}; } },
    "@tauri-apps/api/window": { getCurrentWindow: () => window },
    "@tauri-apps/plugin-dialog": { save: async () => null },
    "../app/state": { S },
    "../app/renderRequest": { render() {} },
    "../ui/alerts": { showError() {} },
    "../ui/dom": { h, button: (cls: string, text: string, run: () => void) => {
      const b = h("button", cls, text); b.addEventListener("click", run); return b;
    } },
    "../ui/dialogParts": { dialogFrame: () => ({ overlay: h("div"), panel: h("div") }) },
    "./tree": {
      fleetKey: (name: string) => `fleet:${name}`, projectKey: (name: string) => `project:${name}`,
      useTreeView() {}, treeSections: () => [{ projects: S.projects.filter((p) => profile.shows(p)) }],
    },
    "./view": { selectProject: (name: string) => { S.selectedProject = name; } },
  };
  profile = moduleFromSource("../src/workspace/profile.ts", imports, {
    document, requestAnimationFrame: (run: () => void) => run(),
    window: { addEventListener() {} }, setInterval() {},
    localStorage: { getItem: (key: string) => stored.get(key) ?? null,
      setItem: (key: string, value: string) => stored.set(key, value), removeItem: (key: string) => stored.delete(key) },
  }, "");
  const elements = (): Element[] => {
    const walk = (el: Element): Element[] => [el, ...el.children.flatMap(walk)];
    return walk(document.body);
  };
  const button = (label: string) => {
    const found = elements().find((el) => el.tag === "button" && el.textContent === label);
    if (!found) throw new Error(`No button: ${label}`);
    return found;
  };
  const fixture = (path: string, projects = ["/a"], fleets: string[] = []) => {
    files.set(path, { path, revision: "read", profile: {
      name: path, projects, fleets, folded_projects: [], folded_fleets: [], selected: null,
    } });
  };
  return { profile, S, calls, files, events, button, elements, fixture };
}
