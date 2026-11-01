/**
 * Keystroke latency trace. Help → Start latency trace turns it on; SKIFF_TRACE
 * turns it on at launch. While on, a corner of the focused pane shows the
 * numbers, and each keystroke appends one JSON line to latency.jsonl in the
 * app's log folder (or the SKIFF_TRACE file). All times are ms on the page
 * clock (performance.now), so the stages add up:
 *
 *   queue       keydown created → the page handles it (main thread busy)
 *   ipc         pty_write sent → Rust took it
 *   daemon-in   pty_write sent → skiffd wrote it to the PTY
 *   program     skiffd wrote it → skiffd read the first output after it
 *   daemon-out  skiffd read it → the output reached the page
 *   parse       output arrived → xterm parsed it
 *   render      parsed → xterm drew the frame
 *   total       keydown → xterm drew the frame
 *
 * `echo` (sent → output back) is daemon-in + program + daemon-out. `program`
 * is on skiffd's clock alone; daemon-in and daemon-out compare skiffd's wall
 * clock with the page's, and are clamped so the three add up to `echo`.
 * A daemon older than the split leaves them null.
 *
 * `busy` is the bytes the session printed in the 250 ms before the key: a
 * streaming program makes `echo` include other output. Each line also holds
 * the system load (`cpu`, `io`, `load`, and the `band` from them) and the
 * `build` of the app and skiffd. Main-thread stalls and slow async work go
 * in lines of their own (see stalls.ts), and so do the counts of renders by
 * cause and each step's runs and total ms, every 2 s (`tally`, `steps`). scripts/latency.py summarizes the
 * file; Help → Copy latency report summarizes this run.
 */
import { invoke } from "@tauri-apps/api/core";
import { copyText } from "../platform/clipboard";
import { button, h } from "../ui/dom";
import { showError } from "../ui/alerts";
import { scheduleRender } from "../app/render";
import { S } from "../app/state";
import { startStalls, stopStalls, takeTallies, type StepTime } from "./stalls";
import { t, tn } from "../i18n";

interface Entry {
  sid: string;
  key: number | null;
  sent: number;
  /** `sent` as Unix ms, to compare with skiffd's times. */
  sentWall: number;
  ipc?: number;
  echo?: number;
  /** skiffd's times for the echo, Unix ms. */
  times?: EchoTimes;
  bytes?: number;
  parsed?: number;
  drawn?: number;
  busy: number;
}

/** skiffd's times for the first output after a write, Unix µs. */
interface EchoTimes {
  wrote: number;
  read: number;
}

/** System load, from the `system_load` command. */
interface Load {
  cpu: number | null;
  io: number | null;
  load: number | null;
}

type Band = "low" | "medium" | "high" | "unknown";
type Stage = "queue" | "ipc" | "daemon-in" | "program" | "daemon-out" | "echo" | "parse" | "render" | "total";
type Row = Record<Stage, number | null> & Load & { at: number; sid: string; bytes: number | null; busy: number; band: Band; build: string };
interface Stall {
  at: number;
  stall: number;
  steps: StepTime[];
  band: Band;
}
interface Work {
  at: number;
  work: string;
  ms: number;
  band: Band;
}

const STAGES: Stage[] = ["queue", "ipc", "daemon-in", "program", "daemon-out", "parse", "render", "total"];
const BANDS: Band[] = ["low", "medium", "high", "unknown"];
/** Two frames at 60 Hz. A key slower than this shows red. */
const SLOW_MS = 33;
const MAX_ROWS = 20_000;

let on = false;
let file = "";
let timers: number[] = [];
let startedAt = 0;
let rows: Row[] = [];
let stalls: Stall[] = [];
let works: Work[] = [];
let lines: string[] = [];
let load: Load = { cpu: null, io: null, load: null };
let build = "";
/** Per session, skiffd's times for the output that comes next. */
const echoes = new Map<string, EchoTimes>();
const lastKey = new Map<string, number>();
const pending = new Map<string, Entry[]>();
const recent = new Map<string, { t: number; n: number }[]>();

