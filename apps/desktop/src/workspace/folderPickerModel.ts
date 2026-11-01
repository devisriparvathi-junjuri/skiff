/** The folder picker's pure parts: crumbs, names, times and filtering. */
import type { FolderEntry, FolderRepo } from "../platform/types";
import { t, tn } from "../i18n";

/** One row of the picker's main list. `path` is the Host's. */
export interface PickRow {
  name: string;
  path: string;
  repo: FolderRepo | null;
  link: boolean;
  /** The folder it is in, for Recent and Git repos rows. */
  sub?: string;
}

/** The breadcrumb of `real`: the Host's name for `/`, then each folder. */
export function crumbs(real: string, host: string): { label: string; path: string }[] {
  const out = [{ label: host, path: "/" }];
  let at = "";
  for (const part of real.split("/").filter(Boolean)) {
    at += `/${part}`;
    out.push({ label: part, path: at });
  }
  return out;
}

/** The last name of a Host path; `/` for the root. */
export function folderName(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? "/";
}

/** The folder above a Host path. `null` at the root. */
export function parentPath(path: string): string | null {
  const parts = path.split("/").filter(Boolean);
  if (!parts.length) return null;
  return `/${parts.slice(0, -1).join("/")}`;
}

/** The child `name` of a Host folder. */
export function childPath(dir: string, name: string): string {
  return `${dir.replace(/\/+$/, "")}/${name}`;
}

export function listRows(dir: string, entries: FolderEntry[]): PickRow[] {
  return entries.map((e) => ({ name: e.name, path: childPath(dir, e.name), repo: e.repo ?? null, link: e.kind === "link" }));
}

/** Rows whose name holds `filter`, ignoring case. */
export function filterRows(rows: PickRow[], filter: string): PickRow[] {
  const f = filter.trim().toLowerCase();
  return f ? rows.filter((r) => r.name.toLowerCase().includes(f)) : rows;
}

/** A new folder's name: one name, no `/`. The Host checks it again. */
export function newFolderName(typed: string): string {
  const name = typed.trim();
  if (!name || name === "." || name === ".." || name.includes("/") || name.includes("\0")) throw t("picker.oneName");
  return name;
}

/** When a branch last moved, as "2 h ago" or "today". */
export function whenText(seconds: number | null | undefined, now = Date.now()): string {
  if (!seconds) return "";
  const ago = Math.max(0, now / 1000 - seconds);
  if (ago < 3600) return tn("disk.agoMin", Math.max(1, Math.round(ago / 60)));
  if (ago < 6 * 3600) return tn("disk.agoHours", Math.round(ago / 3600));
  const day = (ms: number) => new Date(ms).toDateString();
  if (day(seconds * 1000) === day(now)) return t("picker.today");
  if (day(seconds * 1000) === day(now - 86_400_000)) return t("picker.yesterday");
  if (ago < 30 * 86_400) return tn("disk.agoDays", Math.round(ago / 86_400));
  return new Date(seconds * 1000).toISOString().slice(0, 10);
}
