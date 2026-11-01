import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real hosts module over a fake `invoke`, `listen` and document. */
type Host = { id: string; name: string; destination: string; user: string | null; port: number | null; socket: string | null; state: string };
type Answer = { selected: string | null; localName?: string; hosts: Host[]; clipboard: boolean };

/** `live`: what Rust's `host_clipboard` answers now, for the identity the
 *  window's live connection accepted and the file as it is now. */
function hosts(answer: Answer, live = { clipboard: answer.clipboard }) {
  const calls: [string, unknown][] = [];
  const listeners = new Map<string, () => void>();
  let reloads = 0;
  const invoke = async (command: string, args?: unknown) => {
    calls.push([command, args]);
    if (command === "list_hosts") return structuredClone(answer);
    if (command === "host_clipboard") return live.clipboard;
  };
  const document = { title: "", getElementById: () => null };
  const mod = moduleFromSource(
    "../src/app/hosts.ts",
    {
      "@tauri-apps/api/core": { invoke },
      "@tauri-apps/api/event": { listen: async (name: string, run: () => void) => void listeners.set(name, run) },
      "../ui/contextMenu": { ctxMenu: { open() {} } },
      "../ui/confirm": { confirmAction: async () => true },
      "../ui/dom": { h: () => ({}) },
      "../ui/alerts": { showError(e: unknown) { throw e; } },
    },
    { document, location: { reload: () => reloads++ }, Promise, Set, structuredClone },
    "\nexports.__pick = pick;",
  );
  return { mod, calls, listeners, document, reloads: () => reloads };
}

const devbox: Host = { id: "h_dev", name: "devbox", destination: "jose@devbox.lan", user: null, port: null, socket: "/run/user/1000/skiff/skiffd.sock", state: "ok" };

/** The real osc52 module over the given hosts module; returns the texts it
 *  put on the clipboard and a way to feed it an OSC 52 sequence. */
function osc52(hostsMod: unknown) {
  const copied: string[] = [];
  let handler!: (data: string) => boolean;
  const mod = moduleFromSource(
    "../src/terminal/osc52.ts",
    {
      "../platform/clipboard": { copyText: async (t: string) => void copied.push(t) },
      "../ui/alerts": { showError(e: unknown) { throw e; } },
      "../app/hosts": hostsMod,
    },
    { atob, TextDecoder, Uint8Array },
    "",
  );
  mod.handleOsc52({ parser: { registerOscHandler: (_: number, f: (d: string) => boolean) => void (handler = f) } });
  return { copied, send: (text: string) => handler(`c;${btoa(text)}`) };
}

test("this Mac writes the clipboard; a remote Host only after it is allowed", async () => {
  const local = hosts({ selected: null, localName: "thr0w-deb", hosts: [devbox], clipboard: false });
  await local.mod.loadHosts();
  assert.equal(local.mod.isRemote(), false);
  assert.equal(await local.mod.clipboardWrites(), true);
  assert.equal(local.mod.clipboardAllowed(), true);
  assert.equal(local.document.title, "Skiff — This computer (thr0w-deb)");
  assert.equal(local.calls.filter(([c]) => c === "host_clipboard").length, 0);

  const remote = hosts({ selected: "h_dev", hosts: [devbox, { ...devbox, id: "h_other", name: "other", destination: "other" }], clipboard: false });
  await remote.mod.loadHosts();
  assert.equal(remote.mod.isRemote(), true);
  assert.equal(remote.mod.hostName(), "devbox");
  assert.equal(remote.document.title, "Skiff — devbox");
  assert.equal(await remote.mod.clipboardWrites(), false);
  assert.equal(remote.mod.clipboardAllowed(), false);

  const allowed = hosts({ selected: "h_dev", hosts: [devbox], clipboard: true });
  await allowed.mod.loadHosts();
  assert.equal(await allowed.mod.clipboardWrites(), true);
  assert.equal(allowed.mod.clipboardAllowed(), true);
});

/** Residual P2: window 1 stays connected to B; window 2 accepts C at the
 *  same destination and allows C. Window 1's page reloads: Rust answers for
 *  its live B connection, so OSC 52 from B writes nothing, and only Rust's
 *  answer at the time of each write counts, never a value the page loaded. */
test("OSC 52 follows the live identity Rust checks on each write, not the loaded list", async () => {
  // Window 1 after the reload: live B has no permission.
  const w1 = hosts({ selected: "h_dev", hosts: [devbox], clipboard: false });
  await w1.mod.loadHosts();
  const t1 = osc52(w1.mod);
  t1.send("from B");
  await flush();
  assert.deepEqual(t1.copied, []);
  assert.equal(w1.calls.filter(([c]) => c === "host_clipboard").length, 1, "asked Rust for this write");

  // A page that loaded a permission it no longer has: another window
  // accepted another identity since. The write asks again and is refused.
  const live = { clipboard: true };
  const stale = hosts({ selected: "h_dev", hosts: [devbox], clipboard: true }, live);
  await stale.mod.loadHosts();
  const t2 = osc52(stale.mod);
  t2.send("allowed");
  await flush();
  assert.deepEqual(t2.copied, ["allowed"]);
  live.clipboard = false;
  t2.send("after another window accepted C");
  await flush();
  assert.deepEqual(t2.copied, ["allowed"]);
  assert.equal(stale.mod.clipboardAllowed(), true, "the menu keeps what the page loaded until it reloads");

  // A failed check allows nothing.
  const failing = moduleFromSource(
    "../src/app/hosts.ts",
    {
      "@tauri-apps/api/core": {
        invoke: async (c: string) => {
          if (c === "list_hosts") return { selected: "h_dev", hosts: [devbox], clipboard: true };
          throw Error("no answer");
        },
      },
      "@tauri-apps/api/event": { listen: async () => {} },
    },
    { document: { title: "", getElementById: () => null }, Promise },
    "",
  );
  await failing.loadHosts();
  const t3 = osc52(failing);
  t3.send("x");
  await flush();
  assert.deepEqual(t3.copied, []);
});

