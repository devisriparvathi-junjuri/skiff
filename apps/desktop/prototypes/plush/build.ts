/**
 * Writes plush.html: today's avatar (from the real src/appearance/skipperAvatar.ts)
 * next to three plush finishes, at the sizes the app draws faces, in a dark
 * and a light theme. Run: node --experimental-strip-types build.ts
 */
import { readFileSync, writeFileSync } from "node:fs";
import { admiralAvatar, skipperAvatar } from "../../src/appearance/skipperAvatar.ts";
import { finishFor, plushAdmiral, plushAvatar, plushDefs, type Finish } from "./plushAvatar.ts";

const here = new URL(".", import.meta.url);
// The face CSS as it ships: sizes, blink and turn.
const css = readFileSync(new URL("../../src/styles/skippers.css", here), "utf8");
const faceCss = css.slice(css.indexOf(".skipper-face {"), css.indexOf(".mission-card .skipper-face"));

const SIZES: [number, string][] = [
  [16, "pane header"],
  [18, "Mission control card"],
  [20, "sidebar, project Skipper"],
  [24, "sidebar, Admiral"],
  [26, "Settings › Avatars"],
  [32, "avatar picker"],
  [40, "Skippers view"],
];
// Signature colors of a few themes, as faceColor gives --pc.
const CREW: [string, string][] = [["skiff", "#b69cff"], ["fleet:web", "#f4a261"], ["docs", "#4cc9f0"], ["api", "#ef476f"], ["infra", "#06d6a0"]];

type Col = { key: string; label: string; note: string; finish: (px: number) => Finish | null };
const COLS: Col[] = [
  { key: "now", label: "Today", note: "flat SVG", finish: () => null },
  { key: "felt", label: "A · Felt", note: "gradient, no filter", finish: () => "felt" },
  { key: "fur", label: "B · Fur", note: "fur filter at every size", finish: () => "fur" },
  { key: "adapt", label: "C · Adaptive", note: "felt < 24, light fur to 31, fur ≥ 32", finish: finishFor },
];

const face = (scope: string | null, px: number, f: Finish | null, color = "#b69cff") =>
  `<span class="skipper-face" style="--face:${px};--pc:${color}">${
    f === null ? (scope === null ? admiralAvatar() : skipperAvatar(scope)) : scope === null ? plushAdmiral(f) : plushAvatar(scope, f)
  }</span>`;

const cell = (px: number, col: Col) =>
  `<td><div class="faces">${face(null, px, col.finish(px))}${CREW.map(([n, c]) => face(n, px, col.finish(px), c)).join("")}</div></td>`;

// A sidebar row, as the app draws it, at its real face size.
const sideRows = (col: Col) => `<div class="side">${[
  [null, 24, "Admiral", "reviewing the release notes"],
  ["docs", 20, "docs", "write the plush page"],
  ["api", 20, "api", "waiting for an answer"],
  ["fleet:web", 20, "web", "run the page tests"],
].map(([s, px, name, title]) => `<div class="row">${face(s as string | null, px as number, col.finish(px as number), CREW.find(([n]) => n === s)?.[1])}<span class="n">${name}</span><span class="t">${title}</span></div>`).join("")}</div>`;

const panel = (theme: "dark" | "light") => `<section class="theme ${theme}">
<h2>${theme === "dark" ? "Dark theme" : "Light theme"}</h2>
<table><thead><tr><th>px</th>${COLS.map((c) => `<th>${c.label}<small>${c.note}</small></th>`).join("")}</tr></thead><tbody>
${SIZES.map(([px, where]) => `<tr><th>${px}<small>${where}</small></th>${COLS.map((c) => cell(px, c)).join("")}</tr>`).join("\n")}
<tr class="ctx"><th>rows<small>sidebar, 24 + 20</small></th>${COLS.map((c) => `<td>${sideRows(c)}</td>`).join("")}</tr>
<tr class="hero"><th>96<small>not in the app; for the look</small></th>${COLS.map((c) => `<td><div class="faces">${face("skiff", 96, c.finish(96), "#9b6bff")}${face(null, 96, c.finish(96))}</div></td>`).join("")}</tr>
</tbody></table></section>`;

const html = `<!doctype html><html><head><meta charset="utf-8"><title>Plush avatars</title>
<style>
:root { --u: 1px; --mono: ui-monospace, Menlo, monospace; }
body { margin: 0; font: 13px -apple-system, system-ui, sans-serif; background: #0b0d10; }
.bar { padding: 10px 16px; color: #ccd; font-size: 12px; } .bar label { cursor: pointer; }
body.still .skipper-face * { animation: none !important; }
.theme { padding: 14px 16px 18px; }
.dark { background: #0f1216; color: #d8dde6; --dim: #7d8796; --side: #14181d; --line: #222833; }
.light { background: #f5f6f8; color: #1d2430; --dim: #6b7482; --side: #eceef2; --line: #dde1e7; }
h2 { margin: 0 0 8px; font-size: 14px; }
table { border-collapse: collapse; }
th, td { border-bottom: 1px solid var(--line); padding: 6px 12px; text-align: left; vertical-align: middle; }
th small { display: block; font-weight: 400; color: var(--dim); font-size: 10.5px; }
thead th { font-size: 12.5px; }
tbody th { font: 600 12px var(--mono); white-space: nowrap; }
.faces { display: flex; gap: 8px; align-items: center; }
.side { background: var(--side); width: 250px; border-radius: 6px; padding: 4px 0; }
.row { display: flex; align-items: center; gap: 7px; height: 28px; padding: 0 8px; font-size: 12.5px; white-space: nowrap; overflow: hidden; }
.row .n { font-weight: 600; } .row .t { color: var(--dim); overflow: hidden; text-overflow: ellipsis; }
${faceCss}
</style></head><body>
${plushDefs()}
<div class="bar"><label><input type="checkbox" id="still"> stop the animation</label> · Today = src/appearance/skipperAvatar.ts as on main; A–C = prototypes/plush/plushAvatar.ts</div>
${panel("dark")}
${panel("light")}
<script>if (location.search.includes("still")) { document.body.classList.add("still"); document.getElementById("still").checked = true; }
document.getElementById("still").onchange = (e) => document.body.classList.toggle("still", e.target.checked);</script>
</body></html>`;

writeFileSync(new URL("plush.html", here), html);
console.log("wrote plush.html");
