/**
 * How the page reports a daemon that differs from the app, and a reload
 * that did not finish. A step the launch takes on its own reports its
 * outcome passively: the badge and the status bar, never a dialog. Only a
 * step the user asked for, such as Restart skiffd, opens anything.
 */
import type { DaemonWarning, Reloaded } from "../platform/types";
import { sentences, t } from "../i18n";

/** The warning panel opens at once only after a step the user asked for. */
export function opensAtOnce(kind: DaemonWarning["kind"], requested: boolean): boolean {
  return requested && (kind === "protocol" || kind === "hung" || kind === "failed");
}

/** What a reload left undone, in one line for the status bar. `null`: it finished. */
export function reloadNote(r: Pick<Reloaded, "error" | "missing" | "lost">): string | null {
  // A backend line may lack its full stop. A catalog line ends in its language's own.
  const end = (line: string) => (/[.。!?！？]$/.test(line) ? line : `${line}.`);
  const lines: string[] = [];
  if (r.error) lines.push(end(r.missing.length ? t("daemon.reloadMissing", { error: r.error, names: r.missing.join(", ") }) : r.error));
  if (r.lost.length) lines.push(t("daemon.inputLost", { names: r.lost.join(", ") }));
  return lines.length ? sentences(lines) : null;
}
