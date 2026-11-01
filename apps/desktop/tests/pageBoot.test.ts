import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { build } from "vite";

/** A value that answers every property, call and `new` with another one. It
 *  stands in for the DOM and Tauri, so only the modules' own code can throw. */
function stub(): any {
  const props = new Map<PropertyKey, unknown>();
  return new Proxy(function () {}, {
    get(_, key) {
      if (key === Symbol.toPrimitive) return () => "";
      if (key === Symbol.iterator) return function* () {};
      if (typeof key === "symbol" || key === "then") return undefined;
      if (!props.has(key)) props.set(key, stub());
      return props.get(key);
    },
    set(_, key, value) {
      props.set(key, value);
      return true;
    },
    apply: () => stub(),
    construct: () => stub(),
  });
}

// The blank window: one module throws while the page loads, so main.ts never
// runs and nothing is shown. Bundle the page the way vite does and evaluate it.
test("the page loads every module without an evaluation error", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const out = (await build({
    root,
    configFile: false,
    logLevel: "silent",
    build: {
      write: false,
      minify: false,
      lib: { entry: "src/main.ts", formats: ["iife"], name: "skiffPage", fileName: "page" },
    },
  })) as { output: { fileName: string; type: string; code?: string }[] }[];
  const page = out.flatMap((o) => o.output).find((f) => f.type === "chunk" && f.fileName.endsWith(".js"));
  assert.ok(page?.code, "vite gave no page script");

  // Tauri's own calls reject without a webview, as in a plain browser.
  const context = vm.createContext({ __TAURI_INTERNALS__: undefined, setTimeout: () => 0, setInterval: () => 0 });
  const real = vm.runInContext("globalThis", context);
  const scope = new Proxy({}, {
    has: (_, key) => key !== "__scope",
    get: (_, key) => (typeof key === "symbol" ? undefined : key in real ? real[key] : (real[key] = stub())),
    set: (_, key, value) => ((real[key] = value), true),
  });
  context.__scope = scope;
  // boot() then fails on its first Tauri call; that rejection is expected.
  const ignore = () => {};
  process.on("unhandledRejection", ignore);
  try {
    vm.runInContext(`with (__scope) {\n${page.code}\n}`, context, { filename: "page.js" });
  } finally {
    await new Promise((r) => setImmediate(r));
    process.off("unhandledRejection", ignore);
  }
});
