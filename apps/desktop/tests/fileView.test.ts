import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

type El = { tag: string; cls: string; text: string; kids: El[]; src?: string; alt?: string; tabIndex?: number; append(...k: El[]): void };
const h = (tag: string, cls = "", text = ""): El => {
  const el: El = { tag, cls, text, kids: [], append: (...k) => void el.kids.push(...k) };
  return el;
};
const view = () => moduleFromSource("../src/terminal/fileView.ts", { "../ui/dom": { h } }, { atob, Blob, Uint8Array }, "");
const plain = (x: unknown) => JSON.parse(JSON.stringify(x));
const file = (o: object) => ({ content: "text", size: 0, truncated: false, ...o });

test("a Host's text shows as text, never parsed, and says when it is cut", () => {
  const body = view().fileBody(file({ text: "<script>x</script>\n", size: 19 }), () => "blob:x");
  const pre = body.kids.find((k: El) => k.tag === "pre")!;
  assert.equal(pre.text, "<script>x</script>\n");
  assert.equal(pre.kids.length, 0);
  assert.deepEqual(plain(view().fileNotes(file({ text: "a", truncated: true }))), ["The file is longer than 512 KiB. Only its start shows."]);
  assert.deepEqual(plain(view().fileNotes(file({ text: "" }))), ["Empty file."]);
});

test("an image draws from the Host's bytes only for an image type; other kinds say what they are", () => {
  let made: Blob | null = null;
  const png = file({ content: "image", mime: "image/png", bytes: "iVBORw0KGgo=", size: 8 });
  const body = view().fileBody(png, (b: Blob) => ((made = b), "blob:png"));
  const img = body.kids.find((k: El) => k.tag === "img")!;
  assert.equal(img.src, "blob:png");
  assert.equal(made!.type, "image/png");
  assert.equal(made!.size, 8);
  // A type the Host did not find in the bytes, such as SVG, is never drawn.
  for (const f of [file({ content: "image", mime: "image/svg+xml", bytes: "PHN2Zz4=" }), file({ content: "image", mime: "image/png" })]) {
    assert.equal(view().imageBlob(f), null);
  }
  const notes = (f: object) => plain(view().fileNotes(file(f)));
  assert.deepEqual(notes({ content: "image", mime: "image/png", size: 9 * 1024 * 1024, truncated: true }), ["Image, 9.0 MiB. Skiff shows images up to 5 MiB."]);
  assert.deepEqual(notes({ content: "binary", size: 2048 }), ["Binary file, 2 KiB. There is no text to show."]);
  assert.deepEqual(notes({ content: "link", target: "../secret" }), ["Link to ../secret. Skiff does not follow it."]);
});
