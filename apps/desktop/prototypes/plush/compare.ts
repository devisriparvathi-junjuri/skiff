/**
 * Writes compare.html: gummy against plush, large. A dark sheet and a light
 * sheet; in each, two vertical strips (GUMMY, PLUSH) with the same three
 * faces: the Admiral and two Skippers of other shapes, colors and eyes.
 * The faces blink and turn as in the app; a checkbox stops them.
 * `?sheet=dark` or `?sheet=light` shows one sheet; `?still` starts stopped.
 * Run: node --experimental-strip-types compare.ts
 */
import { readFileSync, writeFileSync } from "node:fs";
import { plushAdmiral, plushAvatar, plushDefs, type Finish } from "./plushAvatar.ts";

const here = new URL(".", import.meta.url);
const css = readFileSync(new URL("../../src/styles/skippers.css", here), "utf8");
const faceCss = css.slice(css.indexOf(".skipper-face {"), css.indexOf(".mission-card .skipper-face"));

const PX = 180;
// Name → shape and eyes come from the hash: web is a dome with dot eyes, ops a hex with glad eyes.
const CREW: { scope: string | null; label: string; tag: string; color: string }[] = [
  { scope: null, label: "Admiral", tag: "the mascot head", color: "#b69cff" },
  { scope: "web", label: "web", tag: "dome · dot eyes", color: "#9b6bff" },
  { scope: "ops", label: "ops", tag: "hex · glad eyes", color: "#f4a261" },
];

const face = (c: (typeof CREW)[number], f: Finish) =>
  `<figure><span class="skipper-face" style="--face:${PX};--pc:${c.color}">${c.scope === null ? plushAdmiral(f) : plushAvatar(c.scope, f)}</span>`
  + `<figcaption><b>${c.label}</b><small>${c.tag}</small></figcaption></figure>`;

const strip = (f: Finish, title: string, note: string) =>
  `<div class="strip"><h3>${title}<small>${note}</small></h3>${CREW.map((c) => face(c, f)).join("")}</div>`;

const sheet = (theme: "dark" | "light") => `<section class="sheet ${theme}" data-sheet="${theme}">
<h2>${theme === "dark" ? "Dark theme" : "Light theme"}</h2>
<div class="strips">${strip("gummy", "GUMMY", "gloss, lit rim, soft shadow")}${strip("plush", "PLUSH", "long soft fur, two layers")}</div>
</section>`;

const html = `<!doctype html><html><head><meta charset="utf-8"><title>Gummy vs plush</title>
<style>
:root { --u: 1px; }
* { box-sizing: border-box; }
body { margin: 0; font: 13px -apple-system, system-ui, sans-serif; background: #0b0d10; }
.bar { padding: 10px 16px; color: #ccd; font-size: 12px; } .bar label { cursor: pointer; }
body.still .skipper-face * { animation: none !important; }
.sheets { display: flex; flex-wrap: wrap; }
.sheet { padding: 22px 28px 30px; flex: 1 0 auto; }
.dark { background: #0f1216; color: #d8dde6; --dim: #7d8796; --strip: #151a20; --line: #232a34; }
.light { background: #f5f6f8; color: #1d2430; --dim: #6b7482; --strip: #eceef2; --line: #dde1e7; }
h2 { margin: 0 0 16px; font-size: 15px; }
.strips { display: flex; gap: 24px; justify-content: center; }
.strip { width: 280px; background: var(--strip); border: 1px solid var(--line); border-radius: 14px; padding: 18px 0 8px;
  display: flex; flex-direction: column; align-items: center; }
h3 { margin: 0 0 6px; font-size: 13px; letter-spacing: .14em; text-align: center; }
h3 small { display: block; letter-spacing: 0; font-weight: 400; color: var(--dim); font-size: 11.5px; margin-top: 3px; }
figure { margin: 0; padding: 22px 0 14px; display: flex; flex-direction: column; align-items: center; gap: 14px; }
figure + figure { border-top: 1px solid var(--line); width: 100%; }
figcaption { text-align: center; } figcaption b { font-size: 14px; }
figcaption small { display: block; color: var(--dim); font-size: 11.5px; margin-top: 2px; }
.only-dark .light, .only-light .dark { display: none; }
${faceCss}
</style></head><body>
${plushDefs()}
<div class="bar"><label><input type="checkbox" id="still"> stop the animation</label> · prototypes/plush/plushAvatar.ts, finishes "gummy" and "plush" · ${PX} px</div>
<div class="sheets">${sheet("dark")}${sheet("light")}</div>
<script>
const q = new URLSearchParams(location.search), box = document.getElementById("still");
if (q.get("sheet")) document.body.classList.add("only-" + q.get("sheet"));
if (q.has("still")) { document.body.classList.add("still"); box.checked = true; }
box.onchange = () => document.body.classList.toggle("still", box.checked);
</script>
</body></html>`;

writeFileSync(new URL("compare.html", here), html);
console.log("wrote compare.html");
