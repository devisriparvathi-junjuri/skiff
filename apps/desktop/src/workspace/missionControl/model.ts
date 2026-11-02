/** Mission control's pure model helpers: the text a card matches, and when a session last did anything. */
import type { Project, SessionInfo } from "../../platform/types";
import type { Entry } from "../ledger";
import { ledgerTitle } from "../ledger";
import { agentName, taskTitle } from "../model";
import { t } from "../../i18n";

/** What the search matches: project, agent, title, task and state, as the palette does. Other: its folder. */
export function matchText(s: SessionInfo, p: Project | null, row: Entry | undefined): string {
  const where = p ? p.name : `other ${s.cwd}`;
  return `${where} ${agentName(s)} ${taskTitle(s)} ${row ? ledgerTitle(row) : ""} ${s.state}`;
}

// ---- Time in state ----

/** When each session was last seen in its state. The first sight takes the last output, or now for a working one. */
const since = new Map<string, { state: string; at: number }>();
export function noteSince(s: SessionInfo, now: number) {
  const was = since.get(s.id);
  if (!was) since.set(s.id, { state: s.state, at: s.state === "working" ? now : s.last_output_at || now });
  else if (was.state !== s.state) since.set(s.id, { state: s.state, at: now });
}
export const sinceText = (s: SessionInfo, now: number) => {
  // G.ago's steps, in the catalog's words: "now", "4m", "2h", "3d".
  const m = Math.floor(Math.max(0, now - (since.get(s.id)?.at ?? now)) / 60000);
  if (m < 1) return t("activity.now");
  if (m < 60) return t("activity.minutes", { n: m });
  if (m < 1440) return t("activity.hours", { n: Math.round(m / 60) });
  return t("activity.days", { n: Math.round(m / 1440) });
};
