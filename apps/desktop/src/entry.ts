/**
 * The page's entry. It sets the language before any UI module loads, so
 * words that a module builds when it loads, such as the palette and the
 * Images drop-down, come out in that language. A change to the language
 * shows when the window opens again.
 */
import { invoke } from "@tauri-apps/api/core";
import { setLanguage } from "./i18n";
import type { Appearance } from "./platform/types";

void (async () => {
  const look = await invoke<Appearance>("get_appearance").catch(() => null);
  setLanguage(look?.language ?? navigator.language);
  await import("./main");
})();
