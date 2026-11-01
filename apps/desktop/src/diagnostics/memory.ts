/** The status bar's memory reading: the app with WebKit, plus skiffd. */
import { invoke } from "@tauri-apps/api/core";
import { $, setAttr } from "../ui/dom";
import { rune } from "../ui/runes";
import { t } from "../i18n";

const mb = (b: number) => (b >= 1 << 30 ? `${(b / (1 << 30)).toFixed(1)} GB` : `${Math.round(b / (1 << 20))} MB`);

async function update() {
  const el = $("memory");
  let m: [number, number] | null;
  try {
    m = await invoke<[number, number] | null>("memory_usage");
  } catch (e) {
    el.textContent = t("memory.unknown");
    el.title = String(e);
    return;
  }
  if (!m) {
    el.textContent = "";
    return;
  }
  const [app, daemon] = m;
  // Every 5 s: the bar is laid out again only when the reading changes.
  if (el.textContent !== mb(app + daemon)) el.replaceChildren(rune("devices-memory", 13), mb(app + daemon));
  setAttr(el, "title", t("memory.title", { app: mb(app), daemon: mb(daemon) }));
}

/** Reads now, then every 5 seconds. */
export function startMemory() {
  void update();
  window.setInterval(update, 5000);
}
