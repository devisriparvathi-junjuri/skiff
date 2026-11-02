/** What a session's program reports about itself (OSC 7501), as words for
 *  marks, labels and Mission control. Only when the Host offers
 *  `program_status@1`. The program's text is plain text: it goes into
 *  `textContent` and attributes, never into markup. */
import { hostCan } from "../app/hostCaps";
import { type MessageKey, t, tn } from "../i18n";
import type { ProgramStatus, SessionInfo } from "../platform/types";

/** The report, when the Host offers it. */
export function reported(s: SessionInfo): ProgramStatus | null {
  return hostCan("program_status") ? (s.program_status ?? null) : null;
}

/** The program said its turn ended in an error, and the pane is not read yet. */
export function reportedFailed(s: SessionInfo): boolean {
  return s.state === "idle" && !!s.unread && reported(s)?.state === "error";
}

const KIND: Record<string, MessageKey> = { permission: "status.permission", question: "status.question", auth: "status.auth" };

/** One line on what the report adds to the state: what a waiting program
 *  waits for, a turn's end, its progress, a long silence. "" when it adds nothing. */
export function statusNote(s: SessionInfo): string {
  const p = reported(s);
  if (!p) return "";
  const what =
    s.state === "waiting" && p.state === "blocked"
      ? p.kind && KIND[p.kind]
        ? t(KIND[p.kind])
        : ""
      : s.state === "idle" && s.unread && p.state === "done"
        ? t("status.done")
        : reportedFailed(s)
          ? t("status.failed")
          : s.state === "working" && p.stalled
            ? t("status.stalled")
            : s.state === "working" && p.progress != null
              ? t("status.progress", { n: p.progress })
              : "";
  // A message says more only while the program waits or after its turn.
  const msg = s.state !== "working" && p.state !== "idle" ? (p.msg ?? "") : "";
  if (what && msg) return t("status.withMsg", { what, msg });
  return what || msg;
}

/** The progress to draw, 0 to 100, while the program works or waits. */
export function progressOf(s: SessionInfo): number | null {
  const p = reported(s);
  return p && p.progress != null && (s.state === "working" || s.state === "waiting") ? p.progress : null;
}

/** "3 subtasks", or "1 subtask waits" while one waits. "" with none. */
export function childrenNote(s: SessionInfo): string {
  const kids = reported(s)?.children ?? [];
  if (!kids.length) return "";
  const waiting = kids.filter((c) => c.state === "blocked").length;
  return waiting ? tn("status.childrenWaiting", waiting) : tn("status.children", kids.length);
}

/** The state word, with the note when there is one: for labels a screen reader reads. */
export function stateLabel(s: SessionInfo): string {
  const note = statusNote(s);
  const word = t(`state.${s.state}`);
  return note ? `${word}, ${note}` : word;
}

/** What a mark, a label or a card shows of the report, to tell when to redraw. */
export const statusSig = (s: SessionInfo) => [statusNote(s), progressOf(s), childrenNote(s)];
