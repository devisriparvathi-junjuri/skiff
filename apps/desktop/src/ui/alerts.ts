import { $, button, h } from "./dom";
import { t } from "../i18n";

/**
 * A bar above the workspace that stays until dismissed. The status bar was
 * not enough: the next daemon status update wrote over the message.
 */
export function showError(e: unknown, action?: { label: string; run: () => Promise<void> }) {
  console.error(e);
  showBar(e instanceof Error ? e.message : String(e), "alert", action);
}

/** The same bar for a result the user asked for. */
export function showNotice(text: string) {
  showBar(text, "status");
}

function showBar(text: string, role: string, action?: { label: string; run: () => Promise<void> }) {
  document.getElementById("error-alert")?.remove();
  const alert = h("div", "");
  alert.id = "error-alert";
  alert.setAttribute("role", role);
  if (role === "status") alert.className = "notice";
  alert.append(h("span", "alert-text", text));
  if (action) {
    const run = button("alert-btn", action.label, async () => {
      run.disabled = true;
      try {
        await action.run();
        alert.remove();
      } catch (e) {
        showError(e, action);
      }
    });
    alert.append(run);
  }
  alert.append(button("alert-btn", t("alert.dismiss"), () => alert.remove()));
  $("body").before(alert);
}
