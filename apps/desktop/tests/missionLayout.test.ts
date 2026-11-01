// Mission control's graph: the layout, the strings, the keys' moves and the key map.
import { test } from "node:test";
import assert from "node:assert/strict";
import * as G from "../src/workspace/missionLayout.ts";

const project = (name: string, cards: string[], extra: Partial<G.GProject> = {}): G.GProject => ({ key: `project:${name}`, name, skipper: null, cards, plans: [], folded: false, ...extra });

/** Admiral, a fleet with its Skipper and two projects, a project with its Skipper, a plain project, Other. */
function graph(): G.Graph {
  return {
    admiral: "ad",
    fleets: [{ key: "fleet:Web", name: "Web", skipper: "sk-web", projects: [project("storefront", ["s1", "s2", "s3"], { plans: [{ uid: "p1", title: "Release", cards: ["s1", "s3"], done: 1 }] }), project("checkout", ["s4", "s5"])] }],
    projects: [project("skiff", ["s6", "s7", "s8"], { skipper: "sk-skiff" }), project("gesher", ["s9", "s10"]), project("other", ["s11"])],
  };
}

const overlap = (a: G.Rect, b: G.Rect) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
const inside = (a: G.Rect, b: G.Rect) => a.x >= b.x && a.y >= b.y && a.x + a.w <= b.x + b.w && a.y + a.h <= b.y + b.h;

test("the layout keeps cards apart, inside their clusters, under their Skippers", () => {
  const p = G.placeGraph(graph(), {});
  const cards = [...p.cards.entries()];
  for (const [a, ra] of cards) for (const [b, rb] of cards) if (a < b) assert.ok(!overlap(ra, rb), `${a} and ${b} overlap`);
  assert.equal(p.cards.size, 14, "every session, Skipper and Admiral has a card");
  // Each session card sits inside its project's frame; each project frame inside its fleet's.
  const g = graph();
  for (const pr of [...g.fleets[0].projects, ...g.projects]) for (const id of pr.cards) assert.ok(inside(p.cards.get(id)!, p.clusters.get(pr.key)!), `${id} inside ${pr.key}`);
  for (const pr of g.fleets[0].projects) assert.ok(inside(p.clusters.get(pr.key)!, p.clusters.get("fleet:Web")!), `${pr.key} inside the fleet`);
  // The project frames of one fleet do not overlap each other, and the top blocks do not overlap.
  assert.ok(!overlap(p.clusters.get("project:storefront")!, p.clusters.get("project:checkout")!));
  assert.ok(!overlap(p.clusters.get("fleet:Web")!, p.clusters.get("project:skiff")!));
  assert.ok(!overlap(p.clusters.get("project:skiff")!, p.clusters.get("project:gesher")!));
  // Admiral is above everything, a Skipper above what it owns.
  const ad = p.cards.get("ad")!;
  for (const [id, r] of p.cards) if (id !== "ad") assert.ok(r.y >= ad.y + ad.h, `${id} under Admiral`);
  assert.ok(p.cards.get("sk-web")!.y + G.CARD_H <= p.clusters.get("project:storefront")!.y);
  assert.ok(p.cards.get("sk-skiff")!.y + G.CARD_H <= p.clusters.get("project:skiff")!.y);
  // A plan caption sits above its first card, inside the cluster.
  const plan = p.plans.find((pl) => pl.plan.uid === "p1")!;
  assert.ok(plan.y + plan.h <= p.cards.get("s1")!.y);
  assert.ok(inside(plan, p.clusters.get("project:storefront")!));
});

