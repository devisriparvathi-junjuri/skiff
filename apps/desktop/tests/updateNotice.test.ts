// What the page says when an app update fails. Run: pnpm test
import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// Loaded through the harness, which supplies the word catalog. `Error` is this realm's, so `instanceof` holds.
const { failLine, releasePage } = moduleFromSource("../src/app/updateNotice.ts", {}, { Error }, "");

test("a failed .deb or .rpm install names the updater's reason in one line", () => {
  assert.equal(failLine("install", "Authentication failed or was cancelled"), "The update did not install: Authentication failed or was cancelled.");
  assert.equal(failLine("install", new Error("Failed to install package.")), "The update did not install: Failed to install package.");
  assert.equal(failLine("download", ""), "The update did not download.");
});

test("the fallback opens the release of the update", () => {
  assert.equal(releasePage("1.7.7"), "https://github.com/solutionscay/skiff/releases/tag/v1.7.7");
  assert.equal(releasePage("v1.7.7"), "https://github.com/solutionscay/skiff/releases/tag/v1.7.7");
});
