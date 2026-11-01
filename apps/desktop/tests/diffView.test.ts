import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

type El = { tag: string; cls: string; text: string; kids: El[]; tabIndex?: number; append(...k: El[]): void; setAttribute(): void; firstElementChild: El | null };
const h = (tag: string, cls = "", text = ""): El => {
  const el: El = { tag, cls, text, kids: [], append: (...k) => void el.kids.push(...k), setAttribute() {}, get firstElementChild() { return el.kids[0] ?? null; } };
  return el;
};
const view = () => moduleFromSource("../src/terminal/diffView.ts", { "../ui/dom": { h } }, {}, "");
/** Arrays from the module's realm, as plain values. */
const plain = (x: unknown) => JSON.parse(JSON.stringify(x));
const diff = (o: object) => ({ text: "", truncated: false, binary: false, untracked: false, ...o });

const TEXT = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n ctx\n-old\n+<b>new</b>\n\\ No newline at end of file\n";

test("each line of the Host's text shows with its kind, as text", () => {
  const lines = view().diffLines(TEXT);
  assert.deepEqual(plain(lines.map((l: any) => l.cls)), ["dl-file", "dl-meta", "dl-meta", "dl-meta", "dl-hunk", "dl-ctx", "dl-del", "dl-add", "dl-meta"]);
  // Markup in a file is a line of text, never parsed.
  const body = view().diffBody(diff({ text: TEXT }), "a.txt");
  const pre = body.kids.find((k: El) => k.tag === "pre")!;
  assert.equal(pre.kids[7].text, "+<b>new</b>\n");
  assert.equal(pre.kids[7].tag, "span");
});

test("every state says what it is: no change, untracked, binary, cut, the whole worktree", () => {
  const notes = view().diffNotes;
  const diffNotes = (...a: unknown[]) => plain(notes(...a));
  assert.deepEqual(diffNotes(diff({}), "a.txt"), ["No change against HEAD."]);
  assert.deepEqual(diffNotes(diff({ text: "x", untracked: true }), "n.txt"), ["Untracked file. It is compared with an empty file."]);
  assert.deepEqual(diffNotes(diff({ text: "Binary files /dev/null and b/i.png differ\n", binary: true, untracked: true }), "i.png"), [
    "Untracked file. It is compared with an empty file.",
    "Binary file. There is no text to show.",
  ]);
  assert.deepEqual(diffNotes(diff({ text: "x", truncated: true }), "big.txt"), ["The diff is longer than 512 KiB. Only its start shows."]);
  assert.deepEqual(diffNotes(diff({ text: "x" })), ["Untracked files are not in this view. Select one to see it."]);
  // A binary diff shows git's note as one plain block, not as diff lines.
  const body = view().diffBody(diff({ text: "Binary files a/x and b/x differ\n", binary: true }), "x");
  assert.equal(body.kids.at(-1).kids.length, 0);
});

test("a failed read shows its cause as an alert", () => {
  const body = view().diffState("No diff: skiff-host is busy. Try again.", true);
  assert.equal(body.kids[0].cls, "diff-view-note diff-view-error");
  assert.equal(body.kids[0].text, "No diff: skiff-host is busy. Try again.");
});