test("the outer grid wraps at three blocks a row, rows top-align, and a wide block takes a row alone", () => {
  // Six small projects: two rows of three, the second row under the tallest of the first.
  const g: G.Graph = { admiral: "ad", fleets: [], projects: [project("a", ["a1"]), project("b", ["b1", "b2", "b3"]), project("c", ["c1"]), project("d", ["d1"]), project("e", ["e1"]), project("f", ["f1"])] };
  const p = G.placeGraph(g, {});
  const c = (n: string) => p.clusters.get(`project:${n}`)!;
  assert.deepEqual([c("a").x, c("b").x, c("c").x], [0, G.SLOT_W + 48, 2 * (G.SLOT_W + 48)], "three slots across");
  assert.equal(c("a").y, c("b").y, "a row top-aligns");
  assert.equal(c("d").x, 0, "the fourth block starts the next row");
  assert.equal(c("d").y, c("a").y + c("b").h + 48, "the next row sits under the tallest block of the row above");
  assert.deepEqual([c("e").x, c("f").x], [c("b").x, c("c").x]);
  assert.ok(c("b").h > c("a").h, "b is the tallest: three cards in two columns");
  // Admiral centers on the grid, not on the content, so it stays put when blocks come and go.
  assert.equal(p.cards.get("ad")!.x, G.GRID_W / 2 - G.CARD_W / 2);
  assert.equal(G.placeGraph({ ...g, projects: g.projects.slice(0, 1) }, {}).cards.get("ad")!.x, p.cards.get("ad")!.x);
  // A project with seven cards is three columns wide: wider than a slot, so it takes a row by itself.
  const big = project("big", ["g1", "g2", "g3", "g4", "g5", "g6", "g7"]);
  const q = G.placeGraph({ admiral: null, fleets: [], projects: [project("a", ["a1"]), big, project("c", ["c1"])] }, {});
  const r = (n: string) => q.clusters.get(`project:${n}`)!;
  assert.ok(r("big").w > G.SLOT_W);
  assert.equal(r("big").x, 0);
  assert.ok(r("big").y > r("a").y, "big starts a new row");
  assert.ok(r("c").y > r("big").y, "the block after it starts another");
  assert.equal(r("c").x, 0);
});

test("a fleet lays its projects out three a row inside its frame, and a wide fleet takes a row alone", () => {
  const five = ["p1", "p2", "p3", "p4", "p5"].map((n) => project(n, [`${n}a`]));
  const g: G.Graph = { admiral: "ad", fleets: [{ key: "fleet:F", name: "F", skipper: "sk", projects: five }, { key: "fleet:G", name: "G", skipper: null, projects: [project("g1", ["x1"])] }], projects: [project("solo", ["s1"]), project("other", ["o1"])] };
  const p = G.placeGraph(g, {});
  const frame = p.clusters.get("fleet:F")!;
  const c = (n: string) => p.clusters.get(`project:${n}`)!;
  for (const n of ["p1", "p2", "p3", "p4", "p5"]) assert.ok(inside(c(n), frame), `${n} inside the frame`);
  assert.equal(c("p1").y, c("p3").y, "the first three share a row");
  assert.equal(c("p4").y, c("p1").y + c("p1").h + 24, "the fourth starts a second row inside the frame");
  assert.equal(c("p4").x, c("p1").x);
  assert.equal(c("p5").x, c("p2").x);
  assert.ok(c("p5").y + c("p5").h <= frame.y + frame.h, "the frame holds the second row");
  assert.ok(p.cards.get("sk")!.y + G.CARD_H <= c("p1").y, "the Skipper sits above the rows");
  // The wide fleet took the first row alone; the one-project fleet and the two projects share the next.
  assert.ok(frame.w > G.SLOT_W);
  assert.equal(frame.x, 0);
  const small = p.clusters.get("fleet:G")!;
  assert.ok(small.w <= G.SLOT_W, "a fleet with one small project fits a slot");
  assert.equal(small.y, frame.y + frame.h + 48, "it starts the row under the wide fleet");
  assert.deepEqual([small.x, c("solo").x, c("other").x], [0, G.SLOT_W + 48, 2 * (G.SLOT_W + 48)], "three blocks share that row");
  assert.equal(c("solo").y, small.y);
  // The hand offsets of the first test still move only what they name.
  const moved = G.placeGraph(g, { "fleet:F": { dx: 10, dy: 20 }, "project:solo": { dx: -5, dy: 0 } });
  assert.equal(moved.clusters.get("project:p5")!.x, c("p5").x + 10);
  assert.equal(moved.clusters.get("project:p5")!.y, c("p5").y + 20);
  assert.equal(moved.cards.get("sk")!.y, p.cards.get("sk")!.y + 20);
  assert.equal(moved.clusters.get("project:solo")!.x, c("solo").x - 5);
  assert.deepEqual(moved.clusters.get("project:other"), c("other"));
  assert.deepEqual(moved.clusters.get("fleet:G"), small);
});

