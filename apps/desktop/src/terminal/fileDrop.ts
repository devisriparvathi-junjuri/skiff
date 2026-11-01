/**
 * Files dragged from another app onto a pane. While they are over a pane,
 * its body shows the drop look and one label. On drop, the backend copies
 * each image to the cache and Skiff types the paths as a paste. The drop
 * focuses the pane; the current row stays. Anywhere else, a drop does nothing.
 * A thumbnail dragged from Images shows the same look, with the thumbnail.
 */
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";

import { isRemote, hostName } from "../app/hosts";
import { S, sessions } from "../app/state";
import { showError } from "../ui/alerts";
import { modalOpen } from "../ui/dom";
import { isSlot } from "../workspace/layoutSlots";
import { focusPane } from "./paneActions";
import { previewOnScreen } from "./peek";
import { type Typed, typeFiles, whoOf } from "./terminalClipboard";
import { panes } from "./terminalState";
import { view } from "./terminal";
import { t, tn } from "../i18n";

const overlay = document.createElement("div");
overlay.className = "drop-preview file-drop";
overlay.hidden = true;
const label = document.createElement("span");
label.className = "drag-ghost";
const mini = document.createElement("span");
mini.className = "head-mini";
const words = document.createElement("span");
label.append(mini, words);
overlay.appendChild(label);
document.body.appendChild(overlay);

/** Paths of the drag in progress. `over` events carry only the position. */
let dragged: string[] = [];

/** The live session pane under a window position, in CSS pixels. Images
 *  stays open while one of its thumbnails is dragged. */
function paneAt(x: number, y: number, fromImages = false): string | null {
  if ((!fromImages && modalOpen()) || previewOnScreen()) return null;
  for (const [id, r] of view.cellRects()) {
    if (x < r.left || x >= r.right || y < r.top || y >= r.bottom) continue;
    const s = sessions.get(id);
    if (isSlot(id) || !s || s.state === "done" || !panes.has(id)) return null;
    return id;
  }
  return null;
}

/** `thumb`: the dragged library image's thumbnail, `null` for a file drag.
 *  A file drag types the paths; a library image runs Insert, so it says so. */
function show(id: string | null, n = dragged.length, thumb: string | null = null) {
  const body = id ? view.body(id) : null;
  if (!body || !n) {
    overlay.hidden = true;
    return;
  }
  const r = body.getBoundingClientRect();
  Object.assign(overlay.style, { left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px`, height: `${r.height}px` });
  words.textContent = thumb !== null ? t("drop.insert") : tn("drop.type", n);
  mini.hidden = thumb === null;
  if (thumb !== null && mini.dataset.src !== thumb) {
    mini.dataset.src = thumb;
    mini.replaceChildren(...(thumb ? [Object.assign(document.createElement("img"), { src: thumb, alt: "" })] : []));
  }
  overlay.hidden = false;
}

/** A library thumbnail over the window: the drop look on the pane under it. */
export function showImageDrag(x: number, y: number, thumb: string | null) {
  show(paneAt(x, y, true), 1, thumb ?? "");
}

/** The drag ends: the pane under the pointer, if any. */
export function endImageDrag(x: number, y: number): string | null {
  show(null);
  return paneAt(x, y, true);
}

async function drop(id: string, paths: string[]) {
  if (id !== S.focused) focusPane(id);
  try {
    const typed = await invoke<Typed>("drop_files", { who: whoOf(id), paths, host: isRemote() ? hostName() : null });
    typeFiles(id, typed);
  } catch (e) {
    showError(e);
  }
}

void getCurrentWebview().onDragDropEvent(({ payload: p }) => {
  if (p.type === "leave") {
    dragged = [];
    return show(null);
  }
  if (p.type === "enter") dragged = p.paths;
  const ratio = window.devicePixelRatio || 1;
  const id = paneAt(p.position.x / ratio, p.position.y / ratio);
  if (p.type !== "drop") return show(id);
  const paths = p.paths.length ? p.paths : dragged;
  dragged = [];
  show(null);
  if (id && paths.length) void drop(id, paths);
});
