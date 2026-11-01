import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** A stand-in element: enough for the Hosts tab to build and be clicked. */
class El {
  children: El[] = [];
  attrs: Record<string, string> = {};
  listeners: Record<string, ((e: unknown) => void)[]> = {};
  classList = { toggle() {}, add() {} };
  value = "";
  placeholder = "";
  disabled = false;
  id = "";
  tag: string;
  className: string;
  textContent: string;
  constructor(tag: string, className = "", textContent = "") {
    this.tag = tag;
    this.className = className;
    this.textContent = textContent;
  }
  append(...c: El[]) { this.children.push(...c.filter((x) => x instanceof El)); }
  prepend(...c: El[]) { this.children.unshift(...c.filter((x) => x instanceof El)); }
  setAttribute(k: string, v: string) { this.attrs[k] = v; }
  getAttribute(k: string) { return this.attrs[k] ?? null; }
  addEventListener(k: string, f: (e: unknown) => void) { (this.listeners[k] ??= []).push(f); }
  focus() {}
  all(): El[] { return [this, ...this.children.flatMap((c) => c.all())]; }
  find(text: string) { return this.all().find((e) => e.textContent === text); }
  click() { for (const f of this.listeners.click ?? []) f({ stopPropagation() {} }); }
  getBoundingClientRect() { return { left: 0, bottom: 0 }; }
  replaceChildren(...c: El[]) { this.children = c; }
  replaceWith(e: El) { this.textContent = ""; this.children = [e]; }
}

/** The row menu (⋯) of the Host row, then one of its items. */
function pickFromMenu(x: ReturnType<typeof tab>, label: string) {
  const more = x.page().all().find((e) => e.className === "c-menu")!;
  more.click();
  x.menus.at(-1)!.entries.find((e: any) => e.label === label)!.run();
}

function tab(report?: unknown) {
  const calls: [string, unknown][] = [];
  const steps: ((e: unknown) => void)[] = [];
  const runs: any[] = [];
  const menus: { title: string; entries: any[] }[] = [];
  const dialogs: { title: string; body?: string; tone?: string; content?: El[]; submit: () => Promise<void>; onClose?: () => void }[] = [];
  let prompt: { value?: string; submit: (v: string) => Promise<void> } | null = null;
  const hosts = [{ id: "h_dev", name: "devbox", destination: "devbox", user: "me", port: 2222, state: "attention", skiff: "1.9.0", update: true }];
  const invoke = async (c: string, a?: unknown) => {
    calls.push([c, a]);
    if (c === "resolve_ssh") return { hostname: "devbox.lan", user: "me", port: 2222, jump: "" };
    if (c === "ssh_aliases") return [{ name: "devbox", goes_to: "me@devbox.lan" }];
    if (c === "check_host" && report) return report;
    if (c === "uninstall_preview") return { managed: true, sessions: ["build", "shell"] };
    if (c === "update_skiff")
      return (a as { endSessions: boolean }).endSessions
        ? { result: "done", from: "1.9.0", restarted: true, version: "2.0.0", source: "release" }
        : { result: "no_handover", from: "1.9.0", sessions: ["build", "api"], version: "2.0.0", source: "release" };
    if (c === "install_skiff") return { source: "release", version: "2.0.0", cli_linked: true, changed: true, how: "systemd", linger: "yes", running: true };
    if (c === "check_host") return { ok: true, rows: [{ row: "reach", title: "Reach the server", state: "passed", text: "devbox.lan answers. 12 ms." }] };
  };
  const h = (tag: string, cls?: string, text?: string) => new El(tag, cls, text);
  const mod = moduleFromSource(
    "../src/appearance/hostSettings.ts",
    {
      "@tauri-apps/api/core": { invoke },
      "@tauri-apps/api/event": { listen: async (_e: string, f: (e: unknown) => void) => (steps.push(f), () => {}) },
      "../ui/dom": { h, button: (cls: string, text: string, run: () => void) => { const b = h("button", cls, text); b.addEventListener("click", run); return b; } },
      "../ui/runes": { rune: () => new El("svg") },
      "../ui/confirm": { confirmAction: async () => true, promptAction: (o: typeof prompt) => void (prompt = o) },
      "../ui/dialogParts": { actionDialog: (o: (typeof dialogs)[number]) => void dialogs.push(o) },
      "./hostProgress": { runDialog: (o: any) => void runs.push(o) },
      "../ui/contextMenu": { ctxMenu: { open: (_x: number, _y: number, title: string, entries: any[]) => void menus.push({ title, entries }) } },
      "@tauri-apps/api/app": { getVersion: async () => "2.0.0" },
      "../platform/clipboard": { copyText: async (t: string) => void calls.push(["copy", t]) },
      "../app/hosts": { knownHosts: () => hosts, loadHosts: async () => {}, localHostName: () => "This computer", shownHost: () => null, pick: (id: string) => void calls.push(["pick", id]) },
    },
    { document: { getElementById: () => null, querySelector: () => null }, setTimeout, clearTimeout, Promise, Map },
    "",
  );
  let page: El = new El("div");
  const t = mod.createHostsTab(() => (page = t.draw()));
  return { t, calls, steps, runs, menus, dialogs, page: () => page, prompt: () => prompt!, redraw: () => (page = t.draw()) };
}