test("a folded cluster is one card, and its sessions stand in by it", () => {
  const g = graph();
  g.projects[1].folded = true;
  const p = G.placeGraph(g, {});
  const c = p.clusters.get("project:gesher")!;
  assert.equal(c.folded, true);
  assert.deepEqual([c.w, c.h], [G.CARD_W, G.CARD_H]);
  assert.equal(p.cards.has("s9"), false);
  assert.equal(G.standIn(g, p, "s9"), "project:gesher");
  assert.equal(G.standIn(g, p, "s1"), "s1");
  assert.equal(G.navChildren(g, "project:gesher").length, 0, "a folded cluster has nothing under it");
});

test("a hand offset moves a cluster with its contents, and nothing else; a card has no offset", () => {
  const base = G.placeGraph(graph(), {});
  const moved = G.placeGraph(graph(), { "project:skiff": { dx: 100, dy: -30 }, s4: { dx: 7, dy: 7 } });
  for (const id of ["sk-skiff", "s6", "s7", "s8"]) {
    assert.equal(moved.cards.get(id)!.x, base.cards.get(id)!.x + 100, id);
    assert.equal(moved.cards.get(id)!.y, base.cards.get(id)!.y - 30, id);
  }
  assert.equal(moved.clusters.get("project:skiff")!.x, base.clusters.get("project:skiff")!.x + 100);
  assert.deepEqual(moved.cards.get("s4"), base.cards.get("s4"), "a card's offset is ignored: it never leaves its frame");
  for (const id of ["ad", "sk-web", "s1", "s5", "s9", "s11"]) assert.deepEqual(moved.cards.get(id), base.cards.get(id), `${id} stays`);
  // A drag of Admiral moves Admiral; of a Skipper's card or a folded card, its block; a session card reorders instead.
  const g = graph();
  assert.equal(G.moveKey(g, "ad"), "ad");
  assert.equal(G.moveKey(g, "sk-web"), "fleet:Web");
  assert.equal(G.moveKey(g, "fleet:Web"), "fleet:Web");
  assert.equal(G.moveKey(g, "sk-skiff"), "project:skiff");
  assert.equal(G.moveKey(g, "project:checkout"), "project:checkout");
  assert.equal(G.moveKey(g, "s4"), null);
  assert.equal(G.clusterOf(g, "s4"), "project:checkout");
  assert.equal(G.clusterOf(g, "sk-web"), null);
});

