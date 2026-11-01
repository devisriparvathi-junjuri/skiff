/** Fleets: the order of projects, a drop that moves them, and naming a new fleet. */
import { hostCan, hostInvoke } from "../app/hostCaps";

import type { Project } from "../platform/types";

import { h } from "../ui/dom";
import { showError } from "../ui/alerts";

import { render } from "../app/renderRequest";

import { S } from "../app/state";
import { renameHidden } from "./tree";
import { keepInView, renameWorkspaceFleet } from "./profile";
import { fleetSkipperOf, joinFleet, projectSkipperOf, renameFleetOn } from "../app/skiffAgent";
import { t } from "../i18n";

export interface Fleet {
  name: string;
  projects: Project[];
}

/** Fleets and lone projects in saved order. A fleet stands where its first project is. */
function entries(): (Project | Fleet)[] {
  const out: (Project | Fleet)[] = [];
  const fleets = new Map<string, Fleet>();
  for (const p of S.projects) {
    if (!p.fleet) {
      out.push(p);
      continue;
    }
    let f = fleets.get(p.fleet);
    if (!f) {
      f = { name: p.fleet, projects: [] };
      fleets.set(p.fleet, f);
      out.push(f);
    }
    f.projects.push(p);
  }
  return out;
}

const isFleet = (e: Project | Fleet): e is Fleet => "projects" in e;

/** Every project in saved order, each fleet's projects together where its first one is. */
export function projectOrder(): Project[] {
  return entries().flatMap((e) => (isFleet(e) ? e.projects : [e]));
}

/** What a drop does: where the moved projects land, and the fleet each project ends in. */
export interface Drop {
  /** The project the moved ones land next to. */
  anchor: Project;
  side: "before" | "after";
  /** The moved projects' fleet after the drop. Undefined: a fleet moves whole and keeps its name. */
  fleet?: string | null;
  /** A drop on a lone project: it goes into the new fleet too. */
  make?: Project;
  /** The item that shows the drop, and how. */
  el: HTMLElement;
  mark: "drop-before" | "drop-after" | "drop-into";
  label: string;
}

/** The first "Fleet N" no fleet uses. */
function freeFleetName(): string {
  const used = new Set(S.projects.map((p) => p.fleet));
  let n = 1;
  while (used.has(`Fleet ${n}`)) n++;
  return `Fleet ${n}`;
}

/**
 * The fleets and the project order after the drop, saved in one write. A
 * fleet left with one project goes away, unless its Skipper runs. A project
 * Skipper that comes under a running fleet Skipper is taken over first,
 * after one confirm; Cancel changes nothing.
 */
export function applyDrop(moving: Project[], d: Drop): string | null {
  // The Host keeps the order and the fleets: one that takes no project changes keeps them as they are.
  if (!hostCan("project_config")) return null;
  const joins = d.fleet && !d.make && fleetSkipperOf(d.fleet) ? moving.filter((p) => p.fleet !== d.fleet && projectSkipperOf(p.name)) : [];
  if (!joins.length) return place(moving, d);
  void (async () => {
    try {
      for (const p of joins) if ((await joinFleet(p.name, d.fleet!)) === null) return;
      place(moving, d);
    } catch (e) {
      showError(e);
    }
  })();
  return null;
}

function place(moving: Project[], d: Drop): string | null {
  const order = projectOrder().filter((p) => !moving.includes(p));
  order.splice(order.indexOf(d.anchor) + (d.side === "after" ? 1 : 0), 0, ...moving);
  const made = d.make ? freeFleetName() : null;
  const next = new Map<Project, string | null>(order.map((p) => [p, p.fleet ?? null]));
  if (d.fleet !== undefined) for (const p of moving) next.set(p, made ?? d.fleet);
  if (d.make) next.set(d.make, made);
  const size = new Map<string, number>();
  for (const f of next.values()) if (f) size.set(f, (size.get(f) ?? 0) + 1);
  for (const [p, f] of next) if (f && size.get(f) === 1 && !fleetSkipperOf(f)) next.set(p, null);
  const fleets = [...next].filter(([p, f]) => (p.fleet ?? null) !== f).map(([p, f]) => [p.name, f ?? ""]);
  for (const [p, f] of next) if ((p.fleet ?? null) !== f) keepInView(p, f);
  for (const [p, f] of next) p.fleet = f;
  S.projects = order;
  render();
  hostInvoke("project_config", "reorder_projects", { order: order.map((x) => x.name), fleets }).catch(showError);
  return made;
}

/**
 * A name field under a new fleet's head in the tree. The fleet has its "Fleet N" name
 * already: Enter, Esc or a click away keeps what the field holds.
 */
export function nameFleet(name: string) {
  const head = () => document.querySelector<HTMLElement>(`#sidebar-scroll .fleet-pick[data-key="${CSS.escape(`fleet:${name}`)}"]`);
  const tile = head();
  if (!tile) return;
  const box = h("div", "fleet-name");
  const label = h("label", "fleet-name-label", t("fleet.nameLabel"));
  const input = h("input");
  input.type = "text";
  input.value = name;
  input.spellcheck = false;
  input.id = label.htmlFor = "fleet-name-input";
  const hint = h("div", "fleet-name-hint", t("fleet.nameHint"));
  box.append(label, input, hint);
  const r = tile.getBoundingClientRect();
  box.style.left = `${r.left + 16}px`;
  box.style.top = `${r.bottom}px`;
  document.body.appendChild(box);
  input.focus();
  input.select();
  let done = false;
  const close = (keep: boolean, blur = false) => {
    const next = input.value.trim();
    const taken = next !== name && S.projects.some((p) => p.fleet === next);
    if (keep && taken && !blur) {
      hint.textContent = t("fleet.nameTaken", { name: next });
      return;
    }
    done = true;
    box.remove();
    head()?.focus();
    if (!keep || taken || !next || next === name) return;
    const members = S.projects.filter((p) => p.fleet === name);
    for (const p of members) p.fleet = next;
    renameHidden(name, next);
    renameWorkspaceFleet(name, next);
    render();
    renameFleetOn(name, next, members.map((p) => p.name)).catch(showError);
  };
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Enter") close(true);
    else if (e.key === "Escape") close(false);
  });
  input.addEventListener("blur", () => {
    if (!done) close(true, true);
  });
}
