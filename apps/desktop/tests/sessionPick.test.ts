// The skiffd activity list's selection and its Abandon Ship dialog text (#184). Run: pnpm test
import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// The real sessionPick.ts, with the real catalog for its words.
const { abandonShip, emptyPick, keepLive, moveTo, NAMES_SHOWN, pickAll, picked, pickedText, pickRow, sinkLabel } = moduleFromSource("../src/appearance/sessionPick.ts", {}, {}, "");

const ORDER = ["a", "b", "c", "d", "e"];
const sel = (p: ReturnType<typeof emptyPick>) => picked(p, ORDER).join("");

test("a click selects that row alone", () => {
  const p = emptyPick();
  pickRow(p, ORDER, "b");
  pickRow(p, ORDER, "d");
  assert.equal(sel(p), "d");
  assert.equal(p.now, "d");
});

test("Shift+click selects the range from the anchor, both ways", () => {
  const p = emptyPick();
  pickRow(p, ORDER, "b");
  pickRow(p, ORDER, "d", { range: true });
  assert.equal(sel(p), "bcd");
  // The anchor stays: a second Shift+click replaces the range.
  pickRow(p, ORDER, "a", { range: true });
  assert.equal(sel(p), "ab");
});

test("Ctrl+click adds and drops one row, and Ctrl+Shift+click adds a range", () => {
  const p = emptyPick();
  pickRow(p, ORDER, "a");
  pickRow(p, ORDER, "c", { toggle: true });
  assert.equal(sel(p), "ac");
  pickRow(p, ORDER, "a", { toggle: true });
  assert.equal(sel(p), "c");
  // The anchor is the last row Ctrl+clicked.
  pickRow(p, ORDER, "d", { toggle: true });
  pickRow(p, ORDER, "e", { toggle: true, range: true });
  assert.equal(sel(p), "cde");
});

test("Shift+click with no anchor selects the one row", () => {
  const p = emptyPick();
  pickRow(p, ORDER, "c", { range: true });
  assert.equal(sel(p), "c");
});

test("arrows select, extend with Shift, and move only with Ctrl", () => {
  const p = emptyPick();
  moveTo(p, ORDER, "b");
  moveTo(p, ORDER, "c", { range: true });
  assert.equal(sel(p), "bc");
  moveTo(p, ORDER, "e", { keep: true });
  assert.equal(sel(p), "bc");
  assert.equal(p.now, "e");
});

test("Ctrl+A selects every row", () => {
  const p = emptyPick();
  pickAll(p, ORDER);
  assert.equal(sel(p), "abcde");
  assert.equal(p.now, "a");
});

test("a refresh drops ended sessions from the selection", () => {
  const p = emptyPick();
  pickAll(p, ORDER);
  p.now = p.anchor = "c";
  keepLive(p, ["a", "b", "d"]);
  assert.deepEqual([...p.sel].sort(), ["a", "b", "d"]);
  assert.equal(p.now, null);
  assert.equal(p.anchor, null);
});

test("the button reads Sink, and Sink Selected with more than one row", () => {
  assert.equal(sinkLabel(0), "Sink…");
  assert.equal(sinkLabel(1), "Sink…");
  assert.equal(sinkLabel(2), "Sink Selected…");
  assert.equal(pickedText(0), "");
  assert.equal(pickedText(3), "3 selected");
});

test("Abandon Ship names one session", () => {
  const d = abandonShip(["fix login"]);
  assert.equal(d.title, "Abandon Ship");
  assert.equal(d.body, "Kills fix login.\nUnsaved work in its terminal is lost.");
  assert.equal(d.action, "Sink session");
});

test("Abandon Ship names a few sessions", () => {
  const d = abandonShip(["one", "two", "three"]);
  assert.equal(d.title, "Abandon Ship");
  assert.equal(d.body, "Kills 3 sessions:\none\ntwo\nthree\nUnsaved work in their terminals is lost.");
  assert.equal(d.action, "Sink 3 sessions");
});

test("Abandon Ship gives the count when there are many", () => {
  const names = Array.from({ length: NAMES_SHOWN + 1 }, (_, i) => `s${i}`);
  const d = abandonShip(names);
  assert.equal(d.body, `Kills ${NAMES_SHOWN + 1} sessions.\nUnsaved work in their terminals is lost.`);
  assert.ok(!d.body.includes("s0"));
  assert.equal(d.action, `Sink ${NAMES_SHOWN + 1} sessions`);
});
