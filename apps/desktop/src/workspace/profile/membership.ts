/** Project and fleet membership of the open workspace, and the sidebar and tree menu entries that change it. */
import { S } from "../../app/state";
import type { Project } from "../../platform/types";
import { ctxMenu } from "../../ui/contextMenu";
import type { MenuEntry } from "../../ui/menu";
import { t, upper } from "../../i18n";
import type { createWorkspaceState } from "./state";

const trimPath = (p: string) => (p.length > 1 ? p.replace(/\/+$/, "") : p);

export function createMembership(state: ReturnType<typeof createWorkspaceState>, dependencies: {
  announce: (text: string) => void;
  changed: () => void;
  showView: (focus?: boolean) => void;
}) {
  // ── Add and remove ─────────────────────────────────────────────────────────

  const fleetNames = () => [...new Set(S.projects.map((p) => p.fleet).filter((f): f is string => !!f))];
  const listed = (p: Project) => !!state.doc && state.doc.profile.projects.some((x) => trimPath(x) === trimPath(p.path));

  /** Puts a project or fleet in the open workspace, or takes it out. An edit, like a change in the Edit dialog. */
  function setIncluded(kind: "project" | "fleet", ref: string, on: boolean) {
    if (!state.doc || state.saving) return;
    const d = state.doc;
    const shownBefore = new Set(S.projects.filter(state.shows).map((p) => trimPath(p.path)));
    if (kind === "project") {
      const rest = d.profile.projects.filter((x) => trimPath(x) !== trimPath(ref));
      d.profile.projects = on ? [...rest, trimPath(ref)] : rest;
    } else {
      const rest = d.profile.fleets.filter((x) => x !== ref);
      d.profile.fleets = on ? [...rest, ref] : rest;
    }
    // A project that left the view takes its fold and pick with it.
    const left = (x: string) => shownBefore.has(trimPath(x)) && !S.projects.some((p) => trimPath(p.path) === trimPath(x) && state.shows(p));
    d.profile.folded_projects = d.profile.folded_projects.filter((x) => !left(x));
    if (d.profile.selected && left(d.profile.selected)) d.profile.selected = null;
    if (kind === "fleet" && !on) d.profile.folded_fleets = d.profile.folded_fleets.filter((x) => x !== ref);
    const workspace = state.workspaceName();
    if (kind === "fleet") dependencies.announce(t(on ? "workspace.fleetAdded" : "workspace.fleetRemoved", { fleet: ref, workspace }));
    else {
      const project = S.projects.find((p) => trimPath(p.path) === trimPath(ref))?.name ?? ref;
      dependencies.announce(t(on ? "workspace.projectAdded" : "workspace.projectRemoved", { project, workspace }));
    }
    dependencies.changed();
    if (!on) dependencies.showView(false);
  }

  /** A project Skiff just added: in a workspace, it goes in too, so it shows where the person added it. */
  function includeAdded(p: Project) {
    if (state.doc && !state.shows(p)) setIncluded("project", p.path, true);
  }

  /** A project that goes to a fleet the workspace does not name, or to none, stays in by its folder. Call it before the change. */
  function keepInView(p: Project, fleet: string | null) {
    if (state.doc && state.shows(p) && !listed(p) && !(fleet && state.doc.profile.fleets.includes(fleet))) setIncluded("project", p.path, true);
  }

  /** A renamed fleet stays in the workspace, folded or open as it was. */
  function renameWorkspaceFleet(from: string, to: string) {
    if (!state.doc || state.saving || from === to) return;
    const d = state.doc.profile;
    if (!d.fleets.includes(from) && !d.folded_fleets.includes(from)) return;
    const swap = (xs: string[]) => [...new Set(xs.map((x) => (x === from ? to : x)))];
    d.fleets = swap(d.fleets);
    d.folded_fleets = swap(d.folded_fleets);
    dependencies.changed();
  }

  /** The + over Projects: in a workspace, the projects and fleets it does not show yet, then Add project. */
  function projectsPlus(at: HTMLElement, addProject: () => void) {
    if (!state.doc) return addProject();
    const r = at.getBoundingClientRect();
    const fleets = fleetNames().filter((f) => !state.doc!.profile.fleets.includes(f));
    const hidden = S.projects.filter((p) => !state.shows(p));
    const entries: MenuEntry[] = [];
    if (fleets.length || hidden.length) entries.push({ head: upper(t("workspace.headAdd")) });
    for (const f of fleets) entries.push({ icon: "code-group", label: t("menu.fleet", { name: f }), run: () => setIncluded("fleet", f, true) });
    for (const p of hidden) entries.push({ label: p.fleet ? t("workspace.projectInFleet", { name: p.name, fleet: p.fleet }) : p.name, run: () => setIncluded("project", p.path, true) });
    if (entries.length) entries.push({ head: upper(t("workspace.headSkiff")) });
    entries.push({ icon: "indicators-plus", label: t("cmd.addProject"), run: addProject });
    ctxMenu.open(r.left, r.bottom, state.workspaceName(), entries);
  }

  /** The workspace items in a project's menu. None in All projects. */
  function projectWorkspaceEntries(p: Project): MenuEntry[] {
    if (!state.doc) return [];
    if (listed(p)) return [{ icon: "code-ungroup", label: t("workspace.removeProject"), run: () => setIncluded("project", p.path, false) }];
    // In by its fleet only: the fleet's menu takes it out.
    return [{ icon: "code-ungroup", label: t("workspace.inByFleet", { fleet: p.fleet ?? "" }), disabled: true }];
  }

  /** The workspace items in a fleet's menu. None in All projects. */
  function fleetWorkspaceEntries(name: string): MenuEntry[] {
    if (!state.doc) return [];
    if (state.doc.profile.fleets.includes(name)) return [{ icon: "code-ungroup", label: t("workspace.removeFleet"), run: () => setIncluded("fleet", name, false) }];
    return [{ icon: "indicators-plus", label: t("workspace.addFleet"), run: () => setIncluded("fleet", name, true) }];
  }

  return { includeAdded, keepInView, renameWorkspaceFleet, projectsPlus, projectWorkspaceEntries, fleetWorkspaceEntries };
}