export const tracing = () => on;
export const hasReport = () => rows.length > 0 || stalls.length > 0;

/** Starts the trace at launch when SKIFF_TRACE is set. */
export async function initTrace() {
  const [path, fromEnv] = await invoke<[string, boolean]>("trace_info").catch(() => ["", false] as [string, boolean]);
  file = path;
  if (fromEnv) startTrace();
}

export function startTrace() {
  if (on) return;
  on = true;
  startedAt = Date.now();
  rows = [];
  stalls = [];
  works = [];
  build = "";
  invoke<string>("trace_set", { on: true }).then((b) => (build = b), console.error);
  sampleLoad();
  timers = [
    window.setInterval(flush, 2000),
    // Pressure is a 10 s average, so a sample a second is enough.
    window.setInterval(sampleLoad, 1000),
    // A key with no echo (a shortcut, a program that does not echo) ends here.
    window.setInterval(() => {
      const now = performance.now();
      for (const list of pending.values())
        while (list.length && now - list[0].sent > 1000) done(list.shift()!);
    }, 500),
  ];
  startStalls({
    stall(ms, steps) {
      const st: Stall = { at: Date.now(), stall: ms, steps, band: band(load) };
      stalls.push(st);
      lines.push(JSON.stringify({ ...st, ...load, build }));
    },
    work(name, ms) {
      const w: Work = { at: Date.now(), work: name, ms: Math.round(ms * 10) / 10, band: band(load) };
      works.push(w);
      lines.push(JSON.stringify({ ...w, ...load, build }));
    },
  });
  indicator();
  hud();
  probeGl();
}

function sampleLoad() {
  invoke<Load>("system_load").then((l) => (load = l), console.error);
}

/**
 * The load band: CPU or IO pressure where the system reports it (Linux),
 * else the load average per core. scripts/latency.py reads the stored band.
 */
function band(l: Load): Band {
  const p = Math.max(l.cpu ?? -1, l.io ?? -1);
  if (p >= 0) return p < 5 ? "low" : p < 25 ? "medium" : "high";
  if (l.load === null) return "unknown";
  const perCore = l.load / (navigator.hardwareConcurrency || 1);
  return perCore < 0.3 ? "low" : perCore < 0.8 ? "medium" : "high";
}

/** One bare context, so the `gl` lines can be read against what a context alone costs. */
function probeGl() {
  const t = performance.now();
  const g = document.createElement("canvas").getContext("webgl2", { preserveDrawingBuffer: true });
  lines.push(JSON.stringify({ at: Date.now(), glprobe: performance.now() - t, ok: !!g }));
  g?.getExtension("WEBGL_lose_context")?.loseContext();
}

/** A WebGL renderer attached to a pane: the addon's construction, and `loadAddon` (context, shaders, atlas, first draw), in ms. `warm`: in an idle frame, not for a shown pane. */
export function traceGl(sid: string, ctor: number, load: number, warm: boolean) {
  if (on) lines.push(JSON.stringify({ at: Date.now(), sid, gl: { ctor, load, warm } }));
}

export function stopTrace() {
  if (!on) return;
  on = false;
  timers.forEach(clearInterval);
  timers = [];
  lastKey.clear();
  pending.clear();
  recent.clear();
  echoes.clear();
  stopStalls();
  invoke("trace_set", { on: false }).catch(console.error);
  flush();
  hudEl?.remove();
  indicatorEl?.remove();
}

/** keydown in a terminal: `timeStamp` is when the event was created. */
export function traceKey(sid: string, e: KeyboardEvent) {
  if (on && e.type === "keydown") lastKey.set(sid, e.timeStamp);
}

/** onData: the terminal is about to send input. Returns a callback for the send's reply. */
export function traceSend(sid: string): (() => void) | undefined {
  if (!on) return;
  const now = performance.now();
  const k = lastKey.get(sid);
  lastKey.delete(sid);
  const busy = (recent.get(sid) ?? []).filter((r) => now - r.t < 250).reduce((a, r) => a + r.n, 0);
  const entry: Entry = { sid, key: k !== undefined && now - k < 100 ? k : null, sent: now, sentWall: performance.timeOrigin + now, busy };
  (pending.get(sid) ?? pending.set(sid, []).get(sid)!).push(entry);
  return () => (entry.ipc = performance.now());
}

