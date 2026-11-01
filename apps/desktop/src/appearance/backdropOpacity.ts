/** The dialog for a project's pane opacity over its background image. The panes show each value as the slider moves. */
import { hostInvoke } from "../app/hostCaps";
import { h } from "../ui/dom";
import { actionDialog } from "../ui/dialogParts";
import { S } from "../app/state";
import type { Project } from "../platform/types";
import { OPACITY_DEFAULT, OPACITY_MIN, previewOpacity } from "./backdrop";
import { t } from "../i18n";

export function pickBackgroundOpacity(p: Project) {
  const slider = h("input", "opacity-slider");
  slider.type = "range";
  slider.min = String(OPACITY_MIN);
  slider.max = "100";
  slider.value = String(p.background_opacity ?? OPACITY_DEFAULT);
  slider.setAttribute("aria-label", t("opacity.slider"));
  const val = h("span", "opacity-value", `${slider.value}%`);
  const row = h("div", "opacity-row");
  row.append(slider, val);
  slider.addEventListener("input", () => {
    val.textContent = `${slider.value}%`;
    previewOpacity(Number(slider.value));
  });
  previewOpacity(Number(slider.value));
  const back = document.activeElement as HTMLElement | null;
  actionDialog({
    title: t("opacity.title", { name: p.name }),
    body: t("opacity.body"),
    content: [row],
    action: t("opacity.action"),
    tone: "go",
    focus: slider,
    onClose: () => {
      previewOpacity(null);
      back?.focus?.();
    },
    submit: async () => {
      const percent = Number(slider.value);
      await hostInvoke("project_config", "set_project_background_opacity", { project: p.name, percent });
      // Hold the value until the project list comes back with it.
      const mine = S.projects.find((x) => x.name === p.name);
      if (mine) mine.background_opacity = percent;
    },
  });
}
