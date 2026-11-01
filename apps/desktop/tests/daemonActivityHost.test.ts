// Settings › skiffd activity names the Host it shows (#202). Run: pnpm test
import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

class El {
  children: El[] = []; dataset: Record<string, string> = {}; id = ""; title = ""; type = ""; disabled = false; tabIndex = -1;
  classList = { toggle() {} };
  tag: string; className: string; textContent: string;
  constructor(tag: string, className = "", textContent = "") { this.tag = tag; this.className = className; this.textContent = textContent; }
  append(...c: El[]) { this.children.push(...c); }
  setAttribute() {}
  addEventListener() {}
}
const h = (tag: string, cls = "", text = "") => new El(tag, cls, text);

/** The real section, over a Host with the given name and capabilities. */
function section(name: string, caps: string[]) {
  const mod = moduleFromSource("../src/appearance/daemonActivity.ts", {
    "../app/hostCaps": { hostCan: (cap: string) => caps.includes(cap), hostInvoke: async () => [] },
    "../app/hosts": { hostName: () => name },
    "../app/keys": { isMac: true },
    "../ui/dom": { h },
    "../app/state": { S: { projects: [], closedProjects: [] } },
    "./sessionPick": { emptyPick: () => ({ sel: new Set(), now: null }), picked: () => [], pickedText: () => "", sinkLabel: () => "Sink" },
  }, { Map, Set, Date, String, Number, Math }, "");
  const [head, band] = mod.daemonActivitySection(mod.activityState(), () => {}, () => {}) as El[];
  return { sub: head.children[1].textContent, band: band.children.map((c) => c.textContent).filter(Boolean) };
}

test("the subtitle names the shown Host, and a Host without daemon metrics shows no Up or Memory", () => {
  const local = section("This computer (mac)", ["session_inventory", "daemon_metrics"]);
  assert.equal(local.sub, "This computer (mac)");
  assert.deepEqual(local.band, ["skiffd", "Up —", "0 sessions", "Memory —", "Sink"]);
  const remote = section("devbox", ["session_inventory"]);
  assert.equal(remote.sub, "devbox");
  assert.deepEqual(remote.band, ["skiffd", "0 sessions", "Sink"]);
});