test("a card reorders inside its cluster: the slot under it, the others make room, and a drop outside springs back", () => {
  const g = graph();
  const p = G.placeGraph(g, {});
  const c = p.clusters.get("project:checkout")!;
  assert.deepEqual(p.order.get("project:checkout"), ["s4", "s5"]);
  const s4 = p.cards.get("s4")!;
  const s5 = p.cards.get("s5")!;
  const mid = (r: G.Rect) => ({ x: r.x + r.w / 2, y: r.y + r.h / 2 });
  // Two cards sit side by side. The slot nearest the card's middle: its own, then the other's once it is past the midpoint between them.
  assert.equal(s4.y, s5.y);
  assert.equal(G.slotAt(p, "project:checkout", mid(s4)), 0);
  assert.equal(G.slotAt(p, "project:checkout", { x: (mid(s4).x + mid(s5).x) / 2 - 10, y: mid(s4).y + 20 }), 0);
  assert.equal(G.slotAt(p, "project:checkout", { x: (mid(s4).x + mid(s5).x) / 2 + 10, y: mid(s4).y - 20 }), 1);
  // Outside the frame: no slot, so the order stays and the card springs back to its last place.
  assert.equal(G.slotAt(p, "project:checkout", { x: c.x - 1, y: mid(s4).y }), null);
  assert.equal(G.slotAt(p, "project:checkout", { x: mid(s4).x, y: c.y + c.h + 1 }), null);
  // The others make room: s4 to slot 1 puts s5 first.
  assert.deepEqual(G.reorder(["s4", "s5"], "s4", 1), ["s5", "s4"]);
  assert.deepEqual(G.reorder(["a", "b", "c", "d"], "d", 0), ["d", "a", "b", "c"]);
  assert.deepEqual(G.reorder(["a", "b", "c"], "a", 9), ["b", "c", "a"], "past the end: last");
  // The kept order leads; a new session follows in tree order; a gone one is dropped.
  assert.deepEqual(G.orderCards(["s4", "s5", "s6"], ["s5", "gone", "s4"]), ["s5", "s4", "s6"]);
  assert.deepEqual(G.orderCards(["s4", "s5"], undefined), ["s4", "s5"]);
  // The reordered cluster places the cards in the new order, in the same slots.
  g.fleets[0].projects[1].cards = G.reorder(["s4", "s5"], "s4", 1);
  const q = G.placeGraph(g, {});
  assert.deepEqual(q.order.get("project:checkout"), ["s5", "s4"]);
  assert.deepEqual(q.cards.get("s5"), s4);
  assert.deepEqual(q.cards.get("s4"), s5);
  assert.deepEqual(q.clusters.get("project:checkout"), c, "the frame does not change");
});

test("a project in a fleet stays inside the frame: clamped above and left, and the frame grows right and down to hold it", () => {
  const g = graph();
  const base = G.placeGraph(g, {});
  const frame = base.clusters.get("fleet:Web")!;
  const sf = base.clusters.get("project:storefront")!;
  // Left and up past the inside of the frame: the project stops at the frame's padding, under its head; the applied offset says so.
  const up = G.placeGraph(g, { "project:storefront": { dx: -500, dy: -500 } });
  const c = up.clusters.get("project:storefront")!;
  assert.equal(c.x, frame.x + 16);
  assert.equal(c.y, frame.y + 20 + 16);
  assert.deepEqual(up.offs["project:storefront"], { dx: c.x - sf.x, dy: c.y - sf.y });
  assert.deepEqual(up.clusters.get("fleet:Web"), frame, "the frame keeps its size");
  assert.ok(inside(c, up.clusters.get("fleet:Web")!));
  for (const id of ["s1", "s2", "s3"]) assert.ok(inside(up.cards.get(id)!, c), `${id} inside its moved cluster`);
  // Right and down: the frame grows to hold the project, with its padding, and the blocks after it move down with the taller frame.
  const grown = G.placeGraph(g, { "project:checkout": { dx: 300, dy: 200 } });
  const ck = grown.clusters.get("project:checkout")!;
  const f2 = grown.clusters.get("fleet:Web")!;
  assert.deepEqual(grown.offs["project:checkout"], { dx: 300, dy: 200 }, "an offset inside the frame applies as given");
  assert.ok(inside(ck, f2));
  assert.equal(f2.x + f2.w, ck.x + ck.w + 16);
  assert.equal(f2.y + f2.h, ck.y + ck.h + 16);
  assert.ok(f2.w > frame.w && f2.h > frame.h);
  assert.equal(grown.cards.get("sk-web")!.x, base.cards.get("sk-web")!.x, "the fleet Skipper stays put");
  assert.ok(!overlap(f2, grown.clusters.get("project:skiff")!), "the wider frame takes its row alone");
  // The free blocks and Admiral go where they are put, with no clamp.
  const free = G.placeGraph(g, { "fleet:Web": { dx: -300, dy: -100 }, "project:gesher": { dx: 0, dy: -900 }, ad: { dx: 50, dy: -50 } });
  assert.equal(free.clusters.get("fleet:Web")!.x, frame.x - 300);
  assert.equal(free.clusters.get("project:gesher")!.y, base.clusters.get("project:gesher")!.y - 900);
  assert.deepEqual(free.cards.get("ad"), { ...base.cards.get("ad")!, x: base.cards.get("ad")!.x + 50, y: base.cards.get("ad")!.y - 50 });
});

