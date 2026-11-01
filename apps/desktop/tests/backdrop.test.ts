import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real backdrop module over a controlled Host: each image belongs to
 *  the data connection it was read on. A new connection reads the shown one
 *  again and revokes the old URL; a reply from an older one is never kept. */
function backdrop() {
  let epoch = 1;
  let made = 0;
  const pending: { resolve: (v: unknown) => void; reject: (e: unknown) => void }[] = [];
  const revoked: string[] = [];
  const style = new Map<string, string>();
  const project = { name: "web", background: "/srv/web/bg.png", background_opacity: 80 };
  const mod = moduleFromSource(
    "../src/appearance/backdrop.ts",
    {
      "../app/hostCaps": { hostInvoke: () => new Promise((resolve, reject) => pending.push({ resolve, reject })) },
      "../app/dataPlane": { dataEpoch: () => epoch },
      "../terminal/terminalHost": { host: { classList: { toggle() {} }, style: { setProperty: (k: string, v: string) => style.set(k, v) } } },
      "../app/state": { S: { focused: null, selectedProject: "web", projects: [project] }, sessions: new Map() },
      "../app/stateQueries": { place: () => undefined },
    },
    {
      Promise, Map, Set, Date, Blob: class {},
      URL: { createObjectURL: () => `blob:${++made}`, revokeObjectURL: (u: string) => revoked.push(u) },
      console: { error() {} },
    },
    "",
  );
  return { mod, pending, revoked, image: () => style.get("--bg-image") ?? "none", bump: () => epoch++ };
}

test("a new data connection reads a cached background again and revokes the old one", async () => {
  const b = backdrop();
  b.mod.applyBackdrop();
  b.pending[0].resolve(new ArrayBuffer(1));
  await flush();
  assert.equal(b.image(), 'url("blob:1")');
  // Rendering again on the same connection reads nothing.
  b.mod.applyBackdrop();
  assert.equal(b.pending.length, 1);
  // Reconnect: asked again; the old image stays until the new bytes come.
  b.bump();
  b.mod.reloadBackdrops();
  assert.equal(b.pending.length, 2, "the cached image is read again");
  assert.equal(b.image(), 'url("blob:1")');
  b.pending[1].resolve(new ArrayBuffer(1));
  await flush();
  assert.equal(b.image(), 'url("blob:2")');
  assert.deepEqual(b.revoked, ["blob:1"]);
});

test("a reply from an older data connection is dropped and asked again", async () => {
  const b = backdrop();
  b.mod.applyBackdrop();
  b.bump();
  b.mod.reloadBackdrops();
  // Still pending from connection 1: not asked twice at once.
  assert.equal(b.pending.length, 1);
  b.pending[0].resolve(new ArrayBuffer(1));
  await flush();
  assert.equal(b.image(), "none", "the old connection's bytes are never shown");
  assert.equal(b.pending.length, 2, "asked again on the new connection");
  b.pending[1].resolve(new ArrayBuffer(1));
  await flush();
  assert.equal(b.image(), 'url("blob:1")');
});

test("a failure on an older connection does not hold the retry wait; one on the new connection drops the old image", async () => {
  const b = backdrop();
  b.mod.applyBackdrop();
  b.pending[0].reject(new Error("gone"));
  await flush();
  // Failed on connection 1: waits, unless the connection changes.
  b.mod.applyBackdrop();
  assert.equal(b.pending.length, 1);
  b.bump();
  b.mod.reloadBackdrops();
  assert.equal(b.pending.length, 2);
  b.pending[1].resolve(new ArrayBuffer(1));
  await flush();
  assert.equal(b.image(), 'url("blob:1")');
  b.bump();
  b.mod.reloadBackdrops();
  b.pending[2].reject(new Error("not an image any more"));
  await flush();
  assert.equal(b.image(), "none");
  assert.deepEqual(b.revoked, ["blob:1"]);
});
