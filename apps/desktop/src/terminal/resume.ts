import { invoke } from "@tauri-apps/api/core";

import { agentKind } from "../appearance/agentIcon";
import { S, sessions } from "../app/state";
import { place } from "../app/stateQueries";
import type { SessionInfo, Was } from "../platform/types";
import { showError } from "../ui/alerts";
import { ctxMenu } from "../ui/contextMenu";
import type { MenuEntry } from "../ui/menu";
import { basename, branchName, samePath } from "../workspace/model";
import { locale, t, tn } from "../i18n";
import { hostInvoke } from "../app/hostCaps";

/**
 * Resume after a restart. A restored pane remembers the agent it ran and that
 * agent's last title. Each agent gets the commands its own CLI documents: a
 * picker to open, or a list to choose from. The user always picks the
 * conversation; Skiff never maps a pane to a conversation id.
 */

interface Recent {
  id: string;
  title: string;
  /** Unix ms, or null when the CLI prints only a date. */
  updated: number | null;
  /** The date as the CLI printed it, when there is no time. */
  day?: string;
}

interface Adapter {
  /** The line that opens the agent's own picker. `title` is the pane's last title. */
  picker?: (cmd: string, title: string | null) => string;
  /** A session list the agent prints, and the line that resumes one of them. */
  list?: {
    args: (program: string, n: number) => string[];
    parse: (out: string, cwd: string) => Recent[];
    resume: (cmd: string, id: string) => string;
  };
  /** The line that continues the newest conversation. Runs only when picked. */
  latest?: (cmd: string) => string;
}

