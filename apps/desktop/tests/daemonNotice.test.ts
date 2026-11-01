// The page's reports for the launch's own daemon steps. Run: pnpm test
import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

// Loaded through the harness, which supplies the word catalog.
const { opensAtOnce } = moduleFromSource("../src/app/daemonNotice.ts", {}, {}, "");

const KINDS = ["outdated", "protocol", "newer", "hung", "failed"] as const;

test("a warning from the launch opens no panel", () => {
  for (const kind of KINDS) assert.equal(opensAtOnce(kind, false), false, kind);
});

test("a restart the user asked for opens the panel of a daemon it cannot use", () => {
  for (const kind of ["protocol", "hung", "failed"] as const) assert.equal(opensAtOnce(kind, true), true, kind);
  for (const kind of ["outdated", "newer"] as const) assert.equal(opensAtOnce(kind, true), false, kind);
});
