/** What the page says about an app update. Kept apart from Tauri for the page tests. */
import { t } from "../i18n";

const RELEASES = "https://github.com/solutionscay/skiff/releases";

/** The release that offers the update, for an install that cannot run here. */
export function releasePage(version: string): string {
  return `${RELEASES}/tag/v${version.replace(/^v/, "")}`;
}

/** One error line for a step that failed: what failed, then the updater's reason. */
export function failLine(step: "check" | "download" | "install", e: unknown): string {
  const reason = String(e instanceof Error ? e.message : e).trim().replace(/\.$/, "");
  const what = t(({ check: "update.failCheck", download: "update.failDownload", install: "update.failInstall" } as const)[step]);
  return reason ? t("update.failReason", { what, reason }) : t("update.failNoReason", { what });
}

/** Up to three short lines of release notes. A bare link is no notes: the panel links the release page. */
export function notesLines(body: string | undefined): string[] {
  const text = (body ?? "").trim();
  if (!text || /^https?:\/\/\S+$/.test(text)) return [];
  return text
    .split("\n")
    .map((l) => l.replace(/^\s*(?:[-*•]|#+)\s*/, "").trim())
    .filter((l) => l && !/^https?:\/\//.test(l))
    .slice(0, 3);
}

/** A download's progress: "43%" and "18.2 / 42.6 MB". With no size known, only what came. */
export function progressText(got: number, total: number | undefined): { pct: string; size: string; frac?: number } {
  const mb = (n: number) => (n / 1_048_576).toFixed(1);
  if (!total) return { pct: `${mb(got)} MB`, size: `${mb(got)} MB` };
  const frac = Math.min(1, got / total);
  return { pct: `${Math.floor(frac * 100)}%`, size: `${mb(got)} / ${mb(total)} MB`, frac };
}
