/**
 * Exploration only (branch explore/plush-avatars). A copy of
 * src/appearance/skipperAvatar.ts with one change: a `finish`.
 *
 * - "flat": what ships today.
 * - "felt": the same face, with a shading layer over the body (a light top
 *   left, a dark rim). No filter. Same cost as today.
 * - "fur": felt plus an SVG fur filter on the body: feTurbulence noise
 *   pushes the body's edge about (feDisplacementMap), and a finer noise adds a
 *   grain inside. The visor and the eyes stay crisp, out of the filter.
 * - "fur-sm": a lighter fur (a smaller, coarser push, no grain) for 24 to 31 px.
 *
 * - "gummy": a candy finish for large faces: a deeper shade, a lit inner rim
 *   at the bottom as if light passed through, a broad specular highlight
 *   (feSpecularLighting on the blurred shape) and a soft drop shadow.
 * - "plush": a softer, longer fur for large faces: two fur layers, a long
 *   blurred halo under a shorter core, low-contrast grain, a rounder shade.
 *
 * `finishFor(px)` is the adaptive pick: felt under 24 px, fur-sm to 31, fur
 * from 32.
 *
 * The shading and the filters live once per document in `plushDefs()`, so a
 * face adds no ids of its own. The shading is white and black at an opacity,
 * over the body's own --pc fill, so one gradient serves every color.
 */
export type Finish = "flat" | "felt" | "fur" | "fur-sm" | "gummy" | "plush";

export const finishFor = (px: number): Finish => (px < 24 ? "felt" : px < 32 ? "fur-sm" : "fur");

const FILL = "color-mix(in srgb, var(--pc, var(--project-default)) 72%, #000)";
const VISOR = "#192230";

const BODIES: [string, number][] = [
  ["M16 5c8.5 0 11 2.5 11 11s-2.5 11-11 11S5 24.5 5 16 7.5 5 16 5z", 15],
  ["M5 26V16a11 11 0 0 1 22 0v10z", 17],
  ["M5 22c0-8 2.5-14 6-14 2 0 3 2 5 2s3-2 5-2c3.5 0 6 6 6 14 0 3-1.5 5-4.5 5h-13C6.5 27 5 25 5 22z", 18],
  ["M16 5l10 6v10l-10 6-10-6V11z", 16],
  ["M4 16C8 8 24 8 28 16 24 24 8 24 4 16z", 15],
  ["M6 6h20v9c0 6.5-4.5 10.5-10 12.5C10.5 25.5 6 21.5 6 15z", 13],
  ["M5 7h22l-4.5 19h-13z", 13],
  ["M9 7l10-2 8 7.5-1.5 10.5-10 4L5 21z", 15],
];

const EYES: [(s: number) => string, number, number, number][] = [
  [() => `<circle r="2.3"/>`, 4.6, 4.6, 0],
  [() => `<rect x="-1.6" y="-3.2" width="3.2" height="6.4" rx="1.6"/>`, 3.2, 6.4, 0],
  [(s) => `<rect x="-1.6" y="-3.2" width="3.2" height="6.4" rx="1.6" transform="rotate(${-22 * s})"/>`, 5.4, 7.1, 0],
  [(s) => `<rect x="-1.6" y="-3.2" width="3.2" height="6.4" rx="1.6" transform="rotate(${22 * s})"/>`, 5.4, 7.1, 0],
  [() => `<rect x="-3" y="-1.3" width="6" height="2.6" rx="1.3"/>`, 6, 2.6, 0],
  [() => `<path d="M-2.8 1.2q2.8-4 5.6 0" fill="none" stroke="#fff" stroke-width="2.4" stroke-linecap="round"/>`, 8, 4.4, 0.2],
];

function visor(x: number, y: number, gap: number, [, w, ht, mid]: (typeof EYES)[number], finish: Finish): string {
  const vw = Math.min(2 * gap + w + 3.2, 21), vh = ht + 3.2, r = Math.min(vh / 2, 3.5);
  const at = `x="${x - vw / 2}" y="${y + mid - vh / 2}" width="${vw}" height="${vh}" rx="${r}"`;
  // Plush: the visor gets a soft top light too, so it reads as a part of the toy, not a sticker.
  return `<rect ${at} fill="${VISOR}"/>` + (finish === "flat" ? "" : `<rect ${at} fill="url(#pl-visor)"/>`);
}

function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 0x01000193);
  return h >>> 0;
}

