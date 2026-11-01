/**
 * A Skipper's avatar: a flat body with two eye marks, fixed by its name.
 * `seed` picks another one for Shuffle. Returns an SVG on a 32×32 grid with
 * no size, so the CSS sets it.
 *
 * The body is the element's --pc, the signature color of the Skipper's theme, else --project-default,
 * so a color or theme change repaints it. Black mixed in keeps the white eyes
 * readable on a pale color. The eyes sit on a visor in the Skiff black, the
 * mascot's visor color, which follows the eye style and the face's tilt.
 *
 * The faces live: the eyes blink, and now and then the head turns (the face
 * leans, the visor and the eyes slide aside, the eyes further), in CSS
 * (styles/skippers.css). Each face carries its own rhythm as variables on
 * its root, from the same hash, so no two faces blink in step.
 */
const FILL = "color-mix(in srgb, var(--pc, var(--project-default)) 72%, #000)";
const VISOR = "#192230";

/** Bodies, each with the height of its eye line. A round 3-unit stroke softens every corner. */
const BODIES: [string, number][] = [
  ["M16 5c8.5 0 11 2.5 11 11s-2.5 11-11 11S5 24.5 5 16 7.5 5 16 5z", 15], // squircle
  ["M5 26V16a11 11 0 0 1 22 0v10z", 17], // dome
  ["M5 22c0-8 2.5-14 6-14 2 0 3 2 5 2s3-2 5-2c3.5 0 6 6 6 14 0 3-1.5 5-4.5 5h-13C6.5 27 5 25 5 22z", 18], // bun
  ["M16 5l10 6v10l-10 6-10-6V11z", 16], // hex
  ["M4 16C8 8 24 8 28 16 24 24 8 24 4 16z", 15], // lemon
  ["M6 6h20v9c0 6.5-4.5 10.5-10 12.5C10.5 25.5 6 21.5 6 15z", 13], // shield
  ["M5 7h22l-4.5 19h-13z", 13], // keystone
  ["M9 7l10-2 8 7.5-1.5 10.5-10 4L5 21z", 15], // pebble
];

/** One eye at 0,0, `s` +1 for the right eye and -1 for the left, with the eye's width, height and middle for its visor. */
const EYES: [(s: number) => string, number, number, number][] = [
  [() => `<circle r="2.3"/>`, 4.6, 4.6, 0], // dots
  [() => `<rect x="-1.6" y="-3.2" width="3.2" height="6.4" rx="1.6"/>`, 3.2, 6.4, 0], // tall
  [(s) => `<rect x="-1.6" y="-3.2" width="3.2" height="6.4" rx="1.6" transform="rotate(${-22 * s})"/>`, 5.4, 7.1, 0], // set
  [(s) => `<rect x="-1.6" y="-3.2" width="3.2" height="6.4" rx="1.6" transform="rotate(${22 * s})"/>`, 5.4, 7.1, 0], // unsure
  [() => `<rect x="-3" y="-1.3" width="6" height="2.6" rx="1.3"/>`, 6, 2.6, 0], // calm
  [() => `<path d="M-2.8 1.2q2.8-4 5.6 0" fill="none" stroke="#fff" stroke-width="2.4" stroke-linecap="round"/>`, 8, 4.4, 0.2], // glad
];

/** The visor behind two eyes `gap` from `x`, on the line `y`: 1.6 around the eyes, round at the ends. */
function visor(x: number, y: number, gap: number, [, w, ht, mid]: (typeof EYES)[number]): string {
  const vw = Math.min(2 * gap + w + 3.2, 21), vh = ht + 3.2, r = Math.min(vh / 2, 3.5);
  return `<rect x="${x - vw / 2}" y="${y + mid - vh / 2}" width="${vw}" height="${vh}" rx="${r}" fill="${VISOR}"/>`;
}

/** FNV-1a: the same name gives the same avatar on every machine. */
function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 0x01000193);
  return h >>> 0;
}

