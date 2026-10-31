import { invoke } from "@tauri-apps/api/core";
import { h } from "../ui/dom";
import { confirmAction } from "../ui/confirm";
import { size } from "./skiffFolderDialog";

/** A count of files and their bytes. */
interface Line {
  files: number;
  bytes: number;
}

/** `disk_usage`, as the backend counts the Skiff agent's folder. */
export interface DiskUsage {
  /** The folder with `~` for home. */
  shown: string;
  /** The images, pinned ones included. */
  cache: Line;
  /** The pinned images: no flush removes them. */
  pinned: Line;
  /** Null: the ledger was refused. */
  results: Line | null;
  skippers: { name: string; folder: string; line: Line }[];
  closed: Line;
  items: number;
  plans: number;
  logs: Line;
  other: Line;
  total: Line;
  unreadable: number;
  ledger_error: string | null;
  daemon: boolean;
  /** What Flush done work removes. The flush sends it back. */
  removable: string[];
  /** Auto flush's age in days. 0: off. */
  cache_days: number;
}

/** The page's state. It survives every redraw of Settings. */
export interface DiskState {
  usage: DiskUsage | null;
  counting: boolean;
  /** "Removed N files, X." after a flush. */
  removed: string | null;
  error: string | null;
}

export const diskState = (): DiskState => ({ usage: null, counting: false, removed: null, error: null });

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** Counts the folder. The cells show "…" until the answer comes. */
export async function countDisk(s: DiskState, redraw: () => void) {
  s.counting = true;
  redraw();
  try {
    s.usage = await invoke<DiskUsage>("disk_usage");
    s.error = null;
  } catch (e) {
    s.error = String(e);
  } finally {
    s.counting = false;
    redraw();
  }
}

async function flush(s: DiskState, redraw: () => void, run: () => Promise<{ files: number; bytes: number }>) {
  try {
    const out = await run();
    s.removed = `Removed ${plural(out.files, "file")}, ${size(out.bytes)}.`;
    s.error = null;
  } catch (e) {
    s.error = String(e);
  }
  await countDisk(s, redraw);
}

/** Flush done work…: the counts first, then the flush of what the dialog showed. */
async function flushDone(s: DiskState, u: DiskUsage, redraw: () => void) {
  const lines: string[] = [];
  if (u.closed.files || u.plans)
    lines.push(`Remove ${plural(u.closed.files, "result file")} (${size(u.closed.bytes)}) of ${plural(u.items, "closed work item")}, and ${plural(u.plans, "plan folder")}.`);
  const loose = { files: u.cache.files - u.pinned.files, bytes: u.cache.bytes - u.pinned.bytes };
  if (loose.files) lines.push(`Remove ${plural(loose.files, "image")} (${size(loose.bytes)}). Pinned images stay.`);
  lines.push("Open and working work stays. The ledger, SKIPPER.md and AGENTS.md stay.");
  const ok = await confirmAction({ title: "Flush done work?", body: lines.join("\n"), action: "Flush" });
  if (ok) await flush(s, redraw, () => invoke("flush_done", { expected: u.removable }));
}

/**
 * Settings › Disk use: what the Skiff agent's folder holds, Flush images,
 * Flush done work… and the auto flush switch. `setDays` saves
 * `[skiff_agent] cache_days`.
 */
export function diskUseSection(s: DiskState, redraw: () => void, setDays: (days: number) => void): HTMLElement[] {
  const u = s.usage;
  const head = h("div", "set-head");
  const title = h("div", "set-h", "Disk use");
  title.id = "set-h-disk";
  head.append(title, h("div", "set-sub mono", u?.shown ?? ""));

  const table = h("div", "set-disk");
  table.setAttribute("role", "table");
  table.setAttribute("aria-labelledby", title.id);
  const cols = h("div", "set-row set-cols");
  cols.setAttribute("role", "row");
  cols.append(h("span", "", "TYPE"), h("span", "c-num", "FILES"), h("span", "c-num", "SIZE"), h("span", "", "FOLDER"), h("span"));
  table.append(cols);

  const counted = (l: Line | null | undefined, known = true) =>
    s.counting || !u ? ["…", "…"] : !known || !l ? ["—", "—"] : [String(l.files), size(l.bytes)];
  const row = (name: string, l: Line | null | undefined, opts: { sub?: boolean; total?: boolean; folder?: string; plain?: boolean; act?: HTMLElement; known?: boolean } = {}) => {
    const r = h("div", "set-row set-agent on" + (opts.sub ? " sub" : "") + (opts.total ? " total" : ""));
    r.setAttribute("role", "row");
    const [files, bytes] = counted(l, opts.known ?? true);
    r.append(
      h("span", "c-name", name),
      h("span", "c-num", files),
      h("span", "c-num", bytes),
      h("span", "c-where" + (opts.plain ? "" : " mono"), opts.folder ?? ""),
      opts.act ?? h("span"),
    );
    return r;
  };
  const button = (label: string, off: boolean, click: () => void) => {
    const cell = h("span", "c-acts");
    const b = h("button", "set-rm", label);
    b.type = "button";
    b.disabled = off;
    b.addEventListener("click", click);
    cell.append(b);
    return cell;
  };

  const known = !!u && u.results !== null;
  const loose = u ? u.cache.files - u.pinned.files : 0;
  const doneOff = !u || !known || !u.daemon || (!u.closed.files && !u.plans && !loose);
  table.append(
    row("Images", u?.cache, {
      folder: "cache/",
      act: button("Flush images", !loose, () => void flush(s, redraw, () => invoke("flush_cache"))),
    }),
    row("Pinned", u?.pinned, { sub: true }),
    row("Results", u?.results, { known }),
    ...(u?.skippers ?? []).map((k) => row(k.name, k.line, { sub: true, folder: k.folder, known })),
    row("Closed work", u?.closed, {
      sub: true,
      known,
      plain: true,
      folder: u && known && !s.counting && (u.items || u.plans) ? `${plural(u.items, "item")} · ${plural(u.plans, "plan folder")}` : "",
      act: button("Flush done work…", doneOff, () => u && void flushDone(s, u, redraw)),
    }),
    row("Logs", u?.logs),
    row("Other", u?.other),
    row("Total", u?.total, { total: true }),
  );

  const status = h("div", "set-hint", s.removed ?? "");
  status.setAttribute("role", "status");
  if (s.removed) status.dataset.kind = "added";
  const errors = [
    s.error,
    u?.ledger_error,
    u && !u.daemon ? "Skiff cannot reach skiffd. Flush done work needs the session list." : null,
    u?.unreadable ? `Skiff could not read ${plural(u.unreadable, "file")}.` : null,
  ]
    .filter((e): e is string => !!e)
    .map((e) => {
      const el = h("div", "set-error", e);
      el.setAttribute("role", "alert");
      return el;
    });

  const days = u?.cache_days ?? 0;
  const on = days > 0;
  const auto = h("div", "set-row set-open set-agent set-disk-auto" + (on ? " on" : ""));
  const sw = h("button", "c-mode set-switch");
  sw.type = "button";
  sw.disabled = !u;
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-checked", String(on));
  sw.setAttribute("aria-label", `Auto flush ${on ? "on" : "off"}`);
  sw.appendChild(h("span", "switch"));
  sw.addEventListener("click", () => setDays(on ? 0 : 30));
  auto.append(h("span", "c-name", "Auto flush"), h("span", "c-cmd", on ? plural(days, "day") : ""), sw);

  return [head, table, status, ...errors, auto];
}
