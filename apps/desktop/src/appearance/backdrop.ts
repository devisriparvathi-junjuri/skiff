/** The project's background image: one picture behind every pane of the terminal area. */
import { hostInvoke } from "../app/hostCaps";
import { dataEpoch } from "../app/dataPlane";
import { host } from "../terminal/terminalHost";
import { S, sessions } from "../app/state";
import { place } from "../app/stateQueries";
import type { Project } from "../platform/types";

/** How opaque the panes are over a background image, in percent, when the project sets none. */
export const OPACITY_DEFAULT = 85;
export const OPACITY_MIN = 40;

/** A value the opacity dialog shows while the slider moves. Null: the project's own. */
let trial: number | null = null;

export function previewOpacity(percent: number | null) {
  trial = percent;
  apply();
}

/**
 * Blob URLs by project and the Host's background path. The bytes come from
 * the selected Host (`project_assets@1`), checked there as an image: This Mac
 * and a remote Host alike, never a path read here. A key that failed to load
 * waits before it is tried again, so a missing file is not asked on every render.
 * Each URL belongs to the data connection it was read on: a new one reads
 * them again ([`reloadBackdrops`]), and a reply from an older one is dropped.
 */
const urls = new Map<string, string>();
/** The shown key's URL from the last data connection, kept on screen until the new one answers. */
const stale = new Map<string, string>();
const loading = new Set<string>();
const failedAt = new Map<string, number>();
const RETRY_MS = 30_000;
let shown: string | null = null;

function dropStale(key: string) {
  const url = stale.get(key);
  if (url) URL.revokeObjectURL(url);
  stale.delete(key);
}

async function load(key: string, project: string) {
  if (urls.has(key) || loading.has(key) || Date.now() - (failedAt.get(key) ?? -Infinity) < RETRY_MS) return;
  loading.add(key);
  const at = dataEpoch();
  try {
    const bytes = await hostInvoke<ArrayBuffer>("project_assets", "project_background", { project });
    if (at === dataEpoch()) {
      urls.set(key, URL.createObjectURL(new Blob([bytes])));
      failedAt.delete(key);
      dropStale(key);
    }
  } catch (e) {
    if (at === dataEpoch()) {
      failedAt.set(key, Date.now());
      dropStale(key);
    }
    console.error(e);
  } finally {
    loading.delete(key);
  }
  // Asked on a data connection that is gone since: ask the new one.
  if (at !== dataEpoch()) applyBackdrop();
  else if (key === wanted()) apply();
}

/** The data plane is back, or another: every image is read again from it. The shown one stays until its new bytes come. */
export function reloadBackdrops() {
  const keep = wanted();
  for (const [key, url] of urls) {
    if (key === keep) {
      dropStale(key);
      stale.set(key, url);
    } else URL.revokeObjectURL(url);
  }
  for (const key of [...stale.keys()]) if (key !== keep) dropStale(key);
  urls.clear();
  failedAt.clear();
  applyBackdrop();
}

/** The focused session's project, else the selected one. */
function shownProject(): Project | undefined {
  const s = S.focused ? sessions.get(S.focused) : undefined;
  return (s && place(s)?.project) ?? S.projects.find((x) => x.name === S.selectedProject);
}

/** The shown project's background, as a key: its name and the Host's path for it. */
function wanted(): string | null {
  const p = shownProject();
  return p?.background ? `${p.name}\n${p.background}` : null;
}

function apply() {
  const key = wanted();
  const url = key ? urls.get(key) ?? stale.get(key) ?? null : null;
  const percent = trial ?? shownProject()?.background_opacity ?? OPACITY_DEFAULT;
  host.classList.toggle("has-bg", !!url);
  host.style.setProperty("--pane-alpha", url ? String(percent / 100) : "1");
  if (url !== shown) {
    shown = url;
    host.style.setProperty("--bg-image", url ? `url("${url}")` : "none");
  }
}

/** Call on every render. Cheap when nothing changed. */
export function applyBackdrop() {
  const key = wanted();
  const p = shownProject();
  if (key && p && !urls.has(key)) void load(key, p.name);
  apply();
}