test("Skipper numbers follow the tree: projects in no fleet first, as the Skippers list counts them", () => {
  // Issue #201: one project in no fleet, one fleet, each with a Skipper. Ctrl+Shift+1 must pick the same one in both places.
  const g: G.Graph = { admiral: "ad", fleets: [{ key: "fleet:web", name: "web", skipper: "sk-web", projects: [project("shop", ["s1"], { skipper: "sk-shop" })] }], projects: [project("solo", ["s2"], { skipper: "sk-solo" }), project("other", ["s3"])] };
  assert.deepEqual(G.skipperOrder(g), ["sk-solo", "sk-web", "sk-shop"]);
});

test("the keys move along the tree: parent, children, siblings", () => {
  const g = graph();
  assert.deepEqual(G.navChildren(g, "ad"), ["sk-web", "sk-skiff", "project:gesher", "project:other"]);
  assert.deepEqual(G.navChildren(g, "sk-web"), ["project:storefront", "project:checkout"]);
  assert.deepEqual(G.navChildren(g, "sk-skiff"), ["project:skiff"]);
  assert.deepEqual(G.navChildren(g, "project:storefront"), ["s1", "s2", "s3"]);
  assert.equal(G.navParent(g, "s4"), "project:checkout");
  assert.equal(G.navParent(g, "project:checkout"), "sk-web");
  assert.equal(G.navParent(g, "sk-web"), "ad");
  assert.equal(G.navParent(g, "project:skiff"), "sk-skiff");
  assert.equal(G.navParent(g, "project:gesher"), "ad");
  assert.equal(G.navParent(g, "ad"), null);
  assert.equal(G.navSibling(g, "s1", 1), "s2");
  assert.equal(G.navSibling(g, "s1", -1), "s1", "the first stays at the start");
  assert.equal(G.navSibling(g, "s3", 1), "s3", "the last stays at the end");
  assert.equal(G.navSibling(g, "sk-skiff", 1), "project:gesher");
  assert.equal(G.navSibling(g, "project:storefront", 1), "project:checkout");
  // Up then Down comes back to the first child; every node's parent lists it.
  for (const id of ["s1", "s4", "s6", "s9", "s11", "project:storefront", "project:gesher", "sk-web", "sk-skiff"]) {
    const up = G.navParent(g, id)!;
    assert.ok(G.navChildren(g, up).includes(id), `${up} lists ${id}`);
  }
  // Without Admiral, the top blocks are each other's siblings and have no parent.
  const alone: G.Graph = { ...g, admiral: null };
  assert.equal(G.navFirst(alone), "sk-web");
  assert.equal(G.navParent(alone, "sk-web"), null);
  assert.equal(G.navSibling(alone, "sk-web", 1), "sk-skiff");
  // A fleet with no Skipper is its own node.
  const noSk: G.Graph = { ...g, fleets: [{ ...g.fleets[0], skipper: null }] };
  assert.deepEqual(G.navChildren(noSk, "ad")[0], "fleet:Web");
  assert.equal(G.navParent(noSk, "project:checkout"), "fleet:Web");
});

