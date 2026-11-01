import type { AgentInfo } from "../platform/types";
import { t } from "../i18n";

/**
 * Each known agent's model flag, from its `--help` (2026-10-15). Keep in step
 * with `model_flag` in crates/skiff-core/src/tasks.rs, which builds the line
 * that runs. Settings only shows a hint from this.
 */
const MODEL_FLAGS: Record<string, string> = { claude: "--model", codex: "-m", grok: "-m", kimi: "-m" };

/** The rule `check_name` in skiff-core keeps too. */
export const TASK_TYPE_NAME = /^[a-z0-9][a-z0-9-]*$/;

/** Why a task type name cannot be saved. */
export const badTaskTypeName = () => t("tasks.badName");

/** The agent's model flag. Null: it takes its model in its command, as a custom agent does. */
export function modelFlag(agent: AgentInfo | undefined): string | null {
  return agent && !agent.custom ? (MODEL_FLAGS[agent.id] ?? null) : null;
}

/** The command line a task type starts, as skiffd builds it. An empty model is the agent's default. */
export function taskCommand(agent: AgentInfo | undefined, model: string): string {
  if (!agent) return "";
  const cmd = agent.command.trim();
  const flag = modelFlag(agent);
  const m = model.trim();
  return flag && m ? `${cmd} ${flag} ${m}` : cmd;
}

/** Why a model cannot be saved, or null. */
export function modelError(model: string): string | null {
  return /\s/.test(model.trim()) ? t("tasks.badModel") : null;
}

/** "640 B", "3.4 KB". */
export function fileSize(n: number): string {
  return n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KB`;
}
