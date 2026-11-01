/**
 * A Skipper's face: the generated avatar chosen in its own folder
 * (`avatar.toml` in the Skiff agent's folder, `fleets/<fleet>/` or
 * `projects/<project>/`). Skiff writes a random one there the first time it
 * reads a Skipper's folder, so each face stays. Until then, and on a remote
 * Host, the face is generated from the name, and the Admiral's is the
 * bicorne. The face sets --pc to the signature color of the theme its Skipper's pane
 * wears, so the body follows a theme change.
 *
 * The choices are this Mac's, as project icons are: a remote Host shows the
 * generated faces.
 */
import { invoke } from "@tauri-apps/api/core";
import { admiralAvatar, skipperAvatar } from "../appearance/skipperAvatar";
import { place } from "../app/stateQueries";
import { S } from "../app/state";
import { projectSkipperOf, skiffAgentSession } from "../app/skiffAgent";
import { appThemeOf, signatureColor, themeIdFor } from "../appearance/themes";
import { DEFAULT_THEME } from "../appearance/colors";
import { render } from "../app/renderRequest";
import { dialogFrame, dialogHeader } from "../ui/dialogParts";
import { button, h } from "../ui/dom";
import { t } from "../i18n";

let chosen = new Map<string, number>();

/** Scopes already read, so a face with no choice asks once. */
const asked = new Set<string>();
let reading = false;

/** Every Skipper with a folder on this Mac: "" for the Admiral, then fleets and projects. */
export const avatarScopes = () => [...chosen.keys()];

/** Reads the choices again, then draws. Called with every projects read. */
export async function loadSkipperAvatars() {
  const list = await invoke<{ scope: string; seed: number }[]>("skipper_avatars").catch(() => []);
  const next = new Map(list.map((a) => [a.scope, a.seed]));
  if (JSON.stringify([...next]) === JSON.stringify([...chosen])) return;
  chosen = next;
  render("skipper-avatars");
}

const projectOf = (scope: string) => S.projects.find((p) => p.name === scope) ?? null;

/**
 * The face's color: the signature color (its main color, else its
 * foreground) of the theme the Skipper's pane wears, as a themed session's
 * agent icon shows it. Not the theme's --accent: a theme with no `ui`
 * block gives its magenta there, the same purple for nearly every theme. Its
 * session theme, else its project's, else the app's, as `themeIdFor` gives
 * the pane. With no session, or a session in no project, the scope's
 * project theme, else the app theme. The Admiral follows its own the same way.
 * Each face resolves its own Skipper and sets --pc on itself, so it never
 * takes the focused pane's theme or a color an ancestor sets.
 */
export function faceColor(scope: string | null): string {
  const s = scope === null ? skiffAgentSession() : projectSkipperOf(scope);
  const t = s && (s.theme || place(s)) ? S.themes.find((x) => x.id === themeIdFor(s)) : appThemeOf(scope === null ? null : projectOf(scope));
  const own = t ?? S.themes.find((x) => x.id === DEFAULT_THEME);
  // A literal, never var(--accent): an ancestor (a Mission control lane, the app in a pane's theme) may set that.
  return own ? signatureColor(own) : STYLESHEET_ACCENT;
}

/** The stylesheet's accent, before any theme loads. */
const STYLESHEET_ACCENT = "#b69cff";

/** What a face shows, for the row signatures. */
export const faceSig = (scope: string | null) => [chosen.get(scope ?? "") ?? null, faceColor(scope)];

/** `scope`: a project's name, `fleet:<name>`, or null for the Admiral. `px` at zoom 1. `seed`: a face to show in place of the chosen one. */
export function skipperFace(scope: string | null, px: number, seed?: number): HTMLElement {
  const face = h("span", "skipper-face");
  face.style.setProperty("--face", String(px));
  face.style.setProperty("--pc", faceColor(scope));
  const key = scope ?? "";
  const n = seed ?? chosen.get(key);
  // A new Skipper's folder gets its random face on the next read.
  if (seed === undefined && n === undefined && !asked.has(key)) {
    asked.add(key);
    if (!reading) {
      reading = true;
      queueMicrotask(() => void loadSkipperAvatars().finally(() => (reading = false)));
    }
  }
  face.innerHTML = scope === null ? admiralAvatar() : skipperAvatar(scope, n ?? 0);
  return face;
}

/**
 * Avatar…: twelve generated faces. A click picks one and closes; Shuffle
 * draws twelve more. Esc or a click outside closes with no change. The keys
 * go back to what held them.
 */
export function setAvatarDialog(scope: string | null, title: string, done: () => void) {
  const back = document.activeElement as HTMLElement | null;
  const { overlay, panel } = dialogFrame("confirm-panel av-panel", "dialog", title);
  const close = () => {
    overlay.remove();
    back?.focus?.();
  };
  const now = chosen.get(scope ?? "") ?? 1;
  let base = Math.floor((now - 1) / 12) * 12;
  const grid = h("div", "av-grid");
  const error = h("div", "prompt-error");
  error.setAttribute("role", "alert");
  const draw = () => {
    grid.replaceChildren(
      ...Array.from({ length: 12 }, (_, i) => {
        const seed = base + i + 1;
        const b = button("av-opt", "", async () => {
          try {
            await invoke("set_skipper_avatar", { scope: scope ?? "", seed });
            await loadSkipperAvatars();
            close();
            done();
          } catch (e) {
            error.textContent = String(e);
          }
        });
        b.setAttribute("aria-label", t("avatar.seed", { seed }));
        b.setAttribute("aria-pressed", String(seed === now));
        b.appendChild(skipperFace(scope, 32, seed));
        return b;
      }),
    );
  };
  const shuffle = button("av-act", t("avatar.shuffle"), () => {
    base += 12;
    draw();
    grid.querySelector<HTMLButtonElement>(".av-opt")?.focus();
  });
  panel.append(dialogHeader(title, close), grid, shuffle, error);
  overlay.appendChild(panel);
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });
  overlay.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
  });
  document.body.appendChild(overlay);
  draw();
  (grid.querySelector<HTMLButtonElement>('[aria-pressed="true"]') ?? grid.querySelector<HTMLButtonElement>(".av-opt"))?.focus();
}