test("Rename sends only the new name, for the Host's id", async () => {
  const x = tab();
  await x.t.load();
  x.redraw();
  assert.equal(x.page().find("me@devbox:2222")?.className, "c-server mono", "the Server column shows the destination");
  pickFromMenu(x, "Rename…");
  assert.equal(x.prompt().value, "devbox");
  await x.prompt().submit("Dev box");
  assert.equal(JSON.stringify(x.calls.find(([c]) => c === "rename_host")), JSON.stringify(["rename_host", { id: "h_dev", name: "Dev box" }]));
});

test("Edit opens the form with the Host's values and saves them under its id", async () => {
  const x = tab();
  await x.t.load();
  x.redraw();
  pickFromMenu(x, "Edit…");
  assert.ok(x.page().find("Edit devbox"));
  // User and port were set, so More is open and shows them.
  const port = x.page().all().find((e) => e.attrs["aria-label"] === "Port")!;
  assert.equal(port.value, "2222");
  port.value = "2200";
  for (const f of port.listeners.input) f({});
  x.redraw();
  // The check finds the socket: the form has no field for it.
  assert.equal(x.page().all().find((e) => e.attrs["aria-label"] === "Socket on the server"), undefined);
  assert.equal(x.page().find("Save"), undefined, "a changed port needs Connect");
  x.page().find("Connect")!.click();
  for (let i = 0; i < 6; i++) await flush();
  const edit = x.calls.find(([c]) => c === "edit_host")!;
  assert.equal(JSON.stringify(edit), JSON.stringify(["edit_host", { id: "h_dev", form: { destination: "devbox", name: "devbox", user: "me", port: "2200" } }]));
  // Connect checks the Host at once, and a passed check offers the switch.
  assert.ok(x.calls.some(([c, a]) => c === "check_host" && JSON.stringify(a) === '{"id":"h_dev"}'));
  assert.ok(x.page().find("devbox.lan answers. 12 ms."));
  x.page().find("Switch to devbox")!.click();
  assert.ok(x.calls.some(([c, a]) => c === "pick" && a === "h_dev"));
});

test("one Skiff per server: the check list shows the one skiffd and offers no pick", async () => {
  const x = tab({
    ok: true,
    socket: "/run/user/1000/skiff/skiffd.sock",
    rows: [{ row: "sessions", title: "Sessions service (skiffd)", state: "passed", text: "skiffd 2.1.0 answers.", fix: null }],
  });
  await x.t.load();
  x.t.open({ check: "h_dev" });
  for (let i = 0; i < 4; i++) await flush();
  x.redraw();
  assert.ok(x.page().find("skiffd 2.1.0 answers."));
  assert.equal(x.page().find("Which Skiff"), undefined);
  assert.equal(x.calls.find(([c]) => c === "pick_socket"), undefined);
});

