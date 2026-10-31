/**
 * Settings › Disk use: the tree's sums, words and flush plan, with no DOM.
 * The rule itself is in `skiff_client::disk`: each file comes with its
 * verdict, and this file only adds them up the tree.
 */

/** What the rule says of a file. */
export type Kind = "use" | "held" | "prot" | "stale";

export interface Verdict {
  kind: Kind;
  /** The reason code: see `Verdict::why` in `skiff_client::disk`. */
  why: string;
  /** The work's title or a pane's name. */
  label?: string;
  /** The change that holds it, in Unix seconds. */
  changed?: number;
}

/** A file or folder, as `disk_usage` sends it. */
export interface DiskNode {
  name: string;
  /** From the agent folder, with `/`. Empty for the folder itself. */
  path: string;
  bytes: number;
  files: number;
  mtime: number;
  /** A file's verdict, or a folded folder's. */
  state?: Verdict;
  children?: DiskNode[];
  dir?: boolean;
  /** One of Skiff's own folders: never removed. */
  frame?: boolean;
  /** A ledger row's work folder. */
  work?: { uid: string; kind: string; title: string };
}

/** One item a flush removes, as the count showed it. */
export interface Expected {
  path: string;
  bytes: number;
  files: number;
  mtime: number;
}

const kids = (n: DiskNode) => n.children ?? [];

/** The files of `n`: its leaves, a folded folder and an empty folder each count as one. */
export function leaves(n: DiskNode, out: DiskNode[] = []): DiskNode[] {
  if (!kids(n).length) out.push(n);
  else for (const c of kids(n)) leaves(c, out);
  return out;
}

const stale = (n: DiskNode) => n.state?.kind === "stale";

const flushable = new WeakMap<DiskNode, number>();
/** The bytes a flush of `n` removes: its stale files. */
export function canFlush(n: DiskNode): number {
  let v = flushable.get(n);
  if (v === undefined) {
    v = kids(n).length ? kids(n).reduce((a, c) => a + canFlush(c), 0) : stale(n) ? n.bytes : 0;
    flushable.set(n, v);
  }
  return v;
}

/** Every file under `n` is stale, and `n` is not one of Skiff's folders. */
export function allStale(n: DiskNode): boolean {
  if (n.frame) return false;
  return kids(n).length ? kids(n).every(allStale) : stale(n);
}

/**
 * What a flush of `n` removes: each largest part whose files are all stale.
 * The rest stays. `plan` in `skiff_client::disk` does the same.
 */
export function plan(n: DiskNode): { go: DiskNode[]; keep: DiskNode[] } {
  const go: DiskNode[] = [];
  const keep: DiskNode[] = [];
  const walk = (x: DiskNode) => {
    if (x.path !== "" && allStale(x)) go.push(x);
    else if (kids(x).length) kids(x).forEach(walk);
    else if (!stale(x)) keep.push(x);
  };
  walk(n);
  return { go, keep };
}

export const expected = (n: DiskNode): Expected => ({ path: n.path, bytes: n.bytes, files: n.files, mtime: n.mtime });

/** "4 min ago", "3 h ago", "2 days ago" from `now`. */
export function ago(t: number, now: number): string {
  const m = Math.max(1, Math.round((now - t) / 60));
  if (m < 60) return `${m} min ago`;
  if (m < 48 * 60) return `${Math.round(m / 60)} h ago`;
  return `${Math.round(m / 1440)} days ago`;
}

const WORD: Record<Kind, string> = { use: "In use", held: "Held", prot: "Protected", stale: "Stale" };
const WHY: Record<string, string> = {
  done: "done",
  "no-folder": "no work folder",
  image: "image",
  "clean-checkout": "clean worktree",
  working: "working",
  open: "open task",
  "plan-open": "plan open",
  "pane-working": "pane working",
  pane: "pane",
  "skiff-file": "Skiff file",
  pinned: "pinned",
  unreadable: "unreadable",
  dirty: "dirty worktree",
  ahead: "worktree ahead",
  "no-upstream": "no upstream",
  checkout: "git checkout unknown",
  "no-daemon": "skiffd did not answer",
  "no-ledger": "ledger unreadable",
};

/** "Stale · done", "In use · changed 4 min ago". */
export function verdictText(v: Verdict, now: number): string {
  const why = v.why === "changed" && v.changed !== undefined ? `changed ${ago(v.changed, now)}` : WHY[v.why] ?? v.why;
  return `${WORD[v.kind]} · ${why}`;
}

