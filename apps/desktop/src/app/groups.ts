/** Groups (named splits): persistence through skiffd. */
import { isSlot, PRESETS } from "../workspace/layoutSlots";
import { sessionsOf, shapeName } from "../workspace/layout";

import { agentName } from "../workspace/model";
import type { Group } from "../platform/types";
import { showError } from "../ui/alerts";
import { render } from "./render";
import { S, sessions } from "./state";
import { activeGroupObj, shownIds } from "./stateQueries";

import { inBackground, unfocus } from "../workspace/view";
import { hostCan, hostInvoke } from "./hostCaps";

/** Group writes run one at a time, so a new group has its id before the next save. */
let groupQueue = Promise.resolve();
let groupWrites = 0;
let reloadGroups = false;
/** New groups whose create failed: the Host may have made one anyway, with
 *  the reply lost. Their later saves are dropped, never sent as a second
 *  create; the reload after the queue shows what the Host holds. */
const unsure = new WeakSet<Group>();

/** Agent names joined with " + ", three at most. */
export function autoName(ids: string[]): string {
  const names = ids.map((id) => {
    const s = sessions.get(id);
    return s ? agentName(s) : "session";
  });
  return names.slice(0, 3).join(" + ") + (names.length > 3 ? ` +${names.length - 3}` : "");
}

function persist(op: () => Promise<void>) {
  groupWrites++;
  groupQueue = groupQueue
    .then(op)
    .catch(showError)
    .finally(() => {
      if (--groupWrites === 0 && reloadGroups) {
        reloadGroups = false;
        void loadGroups();
      }
    });
}

/** A group still named after a template follows its shape when panes come and go. */
export function syncTemplateName(g: Group) {
  const shape = shapeName(g.layout);
  if (shape && shape !== g.name && PRESETS.some((p) => p.name === g.name)) g.name = shape;
}

export function saveGroup(g: Group) {
  syncTemplateName(g);
  persist(async () => {
    if (!S.groups.includes(g) || unsure.has(g)) return;
    const creating = g.id.startsWith("tmp-");
    let saved: Group;
    try {
      saved = await hostInvoke<Group>("session_groups", "save_group", {
        group: { id: creating ? "" : g.id, name: g.name, layout: g.layout, focus: g.focus, cwd: g.cwd ?? null },
      });
    } catch (e) {
      if (creating) {
        unsure.add(g);
        reloadGroups = true;
      }
      throw e;
    }
    if (saved.id !== g.id) {
      if (S.activeGroup === g.id) S.activeGroup = saved.id;
      if (S.groupPicked === g.id) S.groupPicked = saved.id;
      // The row's highlight follows the group to its saved id.
      if (S.roveKey === g.id) S.roveKey = saved.id;
      if (S.renaming?.id === g.id) S.renaming.id = saved.id;
      g.id = saved.id;
      render();
    }
  });
}

/** Move `g` next to `anchor` in the list. skiffd keeps the order. */
export function moveGroup(g: Group, anchor: Group, side: "before" | "after") {
  const list = S.groups.filter((x) => x !== g);
  list.splice(list.indexOf(anchor) + (side === "after" ? 1 : 0), 0, g);
  S.groups = list;
  render();
  // Read at run time: a new group has its saved id by then.
  persist(() => hostInvoke("session_groups", "order_groups", { groups: S.groups.map((x) => x.id).filter((id) => !id.startsWith("tmp-")) }));
}

export function deleteGroup(g: Group) {
  S.groups = S.groups.filter((x) => x !== g);
  if (S.activeGroup === g.id) S.activeGroup = null;
  if (S.renaming?.id === g.id) S.renaming = null;
  persist(async () => {
    if (g.id.startsWith("tmp-")) return;
    await hostInvoke("session_groups", "delete_group", { group: g.id }).catch((e) => console.warn(e));
  });
}

export async function loadGroups() {
  if (groupWrites > 0) {
    reloadGroups = true;
    return;
  }
  // A remote Host's groups come in M3; its sessions show without them.
  if (!hostCan("session_groups")) {
    S.groups = [];
    return;
  }
  let list: Group[];
  try {
    list = await hostInvoke<Group[]>("session_groups", "list_groups");
  } catch (e) {
    console.error(e);
    return;
  }
  // A local change started during the await: its own reload follows.
  if (groupWrites > 0) {
    reloadGroups = true;
    return;
  }
  for (const g of list) {
    // Pane focus is local; the daemon keeps the one from the last save.
    const old = S.groups.find((x) => x.id === g.id);
    if (old?.focus && sessionsOf(g.layout).includes(old.focus)) g.focus = old.focus;
  }
  // Groups emptied before the daemon kept their folder: no session and no
  // place, so they piled up under Other. Drop them, unless one is on screen.
  const stale = list.filter((g) => !g.cwd && g.id !== S.activeGroup && sessionsOf(g.layout).every(isSlot));
  const next = list.filter((g) => !stale.includes(g));
  // The same list keeps its objects: the tree compares groups by identity and keeps its rows.
  if (JSON.stringify(next) !== JSON.stringify(S.groups)) S.groups = next;
  for (const g of stale) persist(() => hostInvoke("session_groups", "delete_group", { group: g.id }));
  const was = S.activeGroup;
  if (S.activeGroup && !activeGroupObj()) {
    S.activeGroup = null;
    S.single = S.focused && sessions.has(S.focused) ? S.focused : null;
  }
  const ids = shownIds();
  if (S.focused && !ids.includes(S.focused)) S.focused = ids.find((x) => !isSlot(x)) ?? null;
  if (was && !S.activeGroup && !S.single) inBackground(unfocus);
  else render("groups");
}
