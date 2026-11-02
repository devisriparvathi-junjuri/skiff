// Wire types from skiff-core, as the Tauri commands return them.

export type SessionState = "working" | "waiting" | "idle" | "done";
export type Role = "agent" | "task" | "server" | "shell";

export interface SessionInfo {
  id: string;
  label: string;
  role: Role;
  cwd: string;
  command: string;
  /** Program in the PTY's foreground process group. Absent from older daemons. */
  foreground_program?: string | null;
  args: string[];
  state: SessionState;
  cols: number;
  rows: number;
  pid: number | null;
  exit_code: number | null;
  /** This terminal's own theme id; wins over the project's. */
  theme: string | null;
  /** A name Skiff or the user gave; wins over `title`. */
  name: string | null;
  /** Terminal title the program set, if any. */
  title: string | null;
  /** Unix ms. */
  started_at: number;
  /** Unix ms. */
  last_output_at: number;
  /** Total bytes read from the PTY. */
  output_bytes?: number;
  /** Other worktrees its processes ran in during the last minute. Absent from an older daemon. */
  away?: Away[];
  /** The last agent seen in front, and its title. Absent from an older daemon. */
  was?: Was | null;
  /** `was` as a restore found it, until an agent runs again. Absent from an older daemon. */
  resume?: Was | null;
  /** An agent's turn or a shell command ended, and no pane with the keys has shown it since. Absent from an older daemon. */
  unread?: boolean;
  /** Exit code of the last agent the pane ran, until a pane with the keys shows it. Absent from an older daemon. */
  agent_exit?: number | null;
  /** A tool in the app's peek. Not a session to the user. Absent from older daemons. */
  peek?: boolean;
  /** A peek command took the terminal: a terminal tool. */
  interactive?: boolean;
  /** Counts the input the session took. A guarded submit names it. */
  input_gen?: number;
  /** The Skiff agent's session. skiffd marks at most one live session. */
  coordinator?: boolean;
  /** A project or fleet Skipper: its scope, a project's name or "fleet:<fleet>". A restore drops it. */
  skipper?: string;
  /** A Skipper a fleet Skipper took over: a plain session until skiffd ends it when idle. */
  retiring?: boolean;
  /** The `skiff watch` processes registered for this session. Absent when none is, or from a daemon older than protocol 27. */
  watch?: WatchInfo | null;
  /** The program's own OSC 7501 report while it lasts; it decides `state` unless `ended`. Absent when the program reports nothing, or from an older daemon. */
  program_status?: ProgramStatus | null;
}

/** OSC 7501 (Program Status Protocol). `state` shows "blocked" as waiting, and "done" and "error" as idle. */
export interface ProgramStatus {
  /** "idle", "working", "blocked", "done" or "error". */
  state: string;
  /** What a blocked program waits for: "permission", "question" or "auth". */
  kind?: string | null;
  /** Plain text: never render it as markup. */
  msg?: string | null;
  title?: string | null;
  /** 0 to 100, while working or blocked. */
  progress?: number | null;
  app?: string | null;
  /** Working, with no output for 5 minutes: it may hang. */
  stalled?: boolean;
  /** The program left; a done or error kept until the pane is seen. It decides nothing. */
  ended?: boolean;
  /** Its records under its own (sessions, subagents), by id. */
  children?: ProgramChild[];
}

/** A record a program reports under its own: one of its sessions or subagents. */
export interface ProgramChild {
  id: string;
  state: string;
  kind?: string | null;
  /** Plain text: never render it as markup. */
  msg?: string | null;
  title?: string | null;
  progress?: number | null;
}

export interface DaemonMetrics {
  version: string;
  pid: number;
  started_at: number;
  memory_bytes: number;
  session_count: number;
}

/** The watchers registered for a session. */
export interface WatchInfo {
  /** Unix ms of the oldest live registration. */
  since: number;
  pids: number[];
  /** The newest failure a watcher reports. */
  failure?: WatchFailure | null;
  /** The live panes with an open ledger task that the watchers follow. Absent: unknown (an older watcher or daemon, or no ledger read yet). */
  follows?: string[] | null;
}

export interface WatchFailure {
  /** "undelivered": a line was typed and no turn started. "ledger": the ledger could not be read. */
  kind: string;
  /** The watch log line. */
  message: string;
  /** Unix ms. */
  at: number;
}

