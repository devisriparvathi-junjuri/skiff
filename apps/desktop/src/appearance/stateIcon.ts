/** The mark for a session's state: skiff under way, anchor, bell, unread flag, or a finished flag. */
import { h } from "../ui/dom";
import { icon } from "../ui/icons";
import type { SessionInfo } from "../platform/types";
import { agentFailed } from "../workspace/model";
import { reportedFailed, statusNote } from "../workspace/programStatus";

export const SKIFF = '<path d="M2 15h20l-4 5H6z"></path><path d="M12 15V3l7 10h-7"></path>';
export const BELL = '<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"></path><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"></path>';
const OK = '<circle cx="12" cy="12" r="10"></circle><path d="M8 12l3 3 5-6"></path>';
export const FLAG = '<path d="M5 21V4"></path><path d="M5 4h13l-3 4.5 3 4.5H5"></path>';
const FAILED = '<circle cx="12" cy="12" r="10"></circle><path d="M9 9l6 6M15 9l-6 6"></path>';

function workingIcon(): HTMLElement {
  const el = h("span", "busy");
  el.appendChild(icon(SKIFF));
  // The sidebar is rebuilt while a session streams output. Start a replacement
  // icon at the elapsed animation phase, rather than its static first frame.
  const now = performance.now();
  el.style.setProperty("--water-delay", `${-(now % 600)}ms`);
  el.style.setProperty("--skiff-delay", `${-(now % 1200)}ms`);
  return el;
}

export function stateIcon(s: SessionInfo): HTMLElement {
  const el = markOf(s);
  // What the program reports: what it waits for, how its turn ended. Plain text.
  const note = statusNote(s);
  if (note) el.title = note;
  return el;
}

function markOf(s: SessionInfo): HTMLElement {
  const mark = (cls: string, paths: string) => {
    const el = h("span", cls);
    el.appendChild(icon(paths));
    return el;
  };
  // The agent ended with an error, and the pane is a shell now. Or the
  // program said its turn failed, and the pane is not read yet.
  if (agentFailed(s) || reportedFailed(s)) return mark("finished failed", FAILED);
  switch (s.state) {
    case "working":
      // A skiff under way: it rocks while the water runs past.
      return workingIcon();
    case "idle":
      // A turn or a command ended while you were away: the result is waiting to be read.
      if (s.unread) return mark("unread", FLAG);
      // Idle is the normal state: no mark.
      return h("span", "anchored");
    case "waiting":
      // The agent asks for approval, or a program rang the bell.
      return mark("belled", BELL);
    case "done":
      return s.exit_code && s.exit_code !== 0
        ? mark("finished failed", FAILED)
        : mark("finished", OK);
  }
}