test("the key map: plain keys on the board, the caret's keys in the search, action keys everywhere", () => {
  const none = { ctrl: false, shift: false, alt: false, meta: false };
  const on = (key: string, action: string | null = null, inSearch = false, text = false) => G.boardCommand(key, action, inSearch, text, none);
  assert.equal(on("ArrowDown"), "child");
  assert.equal(on("ArrowUp"), "parent");
  assert.equal(on("ArrowRight"), "next");
  assert.equal(on("ArrowLeft"), "prev");
  assert.equal(on(" "), "fold");
  assert.equal(on("Enter"), "go");
  assert.equal(on("Home"), "fit");
  assert.equal(on("="), "zoom-in");
  assert.equal(on("-"), "zoom-out");
  assert.equal(on("Escape"), "close");
  assert.equal(on("Escape", null, false, true), "clear-search", "the first Esc clears the search");
  assert.equal(on("Escape", null, true, true), "clear-search");
  // In the search, Left, Right, Space and Home move the caret; Up and Down still move on the board.
  for (const k of ["ArrowLeft", "ArrowRight", " ", "Home", "-"]) assert.equal(on(k, null, true), null, k);
  assert.equal(on("ArrowDown", null, true), "child");
  assert.equal(on("Enter", null, true), "go");
  // The action keys keep their meaning in both places.
  const acts: [string, G.BoardCommand][] = [["palette", "search"], ["mission-control", "close"], ["maximize", "go-max"], ["focus-mode", "running"], ["next-waiting", "next-waiting"], ["ledger", "panel"], ["project-menu", "menu"], ["settings", "settings"], ["skiff-agent", "skipper"], ["session-next", "child"], ["session-prev", "parent"], ["list-project", "prev"], ["list-back", "next"]];
  for (const [a, cmd] of acts) {
    assert.equal(on("x", a), cmd, a);
    assert.equal(on("x", a, true), cmd, `${a} in the search`);
  }
  // A key with a modifier that is no action is not the board's.
  assert.equal(G.boardCommand("ArrowDown", null, false, false, { ...none, ctrl: true }), null);
  assert.equal(on("a"), null);
});

test("the view: fit, zoom at a point, center, the edge tick", () => {
  const box = { x: 100, y: 50, w: 2000, h: 500 };
  const v = G.fitView(box, 1000, 600, 0);
  assert.equal(v.z, 0.5, "the width limits the fit");
  assert.equal(Math.round(v.x + box.x * v.z), 0);
  assert.equal(Math.round(v.y + box.y * v.z), 175, "centered on the short axis");
  assert.equal(G.fitView({ x: 0, y: 0, w: 100, h: 100 }, 1000, 600, 0).z, 1, "never past 100%");
  // Zooming around a point keeps that point still.
  const z = G.zoomView({ x: 10, y: 20, z: 1 }, 2, 300, 200);
  assert.equal(z.z, 2);
  assert.equal(300 - (300 - 10) * 2, z.x);
  assert.equal(G.zoomView({ x: 0, y: 0, z: 1.9 }, 2, 0, 0).z, G.ZOOM_MAX);
  assert.equal(G.zoomView({ x: 0, y: 0, z: 0.25 }, 0.1, 0, 0).z, G.ZOOM_MIN);
  const c = G.centerOn({ x: 0, y: 0, z: 1 }, { x: 100, y: 100, w: 200, h: 100 }, 1000, 600);
  assert.deepEqual(c, { z: 1, x: 300, y: 150 });
  assert.equal(G.inView(c, { x: 100, y: 100, w: 200, h: 100 }, 1000, 600), true);
  assert.equal(G.inView(c, { x: -300, y: 100, w: 200, h: 100 }, 1000, 600), false);
  assert.equal(G.edgeTick(c, { x: 100, y: 100, w: 200, h: 100 }, 1000, 600), null, "a card in view has no tick");
  assert.deepEqual(G.edgeTick(c, { x: -2000, y: 100, w: 200, h: 100 }, 1000, 600), { x: 0, y: 295 }, "a card to the left ticks at the left edge");
  assert.deepEqual(G.edgeTick(c, { x: 100, y: 5000, w: 200, h: 100 }, 1000, 600), { x: 495, y: 590 });
});

test("time in state reads as minutes, hours and days", () => {
  const now = 1_700_000_000_000;
  assert.equal(G.ago(now - 20_000, now), "now");
  assert.equal(G.ago(now - 4 * 60_000, now), "4m");
  assert.equal(G.ago(now - 125 * 60_000, now), "2h");
  assert.equal(G.ago(now - 3 * 1440 * 60_000, now), "3d");
});