test("a Host that needs attention opens its check list from the menu link", async () => {
  const x = tab();
  await x.t.load();
  x.t.open({ check: "h_dev" });
  x.redraw();
  assert.ok(x.page().find("Check devbox"));
  assert.ok(x.page().find("Checking…"), "the first row runs while the rest wait");
  for (let i = 0; i < 4; i++) await flush();
  x.redraw();
  assert.ok(x.page().find("Reach the server"));
  assert.equal(x.page().find("Needs attention"), undefined);
});

test("Install key sends the password once, clears the field, then checks again", async () => {
  const failed = {
    ok: false,
    rows: [
      { row: "reach", title: "Reach the server", state: "passed", text: "devbox.lan answers.", fix: null },
      { row: "login", title: "Log in without a prompt", state: "failed", text: "devbox does not accept a key from this computer yet.", fix: "install_key" },
    ],
  };
  const x = tab(failed);
  await x.t.load();
  x.t.open({ check: "h_dev" });
  for (let i = 0; i < 4; i++) await flush();
  x.redraw();
  x.page().find("Install key…")!.click();
  const d = x.dialogs.at(-1)!;
  assert.equal(d.title, "Log in to devbox once");
  const field = d.content![0];
  assert.equal((field as unknown as { type: string }).type, "password");
  field.value = "hunter2";
  const checks = x.calls.filter(([c]) => c === "check_host").length;
  await d.submit();
  for (let i = 0; i < 4; i++) await flush();
  const installs = x.calls.filter(([c]) => c === "install_key");
  assert.equal(JSON.stringify(installs), JSON.stringify([["install_key", { id: "h_dev", password: "hunter2" }]]));
  assert.equal(field.value, "", "the page keeps no password");
  assert.equal(x.calls.filter(([c]) => c === "check_host").length, checks + 1, "the check runs again");
});

test("Copy public key copies the line Rust gives and says it is safe to send", async () => {
  const x = tab({ ok: false, rows: [{ row: "login", title: "Log in without a prompt", state: "failed", text: "keys only", fix: "copy_public_key" }] });
  await x.t.load();
  x.t.open({ check: "h_dev" });
  for (let i = 0; i < 4; i++) await flush();
  x.redraw();
  x.page().find("Copy public key")!.click();
  for (let i = 0; i < 4; i++) await flush();
  assert.ok(x.calls.some(([c]) => c === "public_key"));
  x.redraw();
  assert.ok(x.page().all().some((e) => e.textContent.startsWith("Public key copied.")));
});

test("Install Skiff asks once, names the folder, installs, then checks again", async () => {
  const x = tab({
    ok: false,
    platform: ["linux", "x86_64"],
    version: "2.0.0",
    rows: [{ row: "skiff", title: "Skiff on the server", state: "failed", text: "Skiff is not installed on devbox.", fix: "install_skiff" }],
  });
  await x.t.load();
  x.t.open({ check: "h_dev" });
  for (let i = 0; i < 4; i++) await flush();
  x.redraw();
  x.page().find("Install Skiff…")!.click();
  const d = x.runs.at(-1)!;
  assert.equal(d.title, "Install Skiff on devbox?");
  assert.equal(d.runningTitle, "Installing Skiff on devbox");
  assert.equal(d.steps.length, 6);
  assert.equal(x.calls.filter(([c]) => c === "install_skiff").length, 0, "nothing runs before the confirm");
  const checks = x.calls.filter(([c]) => c === "check_host").length;
  assert.equal(d.body, "Installs Skiff 2.0.0 in your home folder on devbox. It keeps running after a restart.");
  const r = await d.run();
  d.done(r, 14, "0.0  Check devbox");
  for (let i = 0; i < 4; i++) await flush();
  assert.equal(JSON.stringify(x.calls.filter(([c]) => c === "install_skiff")), JSON.stringify([["install_skiff", { id: "h_dev" }]]));
  assert.equal(x.calls.filter(([c]) => c === "check_host").length, checks + 1);
  // The check list row shows what the install did, its time, and the output.
  x.redraw();
  const all = x.page().all();
  assert.ok(all.some((e) => e.textContent === "Installed Skiff 2.0.0."), "the row says it");
  assert.ok(all.some((e) => e.textContent === "14 s"));
  all.find((e) => e.textContent === "Copy output")!.click();
  assert.ok(x.calls.some(([c, t]) => c === "copy" && t === "0.0  Check devbox"));
});

