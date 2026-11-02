import { dialogFrame } from "../../ui/dialogParts";
import { button, h } from "../../ui/dom";
import { t } from "../../i18n";

// ── Choice dialog ──────────────────────────────────────────────────────────

export function trapTab(panel: HTMLElement, e: KeyboardEvent) {
  const stops = [...panel.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled)")];
  const i = stops.indexOf(document.activeElement as HTMLElement);
  stops[(i + (e.shiftKey ? -1 : 1) + stops.length) % stops.length]?.focus();
}

/**
 * A modal with Cancel and the given choices. Cancel has the keys, and Enter
 * on anything but a choice is Cancel. Esc or a click outside is Cancel.
 */
export function choose(o: { title: string; body: string; choices: { id: string; label: string; tone: "go" | "danger" | "plain" }[] }): Promise<string> {
  return new Promise((resolve) => {
    const { overlay, panel } = dialogFrame("confirm-panel", "alertdialog", o.title);
    const title = h("div", "confirm-title", o.title);
    const body = h("div", "confirm-body", o.body);
    title.id = "ws-choose-title";
    body.id = "ws-choose-body";
    panel.setAttribute("aria-labelledby", title.id);
    panel.setAttribute("aria-describedby", body.id);
    panel.removeAttribute("aria-label");
    const foot = h("div", "confirm-foot");
    const done = (id: string) => {
      overlay.remove();
      resolve(id);
    };
    const cancel = button("confirm-cancel", t("workspace.cancel"), () => done("cancel"));
    foot.append(cancel);
    for (const c of o.choices) {
      const cls = c.tone === "go" ? "confirm-act go" : c.tone === "danger" ? "confirm-act" : "confirm-cancel";
      foot.append(button(cls, c.label, () => done(c.id)));
    }
    panel.append(title, body, foot);
    overlay.append(panel);
    document.body.append(overlay);
    overlay.addEventListener("mousedown", (e) => e.target === overlay && done("cancel"));
    overlay.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") done("cancel");
      else if (e.key === "Enter") {
        // Enter presses the button with the keys; anywhere else it cancels.
        const b = document.activeElement;
        if (b instanceof HTMLButtonElement && panel.contains(b)) b.click();
        else done("cancel");
      } else if (e.key === "Tab") trapTab(panel, e);
      else return;
      e.preventDefault();
    });
    cancel.focus();
  });
}