/** skiffd's times for the output that comes next on this session's stream. */
export function traceEcho(sid: string, m: unknown): boolean {
  if (typeof m !== "object" || m === null || !("wrote" in m) || !("read" in m)) return false;
  if (on) echoes.set(sid, m as EchoTimes);
  return true;
}

/** Output reached the page. Returns a callback for when xterm has parsed it. */
export function traceOutput(sid: string, bytes: number): (() => void) | undefined {
  if (!on) return;
  const now = performance.now();
  const r = recent.get(sid) ?? recent.set(sid, []).get(sid)!;
  r.push({ t: now, n: bytes });
  while (r.length && now - r[0].t > 250) r.shift();
  const times = echoes.get(sid);
  echoes.delete(sid);
  const waiting = (pending.get(sid) ?? []).filter((e) => e.echo === undefined);
  if (!waiting.length) return;
  for (const e of waiting) {
    e.echo = now;
    e.bytes = bytes;
    e.times = times;
  }
  return () => {
    const t = performance.now();
    for (const e of waiting) e.parsed = t;
  };
}

/** xterm drew a frame for this session. */
export function traceRender(sid: string) {
  if (!on) return;
  const list = pending.get(sid);
  if (!list) return;
  const t = performance.now();
  while (list.length && list[0].parsed !== undefined) {
    const e = list.shift()!;
    e.drawn = t;
    done(e);
  }
}

const round = (x: number) => Math.round(x * 100) / 100;

/** `echo` split into daemon-in, program and daemon-out, which add up to it. */
function split(e: Entry, echo: number | null) {
  const t = e.times;
  if (echo === null || !t) return { "daemon-in": null, program: null, "daemon-out": null };
  const program = Math.min(echo, Math.max(0, (t.read - t.wrote) / 1000));
  const rest = echo - program;
  const inn = Math.min(rest, Math.max(0, t.wrote / 1000 - e.sentWall));
  return { "daemon-in": round(inn), program: round(program), "daemon-out": round(rest - inn) };
}

function done(e: Entry) {
  const d = (a?: number | null, b?: number | null) => (a == null || b == null ? null : round(b - a));
  const echo = d(e.sent, e.echo);
  const row: Row = {
    at: Date.now(),
    sid: e.sid,
    queue: d(e.key, e.sent),
    ipc: d(e.sent, e.ipc),
    ...split(e, echo),
    echo,
    parse: d(e.echo, e.parsed),
    render: d(e.parsed, e.drawn),
    total: d(e.key ?? e.sent, e.drawn),
    bytes: e.bytes ?? null,
    busy: e.busy,
    ...load,
    band: band(load),
    build,
  };
  rows.push(row);
  if (rows.length > MAX_ROWS) rows.splice(0, rows.length - MAX_ROWS);
  lines.push(JSON.stringify(row));
  hud();
}

function flush() {
  const { tally, steps } = takeTallies();
  const ms = [...steps].map(([k, [n, t]]) => [k, [n, Math.round(t * 10) / 10]]);
  if (tally.size || steps.size) lines.push(JSON.stringify({ at: Date.now(), tally: Object.fromEntries(tally), steps: Object.fromEntries(ms) }));
  if (!lines.length) return;
  const out = lines;
  lines = [];
  invoke("trace_write", { lines: out }).catch(console.error);
}

function pct(xs: number[], p: number): number {
  return xs[Math.min(xs.length - 1, Math.floor((p / 100) * xs.length))];
}

function values(rs: Row[], s: Stage): number[] {
  return rs.map((r) => r[s]).filter((v): v is number => v !== null).sort((a, b) => a - b);
}

// The overlay redraws at most 4 times a second, so it costs little itself.
let hudEl: HTMLElement | null = null;
let hudTimer: number | undefined;