test("picking a Host saves it, then reloads the page; the shown Host does nothing", async () => {
  const h = hosts({ selected: null, hosts: [devbox] });
  await h.mod.loadHosts();
  h.mod.__pick(null);
  await flush();
  assert.equal(h.calls.filter(([c]) => c === "select_host").length, 0);
  h.mod.__pick("h_dev");
  await flush();
  assert.equal(JSON.stringify(h.calls.at(-1)), JSON.stringify(["select_host", { id: "h_dev" }]));
  assert.equal(h.reloads(), 1);
});

test("a lost link turns input off once and starts one recovery", async () => {
  const h = hosts({ selected: "h_dev", hosts: [devbox] });
  await h.mod.loadHosts();
  let reconnects = 0;
  let onlines = 0;
  let finish!: () => void;
  h.mod.watchHostLink(
    () => {
      reconnects++;
      return new Promise<void>((r) => (finish = r));
    },
    () => onlines++,
  );
  await flush();
  const lost = h.listeners.get("skiff:host-lost")!;
  lost();
  assert.equal(h.mod.hostConnected(), false);
  assert.equal(reconnects, 1);
  finish();
  await flush();
  assert.equal(h.mod.hostConnected(), true);
  assert.equal(onlines, 1);
  lost();
  assert.equal(reconnects, 2);
});

/** Review P2 (race.mjs): the real hosts and daemon modules. The new link
 *  drops while recovery subscribes to events, and that subscription fails.
 *  The loss is kept, the whole recovery runs again, and only the second,
 *  clean run turns input back on and shows the Host connected. */
test("a second loss during recovery runs the whole recovery again before input is on", async () => {
  let lost!: () => void;
  const immediate = (f: () => void) => setImmediate(f);
  const hostsMod = moduleFromSource(
    "../src/app/hosts.ts",
    {
      "@tauri-apps/api/core": { invoke: async () => ({ selected: "fixture", hosts: [] }) },
      "@tauri-apps/api/event": { listen: async (_: string, cb: () => void) => void (lost = cb) },
    },
    { document: { title: "", getElementById: () => null }, Promise, setTimeout: immediate, console: { error() {} } },
    "",
  );
  await hostsMod.loadHosts();
  const noop = () => {};
  const elem = { querySelector: () => ({}), replaceChildren: noop, set textContent(_: string) {}, set title(_: string) {} };
  let muted = false;
  const mutes: boolean[] = [];
  let attaches = 0;
  let subscribes = 0;
  const daemon = moduleFromSource(
    "../src/app/daemon.ts",
    {
      "./connectPanel": { connectAttempt() {}, connectDone() {}, connectFailed() {}, connectWait: async () => {} },
      "@tauri-apps/api/core": { invoke: async () => ({ connected: true }), Channel: class {} },
      "@tauri-apps/api/event": { listen: async () => {} },
      "../ui/dom": { $: () => elem },
      "./daemonRestart": { daemonBadge: noop },
      "./skiffAgent": { noteDaemon: noop },
      "./hosts": hostsMod,
      "./render": { render: noop, scheduleRender: noop },
      "./state": { born: new Set(), gone: new Set(), sessions: new Map(), S: {} },
      "./seen": { quietSeen: noop },
      "../terminal/terminalRuntime": {
        muteInput: (x: boolean) => {
          muted = x;
          mutes.push(x);
        },
        reattachPanes: () => attaches++,
      },
      "./hostCaps": {
        loadHostCaps: async () => {},
        hostCan: () => false,
        hostInvoke: async (_: string, cmd: string) => {
          if (cmd === "subscribe_events" && ++subscribes === 1) {
            lost();
            throw Error("second connection dropped during resubscribe");
          }
          if (cmd === "list_sessions") return [];
        },
      },
    },
    { window: { addEventListener: noop }, document: { hasFocus: () => true }, console: { error: noop }, setTimeout: immediate, Promise, Map, Set },
    "",
  );
  let reconnects = 0;
  hostsMod.watchHostLink(async () => {
    reconnects++;
    await daemon.reconnectRemote();
  }, daemon.hostOnline);
  lost();
  for (let i = 0; i < 20; i++) await flush();
  assert.equal(reconnects, 2, "the failed recovery ran again");
  assert.equal(subscribes, 2, "events were subscribed again");
  assert.equal(attaches, 1, "panes reattach only in the clean run");
  assert.equal(hostsMod.hostConnected(), true);
  assert.equal(muted, false);
  // Input stayed off until the end: never on between the two runs.
  assert.deepEqual(mutes, [true, true, false]);
});