/** Quotes one word for the shell. */
const q = (s: string) => `'${s.replace(/'/g, `'\\''`)}'`;

/** A subcommand goes after the program and before the user's flags: `codex resume --dangerously-…`. */
function sub(cmd: string, ...words: string[]): string {
  const [program, ...flags] = cmd.split(/\s+/);
  return [program, ...words, ...flags].join(" ");
}

/**
 * The line that continues the newest conversation in the folder, from each
 * CLI's help (2026-10-13). Only the Skiff agent's own folder runs it on
 * restore: it holds no other conversation. A custom agent has none.
 */
const CONTINUE: Record<string, (cmd: string) => string> = {
  claude: (cmd) => `${cmd} --continue`,
  codex: (cmd) => sub(cmd, "resume", "--last"),
  opencode: (cmd) => `${cmd} --continue`,
  grok: (cmd) => `${cmd} --continue`,
  gemini: (cmd) => `${cmd} --continue`,
  // hermes: `--continue` takes the newest session anywhere; `--in` keeps it to this folder.
  hermes: (cmd) => `${cmd} --resume latest --in .`,
};

/** The agent's continue line for `cmd`, its command line from Settings. Null: it has none. */
export function continueLine(agent: string, cmd: string): string | null {
  return CONTINUE[agent]?.(cmd) ?? null;
}

const ADAPTERS: Record<string, Adapter> = {
  // claude 2.1: --resume opens the picker, and a word that is not an id fills its search.
  claude: { picker: (cmd, title) => `${cmd} --resume${title ? " " + q(title) : ""}` },
  // codex 0.159: `codex resume` opens the picker for this folder. Flags go after `resume`.
  codex: { picker: (cmd) => sub(cmd, "resume") },
  // opencode 1.18: no picker flag, a JSON list with each session's folder.
  opencode: {
    list: {
      args: (p, n) => [p, "session", "list", "--format", "json", "-n", String(n)],
      parse: (out, cwd) => {
        const rows = JSON.parse(out) as { id: string; title?: string; updated?: number; directory?: string }[];
        return rows
          .filter((r) => !r.directory || samePath(r.directory, cwd))
          .map((r) => ({ id: r.id, title: r.title || t("resume.noTitle"), updated: r.updated ?? null }));
      },
      resume: (cmd, id) => `${cmd} -s ${q(id)}`,
    },
  },
  // grok 1.0: no picker flag, a text table of this folder's sessions.
  grok: {
    list: {
      args: (p, n) => [p, "sessions", "list", "-n", String(n)],
      parse: (out) =>
        out
          .split("\n")
          .map((l) => l.match(/^([0-9a-f-]{36})\s+(\S+)\s+(\S+)\s+(\S+)\s+(.*)$/))
          .filter((m): m is RegExpMatchArray => !!m)
          .map((m) => ({ id: m[1], title: m[5], updated: uuidTime(m[1]), day: m[3] })),
      resume: (cmd, id) => `${cmd} --resume ${q(id)}`,
    },
  },
  // gemini (agy 1.2): no list and no picker. Only the newest conversation, on request.
  gemini: { latest: CONTINUE.gemini },
  // hermes: its picker is inside the TUI, and the `sessions list` format is not documented.
  hermes: { latest: CONTINUE.hermes },
};

/** How many sessions the list shows first, and after Show more. */
const FIRST = 8;
const MORE = 50;

/** An agent's own name, which it shows before a conversation has a topic. skiffd drops these too. */
const GENERIC = new Set(["claude", "claude code", "codex", "openai codex", "opencode", "gemini", "gemini cli", "antigravity", "agy", "grok", "grok build", "hermes", "hermes agent"]);

/** The agent a restored pane offers to resume, if Skiff can help with it. */
export function resumeOffer(s: SessionInfo): Was | null {
  const was = s.resume;
  if (!was || s.state === "done" || agentKind(s) !== "shell" || !ADAPTERS[was.agent]) return null;
  // A workspace saved before skiffd dropped generic titles may still hold one.
  const title = was.title?.replace(/^[^\p{L}\p{N}]+/u, "").trim();
  return { agent: was.agent, title: title && !GENERIC.has(title.toLowerCase()) ? title : null };
}

/** A pane that resumes without a choice in Skiff: its agent has a picker. */
const opensPicker = (s: SessionInfo) => {
  const was = resumeOffer(s);
  return !!was && !!ADAPTERS[was.agent].picker;
};

/** The command line the user set for the agent, else its default. A removed agent runs its program: gemini is agy. */
function commandFor(agent: string): string {
  return S.agents.find((a) => a.id === agent)?.command || (agent === "gemini" ? "agy" : agent);
}

/** Types a line into the pane's shell and runs it, with the image cache's read flag for Claude Code at the end. */
function run(id: string, line: string) {
  const agent = sessions.get(id)?.resume?.agent;
  const flags = agent ? invoke<string>("agent_cache_flags", { agent }).catch(() => "") : Promise.resolve("");
  void flags.then((f) => hostInvoke("session_terminal", "pty_write", { session: id, data: line + f + "\r" })).catch(showError);
}

/** A UUIDv7 starts with its creation time in ms. grok prints only a date, so its ids give the time. */
function uuidTime(id: string): number | null {
  const hex = id.replace(/-/g, "");
  return hex.length === 32 && hex[12] === "7" ? parseInt(hex.slice(0, 12), 16) : null;
}

// English keeps "3:42 PM"; other languages take their own clock.
const clock = new Intl.DateTimeFormat(locale(), { hour: "numeric", minute: "2-digit" });
const date = new Intl.DateTimeFormat(locale(), { month: "short", day: "numeric" });

/** "3:42 PM" today, "Yesterday 3:42 PM", else "Sep 30, 3:42 PM". */
function when(r: Recent): string {
  if (r.updated == null) return r.day ?? "";
  const at = new Date(r.updated);
  const days = Math.round((new Date().setHours(0, 0, 0, 0) - new Date(at).setHours(0, 0, 0, 0)) / 86400000);
  if (days === 0) return clock.format(at);
  if (days === 1) return t("resume.yesterday", { time: clock.format(at) });
  return t("resume.dateTime", { date: date.format(at), time: clock.format(at) });
}

/** Where the list opens: a menu point, or under a button with its right edge. */
type Anchor = { x: number; y: number; right?: number };

/** Opens the menu at `at`. Under a button, its right edge lines up with the button's. */
function openAt(at: Anchor, title: string, entries: MenuEntry[]) {
  ctxMenu.open(at.x, at.y, title, entries);
  const menu = document.getElementById("ctx-menu");
  if (menu && at.right != null) menu.style.left = `${Math.max(8, at.right - menu.offsetWidth)}px`;
}

/** The Resume button or menu item: open the picker, or show the list under `at`. */
export function resumePane(id: string, at: Anchor) {
  const s = sessions.get(id);
  const was = s && resumeOffer(s);
  if (!s || !was) return;
  const a = ADAPTERS[was.agent];
  const cmd = commandFor(was.agent);
  if (a.picker) return run(id, a.picker(cmd, was.title));
  const p = place(s);
  const where = p ? branchName(p.worktree) : basename(s.cwd);
  const title = t("resume.recentIn", { agent: was.agent, where });
  if (a.latest) {
    openAt(at, title, [{ icon: "schedule-refresh-cw", label: t("resume.continueLatest"), hint: a.latest(cmd), run: () => run(id, a.latest!(cmd)) }]);
    return;
  }
  const list = a.list!;
  const show = async (n: number) => {
    openAt(at, title, [{ label: t("resume.loading"), disabled: true }]);
    let rows: Recent[];
    try {
      const program = cmd.split(/\s+/)[0];
      const out = await invoke<string>("list_agent_sessions", { command: list.args(program, n), cwd: s.cwd });
      rows = list.parse(out, s.cwd);
    } catch (e) {
      ctxMenu.close();
      return showError(e);
    }
    // The user may have closed the menu or moved on while the CLI ran.
    if (!ctxMenu.isOpen) return;
    const entries: MenuEntry[] = rows.map((r) => ({
      icon: "·",
      label: r.title,
      hint: r.title === was.title ? t("resume.lastTitle", { when: when(r) }) : when(r),
      run: () => run(id, list.resume(cmd, r.id)),
    }));
    if (!entries.length) entries.push({ label: t("resume.none"), disabled: true });
    if (rows.length >= n && n < MORE) entries.push({ icon: "·", label: t("resume.showMore"), run: () => void show(MORE) });
    openAt(at, title, entries);
  };
  void show(FIRST);
}

/** Group menu: open every picker at once. Panes that need a pick from a list keep their button. */
export function resumeAllEntry(ids: string[]): MenuEntry | null {
  const ready = ids.filter((id) => {
    const s = sessions.get(id);
    return !!s && opensPicker(s);
  });
  if (!ready.length) return null;
  return {
    icon: "schedule-refresh-cw",
    label: tn("resume.sessions", ready.length),
    run: () => ready.forEach((id) => resumePane(id, { x: 0, y: 0 })),
  };
}