function rhythm(h: number): string {
  const blink = 4 + ((h >>> 16) % 31) / 10;
  const turn = 3 * (10 + ((h >>> 21) % 9));
  const now = performance.now() / 1000;
  const phase = (period: number, offset: number) => (-((now + offset) % period)).toFixed(2);
  return `--blink:${blink}s;--blink-delay:${phase(blink, ((h >>> 4) % 70) / 10)}s;--blink-anim:${(h >>> 25) % 5 ? "sk-blink" : "sk-blink2"};`
    + `--face-turn:${turn}s;--face-turn-delay:${phase(turn, (h >>> 9) % turn)}s;--dir:${(h >>> 29) & 1 ? 1 : -1}`;
}

/** The body: flat as today, or with the shading layer, in the fur filter for the fur finishes. */
function body(shape: string, finish: Finish): string {
  const flat = `<path d="${shape}" style="fill:${FILL};stroke:${FILL}" stroke-width="3" stroke-linejoin="round"/>`;
  if (finish === "flat") return flat;
  // Gummy and plush: an opaque grey shade blended hard-light (50 % grey changes
  // nothing), so the fill and the 3-unit stroke, which overlap inside the edge,
  // paint once; a see-through shade doubled there into a band.
  const g = finish === "gummy" ? "pl-shade-g" : finish === "plush" ? "pl-shade-p" : "pl-shade";
  const blend = finish === "gummy" || finish === "plush" ? ` style="mix-blend-mode:hard-light"` : "";
  const shade = `<path d="${shape}" fill="url(#${g})" stroke="url(#${g})" stroke-width="3" stroke-linejoin="round"${blend}/>`;
  return finish === "felt" ? flat + shade : `<g filter="url(#pl-${finish})">${flat}${shade}</g>`;
}

export function plushAvatar(name: string, finish: Finish, seed = 0): string {
  const h = hash(seed ? `${name}#${seed}` : name);
  const [d, y] = BODIES[h % 8];
  const eye = EYES[(h >>> 3) % 6];
  const [draw] = eye;
  const tilt = (((h >>> 6) % 9) - 4) * 2;
  const gap = 4.5 + ((h >>> 10) % 3);
  const look = ((h >>> 12) % 3) - 1;
  const at = (s: number) => `<g transform="translate(${16 + look + s * gap} ${y})">${draw(s)}</g>`;
  return `<svg viewBox="0 0 32 32" aria-hidden="true" overflow="visible" style="${rhythm(h)}"><g class="sk-turn"><g transform="rotate(${tilt} 16 16)">${body(d, finish)}<g class="sk-front">${visor(16 + look, y, gap, eye, finish)}<g class="sk-look"><g class="sk-eyes" fill="#fff">${at(-1)}${at(1)}</g></g></g></g></g></svg>`;
}

/** The Admiral, with the same finish on the white head and the crown. */
const EDGE = `stroke="${VISOR}" stroke-width="1.6" stroke-linejoin="round"`;
export function plushAdmiral(finish: Finish): string {
  const fur = finish !== "flat" && finish !== "felt";
  // The ink edge stays, inside the filter, so it frays with the fur: without it a
  // white head melts into a light theme at 24 px.
  // At the large plush finish the ink frays into dirt; there the shade's grey rim holds the white.
  const edge = finish === "plush" ? "" : EDGE;
  const shade = (el: string) => (finish === "flat" ? "" : el.replace(/fill="#fff"/, `fill="url(#pl-shade-w)"`).replace(edge, ""));
  const wrap = (s: string) => (fur ? `<g filter="url(#pl-${finish})">${s}</g>` : s);
  const head = `<rect x="2.4" y="10.6" width="27.2" height="19.4" rx="8" fill="#fff" ${edge}/>`;
  const crown = `<path d="M4.6 9.4C5.4 4.6 10 2 16 2s10.6 2.6 11.4 7.4z" fill="#fff" ${edge}/>`;
  return `<svg viewBox="0 0 32 32" aria-hidden="true" overflow="visible" style="${rhythm(hash("Admiral"))}"><g class="sk-turn">`
    + wrap(head + shade(head))
    + `<g class="sk-cap">${wrap(crown + shade(crown))}`
    + `<rect x="4.2" y="8" width="23.6" height="4.4" rx="1.8" style="fill:${FILL}" ${EDGE}/></g>`
    + `<g class="sk-front"><circle cx="16" cy="8.2" r="1.9" fill="#f4c430" ${EDGE}/>`
    + `<rect x="5.6" y="15.2" width="20.8" height="11.6" rx="4.8" fill="${VISOR}"/>`
    + (finish === "flat" ? "" : `<rect x="5.6" y="15.2" width="20.8" height="11.6" rx="4.8" fill="url(#pl-visor)"/>`)
    + `<g class="sk-look"><g class="sk-eyes" fill="#fff"><rect x="9.2" y="17.6" width="3.4" height="6.6" rx="1.7"/><rect x="19.4" y="17.6" width="3.4" height="6.6" rx="1.7"/></g></g></g></g></svg>`;
}

