/**
 * A Host's file in the peek: the bounded preview its `skiff-host` answers
 * (`files@1`), drawn here. Text is text, never parsed as markup. An image is
 * drawn from the bytes the Host sent, only for a type the Host found in those
 * bytes. A binary file, a link or a file too large says what it is. Nothing
 * runs on this Mac, and the Host's path is never read or opened here.
 */
import { h } from "../ui/dom";
import type { HostFile } from "../platform/types";
import { t } from "../i18n";

/** The Host's limits, for the notes. */
const TEXT_KIB = 512;
const IMAGE_MIB = 5;
/** The image types the Host sends bytes for. */
const IMAGES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/x-icon"]);

/** A size for a note. */
function sizeLabel(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${Math.ceil(bytes / 1024)} KiB`;
}

/** What the viewer says above the content. */
export function fileNotes(f: HostFile): string[] {
  const notes: string[] = [];
  if (f.content === "link") notes.push(t("view.link", { target: f.target ?? "?" }));
  if (f.content === "binary") notes.push(t("view.binaryFile", { size: sizeLabel(f.size) }));
  if (f.content === "text" && !f.text) notes.push(t("view.emptyFile"));
  if (f.content === "text" && f.truncated) notes.push(t("view.fileLong", { kib: TEXT_KIB }));
  if (f.content === "image" && f.truncated) notes.push(t("view.imageLarge", { size: sizeLabel(f.size), mib: IMAGE_MIB }));
  return notes;
}

/** The image's bytes as a blob, if the Host sent an image type this page draws. */
export function imageBlob(f: HostFile): Blob | null {
  if (f.content !== "image" || !f.bytes || !f.mime || !IMAGES.has(f.mime)) return null;
  const raw = atob(f.bytes);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  return new Blob([bytes], { type: f.mime });
}

/** The viewer's body for a file. `url` makes a blob URL; the caller revokes it. */
export function fileBody(f: HostFile, url: (b: Blob) => string): HTMLElement {
  const body = h("div", "diff-view-body");
  body.tabIndex = -1;
  for (const n of fileNotes(f)) body.append(h("div", "diff-view-note", n));
  if (f.content === "text" && f.text) body.append(h("pre", "diff-text mono", f.text));
  const blob = imageBlob(f);
  if (blob) {
    const img = h("img", "file-view-image") as HTMLImageElement;
    img.alt = "";
    img.src = url(blob);
    body.append(img);
  }
  return body;
}