/** A reason group in the dialog: in use, held and protected group by why, stale too. */
export function groupName(v: Verdict): string {
  if (v.why === "changed") return v.kind === "use" ? "In use · changed in the last 15 min" : "Held · changed in the last 24 h";
  return `${WORD[v.kind]} · ${WHY[v.why] ?? v.why}`;
}

/** A row's STATE cell: one verdict, or Mixed with the count of files that stay. */
export function rowState(n: DiskNode, now: number): { kind: Kind | "mixed"; text: string; label: string } {
  const label = n.work?.title ?? "";
  if (!kids(n).length) {
    const v = n.state;
    if (!v) return { kind: "stale", text: "Empty", label };
    return { kind: v.kind, text: verdictText(v, now), label: label || v.label || "" };
  }
  const ls = leaves(n).filter((l) => l.state);
  if (!ls.length) return { kind: "stale", text: "Empty", label };
  const texts = new Set(ls.map((l) => verdictText(l.state!, now)));
  if (texts.size === 1) {
    const v = ls[0].state!;
    const labels = new Set(ls.map((l) => l.state!.label ?? ""));
    return { kind: v.kind, text: verdictText(v, now), label: label || (labels.size === 1 ? v.label ?? "" : "") };
  }
  const stay = ls.filter((l) => !stale(l)).reduce((a, l) => a + l.files, 0);
  if (!stay) return { kind: "stale", text: "Stale", label };
  if (stay === ls.reduce((a, l) => a + l.files, 0)) {
    const kinds = new Set(ls.map((l) => l.state!.kind));
    const k: Kind = kinds.size === 1 ? ls[0].state!.kind : kinds.has("use") ? "use" : kinds.has("held") ? "held" : "prot";
    return { kind: k, text: WORD[k], label };
  }
  return { kind: "mixed", text: `Mixed · ${stay.toLocaleString("en-US")} ${stay === 1 ? "file stays" : "files stay"}`, label };
}

/** Why a row with nothing to flush has no Flush: its newest change that holds it, else nothing. */
export function idleReason(n: DiskNode, now: number): string {
  const t = Math.max(0, ...leaves(n).map((l) => (l.state?.why === "changed" ? l.state.changed ?? 0 : 0)));
  return t ? `Changed ${ago(t, now)}` : "Nothing to flush";
}

/** The rows to draw: `open` folders show their children, by CAN FLUSH, then by name. */
export function visibleRows(root: DiskNode, open: Set<string>): { node: DiskNode; depth: number }[] {
  const out: { node: DiskNode; depth: number }[] = [];
  const walk = (n: DiskNode, depth: number) => {
    out.push({ node: n, depth });
    if (!open.has(n.path)) return;
    [...kids(n)].sort((a, b) => canFlush(b) - canFlush(a) || a.name.localeCompare(b.name)).forEach((c) => walk(c, depth + 1));
  };
  walk(root, 0);
  return out;
}

/** The REMOVE or KEEP lines of the dialog: one per reason, with files, bytes and up to three names. */
export function groups(nodes: DiskNode[], scope: DiskNode): { name: string; kind: Kind; files: number; bytes: number; names: string[] }[] {
  const by = new Map<string, { name: string; kind: Kind; files: number; bytes: number; names: string[] }>();
  // The name a line shows: the work folder or the top part under the scope.
  const shown = (n: DiskNode) => {
    const rel = scope.path ? n.path.slice(scope.path.length + 1) : n.path;
    return rel.split("/")[0] || n.name.replace(/\/$/, "");
  };
  for (const n of nodes) {
    for (const l of leaves(n)) {
      if (!l.state) continue;
      const key = groupName(l.state);
      const g = by.get(key) ?? { name: key, kind: l.state.kind, files: 0, bytes: 0, names: [] };
      g.files += l.files;
      g.bytes += l.bytes;
      const nm = shown(l);
      if (!g.names.includes(nm)) g.names.push(nm);
      by.set(key, g);
    }
  }
  return [...by.values()].sort((a, b) => b.bytes - a.bytes);
}

/** The node at `path`. */
export function find(n: DiskNode, path: string): DiskNode | null {
  if (n.path === path) return n;
  for (const c of kids(n)) if (path === c.path || path.startsWith(c.path + "/")) return find(c, path);
  return null;
}
