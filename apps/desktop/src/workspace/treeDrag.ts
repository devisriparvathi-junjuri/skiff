/**
 * Drag a project row or a fleet head in the tree (#87). A press that does not
 * move stays a click. Over a project, the top and bottom quarters drop the
 * drag next to it; the middle half makes a fleet with a lone project, or joins
 * the project's fleet. A fleet head takes a project at the top of its fleet.
 * The Projects head takes it out of its fleet. A fleet moves whole, before or
 * after another fleet. A label at the pointer says what the drop does. Esc cancels.
 */
import type { Project } from "../platform/types";

import { applyDrop, nameFleet, type Drop } from "./fleets";
import { treeDrag } from "./tree";
import { S } from "../app/state";
import { render } from "../app/render";
import { h } from "../ui/dom";
import { t } from "../i18n";

const MARKS = ["drop-before", "drop-after", "drop-into"];

const fleetOf = (name: string) => S.projects.filter((p) => p.fleet === name);

export function dragTree(down: MouseEvent, source: HTMLElement, moving: Project[], fleet: string | null) {
  if (down.button !== 0 || S.projects.length < 2) return;
  if ((down.target as Element).closest(".wt-plus, .tree-head-add")) return;
  const sy = down.clientY;
  const p = fleet ? null : moving[0];
  let drop: Drop | null = null;
  let ghost: HTMLElement | null = null;
  let active = false;

  const into = (name: string | null) => (name ? t("fleet.dropAdd", { name }) : p?.fleet ? t("fleet.dropOut", { name: p.fleet }) : "");
  const headOf = (name: string) => document.querySelector<HTMLElement>(`#sidebar-scroll .fleet-pick[data-key="${CSS.escape(`fleet:${name}`)}"]`)?.parentElement ?? null;
  const kidsOf = (name: string) => document.querySelector<HTMLElement>(`#sidebar-scroll [data-kids="${CSS.escape(`fleet:${name}`)}"]`);

  function dropAt(x: number, y: number): Drop | null {
    const el = document.elementFromPoint(x, y)?.closest<HTMLElement>("#sidebar-scroll .project-row, #sidebar-scroll > .tree-head");
    if (!el) return null;
    const box = el.getBoundingClientRect();
    const at = (y - box.top) / box.height;
    const q = el.dataset.project ? S.projects.find((x) => x.name === el.dataset.project) : undefined;
    const pick = el.querySelector<HTMLElement>(":scope > .fleet-pick");
    const headFleet = pick ? pick.dataset.key!.slice("fleet:".length) : null;

    if (fleet) {
      // A fleet lands before or after another fleet, whole.
      const name = q ? q.fleet ?? null : headFleet;
      if (!name || name === fleet) return null;
      const other = fleetOf(name);
      const head = headOf(name)!;
      const kids = kidsOf(name);
      const top = head.getBoundingClientRect().top;
      const bottom = (kids ?? head).getBoundingClientRect().bottom;
      if (y < (top + bottom) / 2) return { anchor: other[0], side: "before", el: head, mark: "drop-before", label: "" };
      return { anchor: other[other.length - 1], side: "after", el: kids ?? head, mark: "drop-after", label: "" };
    }
    if (!p) return null;
    if (q) {
      if (q === p) return null;
      const qFleet = q.fleet ?? null;
      const zone = at < 0.25 ? "before" : at > 0.75 ? "after" : "into";
      if (zone === "into") {
        if (qFleet && qFleet === p.fleet) return null;
        if (qFleet) return { anchor: q, side: "after", fleet: qFleet, el, mark: "drop-into", label: into(qFleet) };
        return { anchor: q, side: "after", fleet: null, make: q, el, mark: "drop-into", label: t("fleet.dropMake") };
      }
      const label = qFleet !== (p.fleet ?? null) ? into(qFleet) : "";
      return { anchor: q, side: zone, fleet: qFleet, el, mark: zone === "before" ? "drop-before" : "drop-after", label };
    }
    if (headFleet) {
      // The top of the fleet.
      const first = fleetOf(headFleet)[0];
      if (!first || first === p) return null;
      return { anchor: first, side: "before", fleet: headFleet, el, mark: "drop-into", label: p.fleet === headFleet ? "" : into(headFleet) };
    }
    // The Projects head: the top of the projects in no fleet.
    const first = S.projects.find((x) => !x.fleet) ?? S.projects[0];
    if (first === p) return null;
    return { anchor: first, side: "before", fleet: null, el, mark: "drop-after", label: into(null) };
  }

  function mark() {
    for (const el of document.querySelectorAll("#sidebar-scroll .drop-before, #sidebar-scroll .drop-after, #sidebar-scroll .drop-into")) el.classList.remove(...MARKS);
    if (drop) drop.el.classList.add(drop.mark);
  }

  const place = (m: MouseEvent) => {
    if (!ghost) return;
    ghost.style.left = `${m.clientX + 12}px`;
    ghost.style.top = `${m.clientY - ghost.offsetHeight / 2}px`;
  };

  const move = (m: MouseEvent) => {
    if (!active) {
      if (Math.abs(m.clientY - sy) < 5) return;
      active = treeDrag.active = true;
      source.classList.add("dragging");
      document.body.classList.add("dragging-tree");
      ghost = h("div", "tree-ghost");
      ghost.append(h("span", "tree-ghost-name", fleet ?? p!.name), h("span", "tree-ghost-label"));
      document.body.appendChild(ghost);
    }
    drop = dropAt(m.clientX, m.clientY);
    const label = ghost!.querySelector<HTMLElement>(".tree-ghost-label")!;
    label.textContent = drop?.label ?? "";
    label.hidden = !drop?.label;
    place(m);
    mark();
  };
  const end = () => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
    window.removeEventListener("keydown", esc, true);
    if (!active) return null;
    // The click that follows the release would fold the row.
    const swallow = (c: Event) => c.stopPropagation();
    window.addEventListener("click", swallow, true);
    setTimeout(() => window.removeEventListener("click", swallow, true), 0);
    document.body.classList.remove("dragging-tree");
    source.classList.remove("dragging");
    ghost?.remove();
    const d = drop;
    drop = null;
    mark();
    active = treeDrag.active = false;
    return d;
  };
  const up = () => {
    const d = end();
    if (!d) return render();
    const made = applyDrop(moving, d);
    if (made) nameFleet(made);
  };
  const esc = (k: KeyboardEvent) => {
    if (k.key !== "Escape") return;
    k.preventDefault();
    k.stopPropagation();
    end();
    render();
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
  window.addEventListener("keydown", esc, true);
}
