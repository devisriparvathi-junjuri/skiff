import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

test("project digits use Command+Option on Mac and Ctrl+Shift elsewhere, after overrides", () => {
  for (const userAgent of ["Macintosh", "Linux", "Windows"]) {
    const mac = userAgent === "Macintosh";
    const globals = { navigator: { userAgent }, window: new EventTarget() };
    const keys = moduleFromSource("../src/app/keys.ts", {}, globals, "");
    const keyboard = moduleFromSource("../src/app/keyboard.ts", {
      "./keys": keys,
      "../ui/dom": { $: () => new EventTarget() },
    }, globals, "");
    const sheet = moduleFromSource("../src/ui/infoDialogs.ts", {
      "../app/keys": keys,
    }, globals, "\nexports.fixed = FIXED_SHORTCUTS;");
    assert.equal(sheet.fixed.find(([section]: string[]) => section === "section.projects")[2], mac ? "⌘+Option+1…9" : "Ctrl+Shift+1…9");
    for (let digit = 1; digit <= 9; digit++) {
      const event = { code: `Digit${digit}`, key: String(digit), metaKey: mac, ctrlKey: !mac, altKey: mac, shiftKey: !mac };
      assert.equal(keys.actionFor(event), null, "no default action takes the project key");
      assert.equal(keyboard.appKey(event), digit);
      assert.equal(keyboard.appKey({ ...event, altKey: !mac, shiftKey: mac }), null);
      assert.equal(keyboard.appKey({ ...event, ctrlKey: true, metaKey: true }), null);
      keys.setKeymap({ palette: `ctrl+${mac ? "alt" : "shift"}+${digit}` });
      assert.equal(keyboard.appKey(event), "palette");
      keys.setKeymap({});
    }
    assert.equal(keys.keyLabel("settings"), mac ? "⌘+Shift+," : "Ctrl+Shift+,");
  }
});