/**
 * Once per document. The filters work in the face's own 32-unit grid
 * (userSpaceOnUse), so the fur is the same fraction of the face at every
 * size; that is why the small sizes need their own pick (finishFor).
 */
export const plushDefs = () => `<svg width="0" height="0" style="position:absolute" aria-hidden="true"><defs>
<radialGradient id="pl-shade" cx=".36" cy=".3" r=".78">
  <stop offset="0" stop-color="#fff" stop-opacity=".42"/><stop offset=".42" stop-color="#fff" stop-opacity="0"/>
  <stop offset=".72" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".45"/>
</radialGradient>
<radialGradient id="pl-shade-w" cx=".36" cy=".28" r=".8">
  <stop offset="0" stop-color="#fff"/><stop offset=".55" stop-color="#eef0f4"/><stop offset="1" stop-color="#aeb6c4"/>
</radialGradient>
<linearGradient id="pl-visor" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="#fff" stop-opacity=".16"/><stop offset=".5" stop-color="#fff" stop-opacity="0"/>
</linearGradient>
<filter id="pl-fur" x="-20%" y="-20%" width="140%" height="140%" color-interpolation-filters="sRGB">
  <feTurbulence type="fractalNoise" baseFrequency="1.5" numOctaves="2" seed="7" result="noise"/>
  <feDisplacementMap in="SourceGraphic" in2="noise" scale="2.6" xChannelSelector="R" yChannelSelector="G" result="fuzz"/>
  <feTurbulence type="fractalNoise" baseFrequency="3.2 1.6" numOctaves="1" seed="3" result="grain"/>
  <feColorMatrix in="grain" type="saturate" values="0" result="g"/>
  <feBlend in="fuzz" in2="g" mode="soft-light" result="furred"/>
  <feComposite in="furred" in2="fuzz" operator="in"/>
</filter>
<radialGradient id="pl-shade-g" gradientUnits="userSpaceOnUse" cx="12" cy="10" r="19">
  <stop offset="0" stop-color="#cfcfcf"/><stop offset=".35" stop-color="#808080"/>
  <stop offset=".7" stop-color="#6e6e6e"/><stop offset="1" stop-color="#2a2a2a"/>
</radialGradient>
<radialGradient id="pl-shade-p" gradientUnits="userSpaceOnUse" cx="13.5" cy="11.5" r="17.5">
  <stop offset="0" stop-color="#b8b8b8"/><stop offset=".45" stop-color="#808080"/>
  <stop offset=".75" stop-color="#6c6c6c"/><stop offset="1" stop-color="#3a3a3a"/>
</radialGradient>
<filter id="pl-gummy" x="-25%" y="-25%" width="150%" height="160%" color-interpolation-filters="sRGB">
  <!-- the lit inner rim at the bottom: the shape's edge, blurred, pushed up, softened, kept inside -->
  <feGaussianBlur in="SourceAlpha" stdDeviation="1.6" result="soft"/>
  <feOffset in="soft" dy="-1.8" result="up"/>
  <feComposite in="SourceAlpha" in2="up" operator="out" result="rim0"/>
  <feGaussianBlur in="rim0" stdDeviation=".7" result="rim1"/>
  <feComposite in="rim1" in2="SourceAlpha" operator="in" result="rim"/>
  <feFlood flood-color="#fff" flood-opacity=".38"/>
  <feComposite in2="rim" operator="in" result="glow"/>
  <!-- the inner dark at the top edge, for depth -->
  <feOffset in="soft" dy="1.4" result="down"/>
  <feComposite in="SourceAlpha" in2="down" operator="out" result="top0"/>
  <feGaussianBlur in="top0" stdDeviation=".7" result="top1"/>
  <feComposite in="top1" in2="SourceAlpha" operator="in" result="toprim"/>
  <feFlood flood-color="#000" flood-opacity=".3"/>
  <feComposite in2="toprim" operator="in" result="dark"/>
  <!-- the gloss: a light on the blurred shape as a bump map; a wide blur and a
       low surface keep the 8-bit slopes from banding into rings -->
  <feGaussianBlur in="SourceAlpha" stdDeviation="3.2" result="bump"/>
  <feSpecularLighting in="bump" surfaceScale="2.4" specularConstant=".85" specularExponent="14" lighting-color="#fff" result="spec">
    <fePointLight x="10" y="4" z="22"/>
  </feSpecularLighting>
  <feGaussianBlur in="spec" stdDeviation=".5" result="spec2"/>
  <feComposite in="spec2" in2="SourceAlpha" operator="in" result="gloss"/>
  <feMerge result="body"><feMergeNode in="SourceGraphic"/><feMergeNode in="dark"/><feMergeNode in="glow"/><feMergeNode in="gloss"/></feMerge>
  <!-- a soft shadow under it -->
  <feGaussianBlur in="SourceAlpha" stdDeviation="1.4" result="sb"/>
  <feOffset in="sb" dy="1.8" result="so"/>
  <feFlood flood-color="#000" flood-opacity=".32"/>
  <feComposite in2="so" operator="in" result="shadow"/>
  <feMerge><feMergeNode in="shadow"/><feMergeNode in="body"/></feMerge>
</filter>
<filter id="pl-plush" x="-30%" y="-30%" width="160%" height="160%" color-interpolation-filters="sRGB">
  <!-- tufts: a lower frequency with four octaves, so clumps with fine ends, not scratches -->
  <feTurbulence type="fractalNoise" baseFrequency="1.15" numOctaves="4" seed="11" result="noise"/>
  <!-- the outer fluff: the widest push, well blurred, faint: the soft halo of the reference -->
  <feDisplacementMap in="SourceGraphic" in2="noise" scale="6" xChannelSelector="R" yChannelSelector="G" result="far"/>
  <feGaussianBlur in="far" stdDeviation=".8" result="far2"/>
  <feComponentTransfer in="far2" result="fluff1"><feFuncA type="linear" slope=".55"/></feComponentTransfer>
  <!-- the long fur: a wide push, lightly blurred -->
  <feDisplacementMap in="SourceGraphic" in2="noise" scale="4.2" xChannelSelector="G" yChannelSelector="R" result="long"/>
  <feGaussianBlur in="long" stdDeviation=".35" result="fluff2"/>
  <!-- the core: a short push, crisp -->
  <feDisplacementMap in="SourceGraphic" in2="noise" scale="2" xChannelSelector="R" yChannelSelector="G" result="core"/>
  <feGaussianBlur in="core" stdDeviation=".15" result="core2"/>
  <feMerge result="fluff"><feMergeNode in="fluff1"/><feMergeNode in="fluff2"/><feMergeNode in="core2"/></feMerge>
  <!-- the grain: larger and blurred, low in contrast: a nap, not sandpaper -->
  <feTurbulence type="fractalNoise" baseFrequency="1.3 .9" numOctaves="2" seed="5" result="grain"/>
  <feColorMatrix in="grain" type="saturate" values="0" result="gg"/>
  <feGaussianBlur in="gg" stdDeviation=".2" result="gb"/>
  <feComponentTransfer in="gb" result="g"><feFuncR type="linear" slope=".55" intercept=".22"/><feFuncG type="linear" slope=".55" intercept=".22"/><feFuncB type="linear" slope=".55" intercept=".22"/></feComponentTransfer>
  <feBlend in="fluff" in2="g" mode="soft-light" result="furred"/>
  <feComposite in="furred" in2="fluff" operator="in" result="toy"/>
  <!-- a soft contact shadow, so a white toy holds on a light theme -->
  <feGaussianBlur in="SourceAlpha" stdDeviation="1.8" result="sb"/>
  <feOffset in="sb" dy="1.8" result="so"/>
  <feFlood flood-color="#000" flood-opacity=".22"/>
  <feComposite in2="so" operator="in" result="shadow"/>
  <feMerge><feMergeNode in="shadow"/><feMergeNode in="toy"/></feMerge>
</filter>
<filter id="pl-fur-sm" x="-15%" y="-15%" width="130%" height="130%" color-interpolation-filters="sRGB">
  <feTurbulence type="fractalNoise" baseFrequency="1.1" numOctaves="1" seed="7" result="noise"/>
  <feDisplacementMap in="SourceGraphic" in2="noise" scale="1.5" xChannelSelector="R" yChannelSelector="G"/>
</filter>
</defs></svg>`;
