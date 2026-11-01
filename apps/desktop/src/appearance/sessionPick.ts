/**
 * The selection in the skiffd activity list, and the words of its Sink action.
 * No DOM here: the page tests run it with the real catalog.
 */
import { t, tn } from "../i18n";

/** The selected session ids, the start of a range, and the current row. */
export interface Pick { sel: Set<string>; anchor: string | null; now: string | null; }
export const emptyPick = (): Pick => ({ sel: new Set(), anchor: null, now: null });

/** The ids from `from` to `to` in list order, both included. An unknown `from` starts at `to`. */
function span(order: string[], from: string | null, to: string): string[] {
  const b = order.indexOf(to);
  if (b < 0) return [];
  const a = from === null || order.indexOf(from) < 0 ? b : order.indexOf(from);
  return order.slice(Math.min(a, b), Math.max(a, b) + 1);
}

/**
 * A click, or Space on a row. Plain: that row alone. `toggle` (Ctrl, ⌘ on macOS): add or drop
 * that row. `range` (Shift): the rows from the anchor to that row; with `toggle` too, add them.
 */
export function pickRow(p: Pick, order: string[], id: string, mod: { toggle?: boolean; range?: boolean } = {}) {
  if (!order.includes(id)) return;
  if (mod.range) {
    const rows = span(order, p.anchor, id);
    if (!mod.toggle) p.sel = new Set(rows);
    else for (const r of rows) p.sel.add(r);
    if (p.anchor === null || !order.includes(p.anchor)) p.anchor = id;
  } else {
    if (mod.toggle) { if (!p.sel.delete(id)) p.sel.add(id); }
    else p.sel = new Set([id]);
    p.anchor = id;
  }
  p.now = id;
}

/** An arrow, Home or End to `id`. Plain: that row alone. `range`: from the anchor. `keep`: only the current row moves. */
export function moveTo(p: Pick, order: string[], id: string, mod: { range?: boolean; keep?: boolean } = {}) {
  if (!order.includes(id)) return;
  if (mod.keep) p.now = id;
  else pickRow(p, order, id, { range: mod.range });
}

/** Ctrl+A (⌘A on macOS): every row. */
export function pickAll(p: Pick, order: string[]) {
  p.sel = new Set(order);
  if (p.now === null || !order.includes(p.now)) p.now = order[0] ?? null;
  if (p.anchor === null || !order.includes(p.anchor)) p.anchor = p.now;
}

/** After a refresh: ended sessions leave the selection. */
export function keepLive(p: Pick, order: string[]) {
  const live = new Set(order);
  for (const id of p.sel) if (!live.has(id)) p.sel.delete(id);
  if (p.anchor !== null && !live.has(p.anchor)) p.anchor = null;
  if (p.now !== null && !live.has(p.now)) p.now = null;
}

/** The selected ids in list order. */
export const picked = (p: Pick, order: string[]) => order.filter((id) => p.sel.has(id));

/** The Sink button. It opens a dialog, so it ends in "…". */
export const sinkLabel = (n: number) => t(n > 1 ? "sink.selected" : "sink.one");

/** The status line that reads the selection out. */
export const pickedText = (n: number) => (n ? t("sink.picked", { n }) : "");

/** More names than this and the dialog gives the count only. */
export const NAMES_SHOWN = 5;

/** The Abandon Ship dialog: the sessions that end, by name, or their count when there are many. */
export function abandonShip(names: string[]): { title: string; body: string; action: string } {
  const n = names.length;
  const title = t("sink.title");
  if (n === 1) return { title, body: t("sink.bodyOne", { name: names[0] }), action: t("sink.actionOne") };
  const list = n <= NAMES_SHOWN ? t("sink.listNames", { n, names: names.join("\n") }) : t("sink.listCount", { n });
  return { title, body: `${list}\n${t("sink.lost")}`, action: tn("sink.action", n) };
}
