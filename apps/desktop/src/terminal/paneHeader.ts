import type { SessionInfo } from "../platform/types";
import { agentIcon, agentKind } from "../appearance/agentIcon";
import { slotHead } from "../workspace/canvas";
import { isSlot } from "../workspace/layoutSlots";

import { type Action, keyLabel } from "../app/keys";
import { toggleFocusMode, toggleMaximize } from "../app/modes";
import { rune, type RuneName } from "../ui/runes";
import { branchName, isProjectSkipper, scopeName, skipperBase, taskTitle } from "../workspace/model";
import { faceSig, skipperFace } from "../workspace/skipperFace";
import { stateIcon } from "../appearance/stateIcon";
import { progressOf, statusNote, statusSig } from "../workspace/programStatus";

import { button, h } from "../ui/dom";

import { icon } from "../ui/icons";

import { S, sessions } from "../app/state";
import { accent, awayPlaces, inShownGroup, place, shownIds } from "../app/stateQueries";

import { closePane, dragSessions, focusPane } from "./paneActions";
import { resumeOffer, resumePane } from "./resume";
import { imagesButton, newestImage } from "./images";
import { insertShown } from "./insertSignal";
import { t } from "../i18n";
/** One pane header: the agent icon, the title, then the close button. */
export function paneHead(id: string, head: HTMLElement, cell: HTMLElement) {
  if (isSlot(id)) return slotHead(id, head);
  if (!head.dataset.drag) {
    // Drag a pane by its header to move it within the layout.
    head.dataset.drag = "1";
    // The buttons show on hover too. A class, not :hover: the head rebuilds its children
    // as an agent's title changes, and WebKit drops :hover until the pointer moves.
    head.addEventListener("mouseenter", () => head.classList.add("hover"));
    head.addEventListener("mouseleave", () => head.classList.remove("hover"));
    head.addEventListener("mousedown", (e) => {
      if (!(e.target as Element).closest("button")) dragSessions(e, [id]);
    });
    // A double-click on the header zooms the pane: maximize in a split, focus mode alone.
    head.addEventListener("dblclick", (e) => {
      if ((e.target as Element).closest("button")) return;
      if (id !== S.focused) focusPane(id);
      if (shownIds().length > 1) toggleMaximize();
      else toggleFocusMode();
    });
  }
  const s = sessions.get(id);
  const at = s ? place(s) : null;
  cell.style.setProperty("--pc", accent(at?.project));
  cell.classList.toggle("focused", id === S.focused);
  // A session picked in the list shows in its pane too, so a picked pair reads as one.
  cell.classList.toggle("selected", S.selection.includes(id));
  const multi = shownIds().length > 1;
  const grouped = inShownGroup(id);
  const away = s ? awayPlaces(s).map((a) => branchName(a.worktree)).join(", ") : "";
  // Rebuild only on change, so a click that spans a daemon event still lands.
  const sig = s ? JSON.stringify([s.state, s.exit_code, s.agent_exit, s.unread, S.maximized === id, S.focusMode, at?.project.name, taskTitle(s), agentKind(s), multi, grouped, away, resumeOffer(s), newestImage(), insertShown(id), faceSig(s.coordinator ? null : (s.skipper ?? null)), statusSig(s)]) : "";
  if (head.dataset.sig === sig) return;
  head.dataset.sig = sig;
  head.replaceChildren();
  if (!s) return;
  head.classList.toggle("st-waiting", s.state === "waiting");
  head.classList.toggle("st-done", s.state === "done");
  // The state reads with the name it belongs to, not with the buttons.
  head.append(agentIcon(s), headTitle(s), stateIcon(s));
  // What the program reports: what it waits for and asks, how its turn ended.
  const note = statusNote(s);
  if (note) head.appendChild(h("span", "where status-note", note));
  // The program's own progress, along the header's lower edge.
  const progress = progressOf(s);
  if (progress !== null) {
    const bar = h("span", "rp-bar head-progress");
    bar.setAttribute("role", "progressbar");
    bar.setAttribute("aria-valuenow", String(progress));
    bar.setAttribute("aria-valuemin", "0");
    bar.setAttribute("aria-valuemax", "100");
    const fill = h("i");
    fill.style.width = `${progress}%`;
    bar.append(fill);
    head.appendChild(bar);
  }
  if (away) head.appendChild(h("span", "where mono", t("pane.workingIn", { from: at ? branchName(at.worktree) : t("sidebar.otherWhere"), away })));
  // After a restart: the agent this pane ran, and a button to get back to it.
  const offer = resumeOffer(s);
  if (offer) head.appendChild(h("span", "where", offer.title ? t("pane.wasTitle", { agent: offer.agent, title: offer.title }) : t("pane.was", { agent: offer.agent })));
  head.appendChild(h("span", "head-gap"));
  if (offer) {
    const r = button("head-resume", t("pane.resume"), () => {
      // Measure first: focusing redraws the head, and a replaced button measures as 0,0.
      const at = r.getBoundingClientRect();
      if (id !== S.focused) focusPane(id);
      resumePane(id, { x: at.left, y: at.bottom, right: at.right });
    });
    r.title = t("pane.resumeTip", { agent: offer.agent });
    head.appendChild(r);
  }
  const images = imagesButton(id);
  if (images) head.appendChild(images);
  // Mouse controls for the two view modes. The pane is focused first: they act on the focused pane.
  const viewBtn = (name: RuneName, label: string, key: Action, run: () => void) => {
    const b = button("head-btn", "", () => {
      if (id !== S.focused) focusPane(id);
      run();
    });
    b.title = `${label} (${keyLabel(key)})`;
    b.setAttribute("aria-label", label);
    b.appendChild(rune(name, 12));
    head.appendChild(b);
  };
  if (multi) {
    const max = S.maximized === id;
    viewBtn(max ? "view-restore" : "view-maximize", max ? t("cmd.restore") : t("cmd.maximize"), "maximize", toggleMaximize);
  }
  viewBtn("view-focus", S.focusMode ? t("cmd.leaveFocusMode") : t("cmd.focusMode"), "focus-mode", toggleFocusMode);
  // Only a pane of a group has a × : it takes the session out and keeps it selected.
  if (grouped) {
    const x = button("head-btn", "", () => closePane(id));
    const label = t("pane.removeFromGroup");
    x.title = `${label} (${keyLabel("close-pane")})`;
    x.setAttribute("aria-label", label);
    x.appendChild(icon('<path d="M6 6l12 12M18 6L6 18"></path>'));
    head.appendChild(x);
  }
}

/** The title in the head. A project Skipper reads "Skipper · gesher", with
 *  its face before the project; the Skiff agent's face comes first. */
function headTitle(s: SessionInfo): HTMLElement {
  const title = h("span", "title", taskTitle(s));
  if (s.coordinator) {
    title.prepend(skipperFace(null, 16));
    return title;
  }
  if (!isProjectSkipper(s)) return title;
  title.replaceChildren(`${skipperBase(s)} · `, skipperFace(s.skipper ?? "", 16), scopeName(s.skipper ?? ""));
  return title;
}