/** An agent that ran in a session, and the last title it set. */
export interface Was {
  agent: string;
  title: string | null;
}

/** A worktree other than the session's own, where one of its processes ran. */
export interface Away {
  /** The worktree root. */
  path: string;
  /** The last process seen there. */
  command: string;
  /** Unix ms. */
  at: number;
}

/** A project's branches from the Host, for New worktree. */
export interface HostBranches {
  /** The branch the project's folder has checked out. */
  head: string | null;
  /** Newest commit first. `remote`: `origin/main` and the like. */
  branches: { name: string; remote?: boolean; worktree?: boolean }[];
}

export interface Worktree {
  path: string;
  /** Short branch name. `null` when detached. */
  branch: string | null;
  head: string;
  is_main: boolean;
  locked: boolean;
  prunable: boolean;
}

export interface Project {
  name: string;
  short: string;
  path: string;
  color: string | null;
  agents: string[];
  /** Project icon as a data URL, configured or found in the repo. */
  icon: string | null;
  /** Absolute path of the background image behind the terminals, when set. */
  background: string | null;
  /** Pane opacity over the background image, in percent. Absent: the default. */
  background_opacity?: number | null;
  worktrees: Worktree[];
  /** The branch the main checkout normally holds. `null` when unknown or from an older skiffd. */
  default_branch?: string | null;
  /** `folder`: a plain folder in no git repository. Its only worktree is the folder. Absent from an older daemon. */
  kind?: "git" | "folder";
  /** Path missing, and so on. `worktrees` is empty then. */
  error: string | null;
  /** Out of the tree until it opens again. Its worktrees are not read. Absent from an older daemon. */
  closed?: boolean;
  /** The fleet it sits in. Absent: under Projects, in no fleet. */
  fleet?: string | null;
  /** `changes = false` in the Host's projects.toml: no Changes section. Absent: shown. */
  changes?: boolean | null;
  /** The project's own theme id. Absent: the app theme. */
  theme?: string | null;
  /** Shows a Files tree under each worktree. */
  files?: boolean;
}

/** A diff as plain text from the selected Host's git (`git_diff@1`). */
export interface HostDiff {
  /** Unified diff text. Empty: no change. */
  text: string;
  /** Longer than the Host's limit: `text` is its start, cut at a line end. */
  truncated: boolean;
  /** A binary file: `text` holds git's note only. */
  binary: boolean;
  /** An untracked file, compared with an empty file. */
  untracked: boolean;
}

/** An entry of a Host's folder, as it is on its disk: a link is not followed (`files@1`). */
export interface HostEntry {
  name: string;
  kind: "dir" | "file" | "link" | "other";
}

/** One folder of a worktree from the selected Host. */
export interface HostListing {
  entries: HostEntry[];
  /** More entries than the Host lists: the rest are left out. */
  truncated: boolean;
}

/** A file's preview from the selected Host. Its kind comes from its bytes, never its name. */
export interface HostFile {
  content: "text" | "image" | "binary" | "link";
  size: number;
  mime?: string;
  /** A text file, cut at 512 KiB when `truncated`. */
  text?: string;
  /** An image's bytes, base64. Absent for an image over 5 MiB (`truncated`). */
  bytes?: string;
  truncated: boolean;
  /** Where a link points. It was not followed. */
  target?: string;
}

export type DaemonEvent =
  | { event: "state"; session: string; state: SessionState }
  | { event: "exit"; session: string; code: number | null }
  | { event: "session_created"; session: SessionInfo }
  | { event: "session_removed"; session: string }
  | { event: "session_updated"; session: SessionInfo }
  | { event: "turn_ended"; session: string }
  | { event: "groups_changed" }
  | { event: "title"; session: string; title: string | null }
  | { event: "away"; session: string; away: Away[] };

export interface AgentInfo {
  id: string;
  /** What the + menu runs: the user's command, else the default. */
  command: string;
  /** Empty for a custom agent. */
  default_command: string;
  /** Added by the user. Absent from an older daemon. */
  custom?: boolean;
  installed: boolean;
  enabled: boolean;
  /** What starts it: "Used by task type review". It cannot be removed then. Absent: nothing. */
  used_by?: string | null;
}