test("Remove asks first; the uninstall box shows for a managed install and names the sessions that end", async () => {
  const x = tab();
  await x.t.load();
  x.redraw();
  pickFromMenu(x, "Remove…");
  const d = x.dialogs.at(-1)!;
  assert.equal(d.title, "Remove devbox?");
  for (let i = 0; i < 4; i++) await flush();
  const all = d.content!.flatMap((c) => c.all());
  const box = all.find((e) => e.attrs.role === "switch")!;
  assert.ok(box, "the switch shows once the server says Skiff installed it");
  assert.ok(all.some((e) => e.textContent.includes("This ends: build, shell.")));
  assert.equal(x.calls.filter(([c]) => c === "remove_host").length, 0, "nothing changes before the confirm");
  box.click();
  assert.equal(box.attrs["aria-checked"], "true");
  assert.match(d.content![0].textContent, /Your other files stay/);
  await d.submit();
  assert.equal(JSON.stringify(x.calls.filter(([c]) => c === "remove_host")), JSON.stringify([["remove_host", { id: "h_dev", uninstall: true }]]));
});

test("Install Skiff on a Mac names the LaunchAgent and when it starts again", async () => {
  const x = tab({
    ok: false,
    platform: ["macos", "aarch64"],
    version: "2.0.0",
    rows: [{ row: "skiff", title: "Skiff on the server", state: "failed", text: "Skiff is not installed on devbox.", fix: "install_skiff" }],
  });
  await x.t.load();
  x.t.open({ check: "h_dev" });
  for (let i = 0; i < 4; i++) await flush();
  x.redraw();
  x.page().find("Install Skiff…")!.click();
  const body = x.runs.at(-1)!.body ?? "";
  assert.equal(body, "Installs Skiff 2.0.0 in your home folder on devbox. It starts when you log in there.");
});

test("Update Skiff asks once; when sessions would end, a red dialog names them first", async () => {
  const x = tab({
    ok: true,
    platform: ["linux", "x86_64"],
    version: "2.0.0",
    rows: [{ row: "skiff", title: "Skiff on the server", state: "warning", text: "Update available. devbox runs Skiff 1.9.0. This computer runs 2.0.0.", fix: "update_skiff" }],
  });
  await x.t.load();
  x.t.open({ check: "h_dev" });
  for (let i = 0; i < 4; i++) await flush();
  x.redraw();
  x.page().find("Update Skiff…")!.click();
  assert.equal(x.runs.at(-1)!.title, "Update Skiff on devbox?");
  assert.equal(x.calls.filter(([c]) => c === "update_skiff").length, 0, "nothing runs before the confirm");
  const first = x.runs.at(-1)!;
  first.done(await first.run(), 3, "");
  for (let i = 0; i < 4; i++) await flush();
  const red = x.runs.at(-1)!;
  assert.equal(red.tone, "danger");
  assert.ok(red.body!.includes("This ends: build, api."), red.body);
  red.done(await red.run(), 5, "");
  for (let i = 0; i < 4; i++) await flush();
  assert.equal(
    JSON.stringify(x.calls.filter(([c]) => c === "update_skiff")),
    JSON.stringify([["update_skiff", { id: "h_dev", endSessions: false, reinstall: false }], ["update_skiff", { id: "h_dev", endSessions: true, reinstall: false }]]),
  );
});
