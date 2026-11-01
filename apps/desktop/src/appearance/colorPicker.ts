import { rgb } from "./colors";
import { h } from "../ui/dom";
import { actionDialog } from "../ui/dialogParts";
import { t } from "../i18n";

/** A color picker drawn in the page: the webview's own color input does not open on Linux. */

type Hsv = [number, number, number];

function toHsv(hex: string): Hsv {
  const [r, g, b] = rgb(hex).map((v) => v / 255);
  const max = Math.max(r, g, b);
  const d = max - Math.min(r, g, b);
  let h = 0;
  if (d) h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [(h * 60 + 360) % 360, max ? d / max : 0, max];
}

function toHex([h, s, v]: Hsv): string {
  const f = (n: number) => {
    const k = (n + h / 60) % 6;
    return Math.round((v - v * s * Math.max(0, Math.min(k, 4 - k, 1))) * 255)
      .toString(16)
      .padStart(2, "0");
  };
  return `#${f(5)}${f(3)}${f(1)}`;
}

/** Drag on `el` and report x and y as 0..1. */
function drag(el: HTMLElement, on: (x: number, y: number) => void) {
  const at = (e: PointerEvent) => {
    const r = el.getBoundingClientRect();
    on(Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)), Math.min(1, Math.max(0, (e.clientY - r.top) / r.height)));
  };
  el.addEventListener("pointerdown", (e) => {
    el.setPointerCapture(e.pointerId);
    at(e);
    const move = (m: PointerEvent) => at(m);
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", () => el.removeEventListener("pointermove", move), { once: true });
  });
}

export function pickColor(o: { title: string; start: string; action: string; submit: (hex: string) => Promise<void>; onClose?: () => void }): void {
  let hsv = toHsv(/^#[0-9a-f]{6}$/i.test(o.start) ? o.start : "#7ee0cb");

  const area = h("div", "cp-area");
  const areaDot = h("div", "cp-dot");
  area.append(areaDot);
  const hue = h("div", "cp-hue");
  const hueDot = h("div", "cp-bar");
  hue.append(hueDot);
  const readout = h("div", "cp-readout");
  const chip = h("span", "cp-chip");
  const code = h("input", "cp-code");
  code.spellcheck = false;
  code.maxLength = 7;
  code.setAttribute("aria-label", t("color.hex"));
  readout.append(chip, code);
  const paint = (skipCode = false) => {
    const hex = toHex(hsv);
    area.style.background = `linear-gradient(to top, #000, transparent), linear-gradient(to right, #fff, transparent), ${toHex([hsv[0], 1, 1])}`;
    areaDot.style.left = `${hsv[1] * 100}%`;
    areaDot.style.top = `${(1 - hsv[2]) * 100}%`;
    hueDot.style.left = `${(hsv[0] / 360) * 100}%`;
    chip.style.background = hex;
    if (!skipCode) code.value = hex;
  };
  drag(area, (x, y) => ((hsv = [hsv[0], x, 1 - y]), paint()));
  drag(hue, (x) => ((hsv = [x * 359.99, hsv[1], hsv[2]]), paint()));
  code.addEventListener("input", () => {
    const v = "#" + code.value.replace(/[^0-9a-f]/gi, "").slice(0, 6);
    if (/^#[0-9a-f]{6}$/i.test(v)) {
      hsv = toHsv(v);
      paint(true);
    }
  });

  paint();
  actionDialog({
    title: o.title,
    content: [area, hue, readout],
    action: o.action,
    tone: "go",
    onClose: o.onClose,
    submit: () => o.submit(toHex(hsv)),
  });
}