// While the trace runs the status bar says so, with the key count; a click stops it.
let indicatorEl: HTMLButtonElement | null = null;

function indicator() {
  indicatorEl ??= button("status-item trace-indicator", "", () => {
    stopTrace();
    scheduleRender("trace");
  });
  indicatorEl.title = t("latency.stop");
  indicatorEl.replaceChildren(h("span", "dot"), h("span", "", tn("latency.indicator", rows.length)));
  if (!indicatorEl.isConnected) document.querySelector("#statusbar .spacer")?.before(indicatorEl);
}

function hud() {
  if (hudTimer !== undefined) return;
  hudTimer = window.setTimeout(() => {
    hudTimer = undefined;
    if (!on) return;
    indicator();
    const cell = [...document.querySelectorAll<HTMLElement>(".cell")].find((c) => c.dataset.session === S.focused);
    if (!cell) return hudEl?.remove();
    hudEl ??= h("div", "latency-hud");
    const mine = rows.filter((r) => r.sid === S.focused && r.total !== null);
    const last = mine.at(-1)?.total ?? null;
    const totals = values(mine, "total");
    hudEl.classList.toggle("slow", last !== null && last > SLOW_MS);
    hudEl.replaceChildren(
      h("b", "", last === null ? t("latency.typeToMeasure") : `${last.toFixed(1)} ms`),
      h("span", "", totals.length ? tn("latency.percentiles", totals.length, { p50: pct(totals, 50).toFixed(1), p99: pct(totals, 99).toFixed(1) }) : t("latency.on")),
    );
    if (hudEl.parentElement !== cell) cell.append(hudEl);
  }, 250);
}

/** Per-stage percentiles for this run: by quiet and printing session, then by load band. */
function report(): string {
  const out = [
    `Skiff keystroke latency, ${new Date(startedAt).toISOString()} to ${new Date().toISOString()}`,
    `${navigator.userAgent}`,
    `Build: ${build || "unknown"}`,
    `Trace file: ${file}`,
  ];
  const table = (title: string, rs: Row[]) => {
    out.push("", `${title}: ${rs.length} keys`, `  ${"stage".padEnd(10)} ${["n", "p50", "p90", "p99", "max"].map((c) => c.padStart(7)).join(" ")}`);
    // A daemon older than the split leaves one `echo`.
    const shown: Stage[] = rs.some((r) => r.program !== null) ? STAGES : ["queue", "ipc", "echo", "parse", "render", "total"];
    for (const s of shown) {
      const xs = values(rs, s);
      out.push(
        `  ${s.padEnd(10)} ${String(xs.length).padStart(7)} ` +
          (xs.length ? [50, 90, 99].map((p) => pct(xs, p).toFixed(2).padStart(7)).join(" ") + " " + xs[xs.length - 1].toFixed(2).padStart(7) : ""),
      );
    }
    const noEcho = rs.filter((r) => r.echo === null).length;
    if (noEcho) out.push(`  no echo within 1 s: ${noEcho}`);
  };
  table("Quiet session", rows.filter((r) => !r.busy));
  table("Session printing", rows.filter((r) => r.busy));
  for (const b of BANDS) {
    const rs = rows.filter((r) => r.band === b);
    if (rs.length) table(`Load ${b}`, rs);
  }
  out.push("", `Main-thread stalls over 50 ms: ${stalls.length}`);
  for (const st of [...stalls].sort((a, b) => b.stall - a.stall).slice(0, 10))
    out.push(`  ${st.stall.toFixed(1).padStart(7)} ms  ${st.band.padEnd(7)} ${st.steps.map((s) => `${s.name} ${s.ms}`).join(", ") || "no step recorded"}`);
  if (works.length) {
    out.push("", `Slow work over 50 ms: ${works.length}`);
    for (const w of [...works].sort((a, b) => b.ms - a.ms).slice(0, 10)) out.push(`  ${w.ms.toFixed(1).padStart(7)} ms  ${w.band.padEnd(7)} ${w.work}`);
  }
  return out.join("\n");
}

export async function copyReport() {
  await copyText(report()).catch(showError);
}