/**
 * The face's rhythm: a blink every 4 to 7 s, a double blink for one face in
 * five, and a head turn every 10 to 18 s. The turns go one way, the other
 * way, then up, so a cycle holds three; `--dir` picks the first side. Each
 * at its own phase.
 */
function rhythm(h: number): string {
  const blink = 4 + ((h >>> 16) % 31) / 10;
  const turn = 3 * (10 + ((h >>> 21) % 9));
  // The delays count from the page clock, not from when this face was drawn: a
  // row drawn again (the Admiral's, on every state change) picks up its
  // rhythm where it was, instead of starting over and never reaching a turn.
  const now = performance.now() / 1000;
  const phase = (period: number, offset: number) => (-((now + offset) % period)).toFixed(2);
  return `--blink:${blink}s;--blink-delay:${phase(blink, ((h >>> 4) % 70) / 10)}s;--blink-anim:${(h >>> 25) % 5 ? "sk-blink" : "sk-blink2"};`
    + `--face-turn:${turn}s;--face-turn-delay:${phase(turn, (h >>> 9) % turn)}s;--dir:${(h >>> 29) & 1 ? 1 : -1}`;
}

export function skipperAvatar(name: string, seed = 0): string {
  const h = hash(seed ? `${name}#${seed}` : name);
  const [d, y] = BODIES[h % 8];
  const eye = EYES[(h >>> 3) % 6];
  const [draw] = eye;
  const tilt = (((h >>> 6) % 9) - 4) * 2;
  const gap = 4.5 + ((h >>> 10) % 3);
  const look = ((h >>> 12) % 3) - 1;
  const at = (s: number) => `<g transform="translate(${16 + look + s * gap} ${y})">${draw(s)}</g>`;
  return `<svg viewBox="0 0 32 32" aria-hidden="true" style="${rhythm(h)}"><g class="sk-turn"><g transform="rotate(${tilt} 16 16)"><path d="${d}" style="fill:${FILL};stroke:${FILL}" stroke-width="3" stroke-linejoin="round"/><g class="sk-front">${visor(16 + look, y, gap, eye)}<g class="sk-look"><g class="sk-eyes" fill="#fff">${at(-1)}${at(1)}</g></g></g></g></g></svg>`;
}

/**
 * The Admiral: the Skiff mascot's head, filling the grid as the crew's bodies
 * do. A wide rounded-square white head; the Skiff-black visor across most of
 * its front, with two tall white eyes set wide, which blink and turn as the
 * crew's do; a sailor cap sitting low and wide: a white crown and a band in
 * the --pc color with a gold button, straight on the head, as on the mascot. A thin Skiff-black
 * edge keeps the white on a light theme.
 */
const EDGE = `stroke="${VISOR}" stroke-width="1.6" stroke-linejoin="round"`;
export const admiralAvatar = () => `<svg viewBox="0 0 32 32" aria-hidden="true" style="${rhythm(hash("Admiral"))}"><g class="sk-turn">`
  + `<rect x="2.4" y="10.6" width="27.2" height="19.4" rx="8" fill="#fff" ${EDGE}/>`
  // The cap's crown and band stay back in a turn; the front, on one centerline,
  // moves as one: the button, the visor and the eyes. No brim, as on the mascot.
  + `<g class="sk-cap"><path d="M4.6 9.4C5.4 4.6 10 2 16 2s10.6 2.6 11.4 7.4z" fill="#fff" ${EDGE}/>`
  + `<rect x="4.2" y="8" width="23.6" height="4.4" rx="1.8" style="fill:${FILL}" ${EDGE}/></g>`
  + `<g class="sk-front"><circle cx="16" cy="8.2" r="1.9" fill="#f4c430" ${EDGE}/>`
  + `<rect x="5.6" y="15.2" width="20.8" height="11.6" rx="4.8" fill="${VISOR}"/>`
  + `<g class="sk-look"><g class="sk-eyes" fill="#fff"><rect x="9.2" y="17.6" width="3.4" height="6.6" rx="1.7"/><rect x="19.4" y="17.6" width="3.4" height="6.6" rx="1.7"/></g></g></g></g></svg>`;