/** `[appearance]` in the daemon config. `font_size` is absent from an older daemon. */
export interface Appearance {
  theme: string | null;
  font_size?: number | null;
  /** Mission control's filter: `running` or `all`. Absent: `running`. */
  mission_filter?: string | null;
  /** The app's language, such as `es`. Absent: the system language. */
  language?: string | null;
}

export interface DaemonStatus {
  connected: boolean;
  version: string | null;
  /** The daemon's protocol. `null` when the app could not ask. Absent from an older app backend. */
  protocol?: number | null;
  socket: string;
  /** The daemon's pid, when the app could read it. */
  pid: number | null;
  spawned: boolean;
  /** The daemon differs from this app and was not replaced. */
  warning: DaemonWarning | null;
  /** The version of an older daemon this app replaced or reloaded. */
  replaced: string | null;
  /** The daemon is older and can reload onto the app's skiffd. */
  reload: boolean;
}

/** What `reload_daemon` did. */
export interface Reloaded {
  status: DaemonStatus;
  /** Why the reload failed or is not proven. `null`: it worked. */
  error: string | null;
  /** The app talks to a new connection: the page subscribes again. */
  reconnected: boolean;
  /** Live sessions that did not come through, by name. */
  missing: string[];
  /** Sessions whose input, typed during the reload, was dropped. */
  lost: string[];
}

export interface DaemonWarning {
  /** `remote`: a remote Host this app reaches but never restarts or updates.
   *  `identity`: that Host answered with another Skiff identity. */
  kind: "outdated" | "protocol" | "newer" | "hung" | "failed" | "remote" | "identity" | "update";
  message: string;
  /** Live sessions a restart stops. `null` when the daemon did not say. */
  sessions: number | null;
}

/** A git repository's state, read by the Host from its files. */
export interface FolderRepo {
  branch: string | null;
  /** Unix seconds when the branch last moved. */
  last_commit: number | null;
}

/** One folder on a Host (`folders@1`): a folder or a link to one. */
export interface FolderEntry {
  name: string;
  kind: "dir" | "link";
  repo?: FolderRepo;
}

/** The folders in one Host folder. Paths are the Host's own. */
export interface FolderListing {
  path: string;
  /** The same folder, links resolved. */
  real: string;
  parent: string | null;
  entries: FolderEntry[];
  /** More than 2000 folders: the rest are left out. */
  truncated: boolean;
}

/** The picker's Places from the Host: Recent, and repositories under home. */
export interface FolderPlaces {
  /** Newest first. */
  recent: string[];
  repos: { path: string; branch: string | null; last_commit: number | null }[];
  /** A scan runs: ask again. */
  scanning: boolean;
}

/** A Host error as the Host typed it. */
export interface HostError {
  code: string;
  message?: string;
}

export interface FolderInfo {
  /** Git top level, or the folder itself when it is in no repository. `null` when it cannot be added. */
  root: string | null;
  name: string;
  short: string;
  color: string;
  branch: string | null;
  worktrees: number;
  /** The icon the Host found in the repo, as a data URL. */
  icon: string | null;
  /** Why the folder cannot be added. */
  error: string | null;
}

export type SplitDir = "row" | "col";

/** `row`: a left, b right. `col`: a top, b bottom. `ratio` is the share of `a`. */
export type Layout =
  | { type: "pane"; session: string }
  | { type: "split"; dir: SplitDir; ratio: number; a: Layout; b: Layout };

/** A named split kept by the daemon. `id` is empty until the daemon assigns one. */
export interface Group {
  id: string;
  name: string;
  layout: Layout;
  focus: string | null;
  /** Where the group's empty panes start their sessions. */
  cwd?: string | null;
}

export type ThemeColorFamily = "Neutral" | "Beige" | "Red" | "Orange" | "Yellow" | "Green" | "Cyan" | "Blue" | "Purple" | "Pink";

export interface TerminalTheme {
  /** builtin:<slug> or file:<name>. */
  id: string;
  name: string;
  source: "built-in" | "file";
  foreground: string;
  background: string;
  cursor: string | null;
  cursor_foreground?: string;
  selection: string | null;
  selection_foreground?: string;
  color_family?: ThemeColorFamily;
  main_color?: string;
  /** ANSI 0–15. */
  palette: string[];
  /** App colors from the theme file (Superset's ui block), when it has them. */
  ui?: Record<string, string>;
}
