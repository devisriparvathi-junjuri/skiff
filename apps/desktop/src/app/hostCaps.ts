/** The selected Host's capabilities. Every session and terminal call goes
 *  through `hostInvoke`, which refuses a call the Host does not offer, and
 *  every surface asks `hostCan` before it offers an action. The Host
 *  connection checks each request again; nothing falls back to local data. */
import { invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { type MessageKey, t } from "../i18n";

type HostCap =
  | "session_inventory"
  | "session_screen"
  | "session_updates"
  | "session_terminal"
  | "session_lifecycle"
  | "session_groups"
  | "session_agents"
  | "program_status"
  | "daemon_metrics"
  | "projects"
  | "git_status"
  | "git_diff"
  | "project_config"
  | "worktrees"
  | "files"
  | "folders"
  | "project_assets"
  | "ledger"
  | "skipper"
  | "skipper_config"
  | "skipper_file"
  | "skipper_run"
  | "skipper_fleets"
  | "agent_config";

/** The major of each capability that this page speaks. */
const MAJOR: Record<HostCap, number> = {
  session_inventory: 1,
  session_screen: 1,
  session_updates: 1,
  session_terminal: 1,
  session_lifecycle: 1,
  session_groups: 1,
  session_agents: 1,
  program_status: 1,
  daemon_metrics: 1,
  projects: 1,
  git_status: 1,
  git_diff: 1,
  project_config: 1,
  worktrees: 1,
  files: 1,
  folders: 1,
  project_assets: 1,
  ledger: 1,
  skipper: 1,
  skipper_config: 1,
  skipper_file: 1,
  skipper_run: 1,
  skipper_fleets: 2,
  agent_config: 1,
};

/** A capability the page names apart from its wire name: fleet Skippers are `skipper_run@2`. */
const WIRE: Partial<Record<HostCap, string>> = { skipper_fleets: "skipper_run" };

/** What each capability offers, for the refusal. */
const OFFERS: Record<HostCap, MessageKey> = {
  session_inventory: "hostCaps.session_inventory",
  session_screen: "hostCaps.session_screen",
  session_updates: "hostCaps.session_updates",
  session_terminal: "hostCaps.session_terminal",
  session_lifecycle: "hostCaps.session_lifecycle",
  session_groups: "hostCaps.session_groups",
  session_agents: "hostCaps.session_agents",
  program_status: "hostCaps.program_status",
  daemon_metrics: "hostCaps.daemon_metrics",
  projects: "hostCaps.projects",
  git_status: "hostCaps.git_status",
  git_diff: "hostCaps.git_diff",
  project_config: "hostCaps.project_config",
  worktrees: "hostCaps.worktrees",
  files: "hostCaps.files",
  folders: "hostCaps.folders",
  project_assets: "hostCaps.project_assets",
  ledger: "hostCaps.ledger",
  skipper: "hostCaps.skipper",
  skipper_config: "hostCaps.skipper_config",
  skipper_file: "hostCaps.skipper_file",
  skipper_run: "hostCaps.skipper_run",
  skipper_fleets: "hostCaps.skipper_fleets",
  agent_config: "hostCaps.agent_config",
};

/** `name@major` of the selected Host. `null` until the first answer. */
let caps: Set<string> | null = null;
let loading: Promise<void> | null = null;

/** Asks the selected Host again: at launch and after the daemon reloads. */
export function loadHostCaps(): Promise<void> {
  const load = invoke<string[]>("host_capabilities").then((list) => {
    caps = new Set(list);
  });
  loading = load.finally(() => {
    if (loading === load) loading = null;
  });
  return load;
}

/** The Host offers `cap`. Before the first answer, a surface stays as it is;
 *  the call itself still waits for the answer in `hostInvoke`. */
export function hostCan(cap: HostCap): boolean {
  return !caps || caps.has(`${WIRE[cap] ?? cap}@${MAJOR[cap]}`);
}

/** `invoke`, only when the selected Host offers `cap`. Once the
 *  capabilities are known it calls at once, so keystrokes keep their order. */
export async function hostInvoke<T = void>(cap: HostCap, cmd: string, args?: InvokeArgs): Promise<T> {
  if (!caps) await (loading ?? loadHostCaps());
  if (!hostCan(cap)) throw new Error(t("hostCaps.refusal", { what: t(OFFERS[cap]) }));
  return invoke<T>(cmd, args);
}
