import { test } from "node:test";
import assert from "node:assert/strict";
import { workspaceHarness } from "./workspaceHarness.ts";

// SPLIT-SCAFFOLD: characterization coverage for the profile split.
const seams = "\nexports.__splitScaffold = { checkDisk, recents, remember, saveAsNow, treeView, workspaceMenu };";

test("profile keeps valid recents in newest-first order", () => {
  const h = workspaceHarness(seams);
  const seam = h.profile.__splitScaffold;
  h.profile.__splitScaffold.remember("/first.skiffw");
  h.profile.__splitScaffold.remember("/second.skiffw");
  h.profile.__splitScaffold.remember("/first.skiffw");
  assert.deepEqual(Array.from(seam.recents()), ["/first.skiffw", "/second.skiffw"]);
});

test("profile ignores invalid recent storage", () => {
  const h = workspaceHarness(seams);
  const seam = h.profile.__splitScaffold;
  h.stored.set("skiff.workspace.recent", "not JSON");
  assert.deepEqual(Array.from(seam.recents()), []);
});

test("a changed workspace saves as a new file", async () => {
  const h = workspaceHarness(seams);
  h.fixture("/work.skiffw");
  await h.profile.openWorkspace("/work.skiffw");
  h.setDiskRevision("other");
  await h.profile.__splitScaffold.checkDisk();
  assert.equal(h.profile.workspaceNotice().disk, "changed");
  assert.equal(h.profile.workspaceNotice().missing, 0);
  h.setSavePath("/copy");
  assert.equal(await h.profile.__splitScaffold.saveAsNow(), true);
  assert.equal(h.files.has("/copy.skiffw"), true);
  assert.equal(h.profile.workspaceName(), "/work.skiffw");
});

test("changed and missing disk notices can keep the current workspace", async () => {
  const h = workspaceHarness(seams);
  h.fixture("/work.skiffw");
  await h.profile.openWorkspace("/work.skiffw");
  h.setDiskRevision("other");
  await h.profile.__splitScaffold.checkDisk();
  assert.equal(h.profile.workspaceNotice().disk, "changed");
  assert.equal(h.profile.workspaceNotice().missing, 0);
  h.profile.keepWorkspace();
  assert.equal(h.profile.workspaceNotice().disk, null);
  h.setDiskRevision(null);
  await h.profile.__splitScaffold.checkDisk();
  assert.equal(h.profile.workspaceNotice().disk, "missing");
  h.profile.keepWorkspace();
  assert.equal(h.profile.workspaceEdited(), true);
});

test("opening restores the saved selection and folds", async () => {
  const h = workspaceHarness(seams);
  h.files.set("/work.skiffw", { path: "/work.skiffw", revision: "read", profile: {
    name: "work", projects: ["/a", "/b"], fleets: [], folded_projects: ["/a"], folded_fleets: [], selected: "/b",
  } });
  await h.profile.openWorkspace("/work.skiffw");
  assert.equal(h.S.selectedProject, "B");
  assert.equal(h.profile.__splitScaffold.treeView.isFolded("project:A"), true);
});

test("closing the status menu returns focus to its item", () => {
  const h = workspaceHarness(seams);
  let focused = 0;
  const item = {
    isConnected: true,
    hidden: false,
    getBoundingClientRect: () => ({ left: 1, top: 2 }),
    focus: () => focused++,
  };
  h.profile.__splitScaffold.workspaceMenu(item);
  h.menus[0]();
  assert.equal(focused, 1);
});
