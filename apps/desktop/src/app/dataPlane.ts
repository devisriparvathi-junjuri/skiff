/**
 * The selected Host's data plane (`skiff-host`): it serves projects and Git,
 * apart from skiffd's sessions and terminals. The page checks it on its own
 * and reads projects and Git again when a new data connection comes up: the
 * service started late, restarted, or the link came back. A read that
 * failed is tried again at the next check, on the same connection too. No
 * page reload, and no terminal stream is touched. Each new connection, or
 * its loss, is a new data epoch: a reply asked for in an older epoch is not
 * shown.
 */
import { invoke } from "@tauri-apps/api/core";

export interface DataStatus {
  ready: boolean;
  /** No live session plane: the data plane was not tried. */
  waiting: boolean;
  host_id: string | null;
  instance_id: string | null;
  /** Changes with every new data connection. */
  connection: number;
  capabilities: string[];
  message: string | null;
}

/** The checks: often while it is out of reach, then less often. */
const READY_EVERY_MS = 5000;
const RETRY_FIRST_MS = 1000;
const RETRY_MAX_MS = 30_000;

let epoch = 0;
/** The current data epoch. A reply from an older one is not shown. */
export const dataEpoch = () => epoch;

/** What a new status asks of the page: read again, show the loss, or nothing. */
export function dataChange(prev: DataStatus | null, next: DataStatus): "reload" | "down" | null {
  // The session plane is down: its own recovery runs, and this waits.
  if (next.waiting) return null;
  if (next.ready) return !prev?.ready || prev.connection !== next.connection || prev.host_id !== next.host_id ? "reload" : null;
  return !prev || prev.ready || prev.message !== next.message ? "down" : null;
}

interface DataHooks {
  /** A new data connection: capabilities, projects and Git again. False:
   *  a read failed, so the next ready check reads again. */
  reload(): Promise<boolean>;
  /** Out of reach, and why. */
  down(message: string | null): void;
  /** What the page shows came from a good read, so a first ready status needs none. */
  fresh(): boolean;
}

let last: DataStatus | null = null;
let timer: number | undefined;
let checking = false;
/** The last reload failed: the next ready check reads again. */
let unfinished = false;

/** One check, and what it changes. Returns the status, or null when the check failed. */
export async function checkDataPlane(hooks: DataHooks): Promise<DataStatus | null> {
  if (checking) return last;
  checking = true;
  try {
    const next = await invoke<DataStatus>("data_plane_status").catch(() => null);
    if (!next) return null;
    const first = last === null;
    const change = first && next.ready && hooks.fresh() ? null : dataChange(last, next);
    if (!next.waiting) last = next;
    if (change) epoch++;
    if (change === "reload" || (next.ready && !change && unfinished)) unfinished = !(await hooks.reload());
    else if (change === "down") hooks.down(next.message);
    return next;
  } finally {
    checking = false;
  }
}

/** Checks on its own, from now on: every few seconds while ready, and 1 s,
 *  then twice as long up to 30 s, while out of reach. */
export function watchDataPlane(hooks: DataHooks) {
  let wait = RETRY_FIRST_MS;
  const tick = async () => {
    const s = await checkDataPlane(hooks);
    let next = READY_EVERY_MS;
    if (s?.ready) wait = RETRY_FIRST_MS;
    else if (s?.waiting) next = RETRY_FIRST_MS * 2;
    else {
      next = wait;
      wait = Math.min(wait * 2, RETRY_MAX_MS);
    }
    timer = window.setTimeout(tick, next);
  };
  window.clearTimeout(timer);
  void tick();
}
