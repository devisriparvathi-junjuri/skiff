/** What the user has seen: the pane with the keys, while the window is in front. */

import { S, sessions } from "./state";
import { previewOnScreen } from "../terminal/peek";
import { hostCan, hostInvoke } from "./hostCaps";

/** A picked group row holds the keys: the focused pane is shown but not selected. */
export const keysOffPanes = () => !!S.groupPicked && S.groupPicked === S.activeGroup;

/** True when `id` is the selected pane of the active window. Its result is in front of the user. A preview over it hides it. */
export const hasKeys = (id: string) => S.focused === id && document.hasFocus() && !keysOffPanes() && !previewOnScreen();

/** Set while the app reconnects after a reload: its refreshes and renders mark nothing seen. */
let quiet = false;
export function quietSeen(on: boolean) {
  quiet = on;
}

/** Drops the session's unread flag and failed mark, here and in the daemon. */
export function markSeen(id: string) {
  if (quiet) return;
  const s = sessions.get(id);
  if (!s || (!s.unread && s.agent_exit == null)) return;
  // A remote Host keeps its marks until M3 lets this app change them.
  if (!hostCan("session_lifecycle")) return;
  s.unread = false;
  s.agent_exit = null;
  hostInvoke("session_lifecycle", "session_seen", { session: id }).catch(console.error);
}
