import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { h, button } from "../ui/dom";
import { rune } from "../ui/runes";
import { ctxMenu } from "../ui/contextMenu";
import type { MenuEntry } from "../ui/menu";
import { confirmAction, promptAction } from "../ui/confirm";
import { actionDialog } from "../ui/dialogParts";
import { runDialog } from "./hostProgress";
import { copyText } from "../platform/clipboard";
import { type HostsPage, type KnownHost, knownHosts, loadHosts, localHostName, pick, shownHost } from "../app/hosts";

/**
 * Settings › Hosts: the known Hosts, the form that adds or edits one, and
 * a Host's check list. The list is this computer's own (`hosts.toml`), so
 * the tab works the same while any Host is shown. The form needs one field,
 * Server. Name, user and port sit under More: ssh config gives them, the
 * line under Server says what `ssh -G` makes of them, with no network, and
 * the check finds the socket: each server has one skiffd, on
 * `<runtime dir>/skiff/skiffd.sock`. Connect saves, then checks.
 * A failed row has at most one fix. Each fix asks first, names what it
 * changes, and then checks again. A password or passphrase goes to Rust,
 * which gives it to ssh through askpass only; the page keeps none.
 */

interface Resolved {
  hostname: string;
  user: string;
  port: number;
  jump: string;
}

interface Alias {
  name: string;
  goes_to: string;
}

type Draft = { destination: string; name: string; user: string; port: string };

type Form = { id: string | null; draft: Draft; more: boolean; error: string | null; busy: boolean };

/** One row of the check list, as Rust's `check_host` answers it. */
interface CheckLine {
  row: string;
  title: string;
  state: "passed" | "failed" | "skipped" | "warning";
  text: string;
  fix: Fix | null;
}
type Fix = "trust_key" | "review_key" | "make_key" | "unlock_key" | "install_key" | "copy_public_key" | "fix_permissions" | "install_skiff" | "start_skiff" | "turn_on_linger" | "update_skiff";
const FIX_LABEL: Record<Fix, string> = {
  trust_key: "Trust this key…",
  review_key: "Review…",
  make_key: "Make key…",
  unlock_key: "Unlock key…",
  install_key: "Install key…",
  copy_public_key: "Copy public key",
  fix_permissions: "Fix permissions…",
  install_skiff: "Install Skiff…",
  start_skiff: "Start",
  turn_on_linger: "Turn on…",
  update_skiff: "Update Skiff…",
};
/** What `update_skiff` did. */
type Updated = { version: string; source: string } & (
  | { result: "done"; from: string; restarted: boolean }
  | { result: "same" }
  | { result: "newer"; from: string }
  | { result: "no_handover"; from: string; sessions: string[] }
);
interface Fingerprint {
  kind: string;
  sha256: string;
}
interface HostKey {
  token: string;
  fingerprints: Fingerprint[];
  known: Fingerprint[];
  file: string;
}
interface Report {
  rows: CheckLine[];
  ok: boolean;
  /** The socket in use. */
  socket: string | null;
  /** The server's OS and arch as bundle names write them. */
  platform: [string, string] | null;
  /** The Skiff version this app installs. */
  version: string;
  /** The server has this version in another build. */
  other_build: boolean;
}
/** The rows, in order, before an answer. */
const CHECK_ROWS = [
  "Reach the server",
  "Host key",
  "Log in without a prompt",
  "Server supported",
  "Skiff on the server",
  "Sessions service (skiffd)",
  "Socket forwarding",
  "Skiff identity and version",
  "Projects service (skiff-host)",
  "Keeps running after logout",
];

/** A run's outcome on its check list row: the text, the time it took or
 *  the failure with its fix, and the output to copy. */
type RowResult = { row: string; text: string; secs?: number; failed?: boolean; fix?: Fix; output?: string };
type Check = { id: string; report: Report | null; error: string | null; note?: string; result?: RowResult };

export function createHostsTab(redraw: () => void) {
  let form: Form | null = null;
  let check: Check | null = null;
  let error: string | null = null;
  let aliases: Alias[] = [];
  /** This app's version, for this computer's Skiff cell. */
  let version = "";
  /** The form's resolve line, and the input it answers. */
  let line: { for: string; text: string; bad: boolean } = { for: "", text: "", bad: false };
  let timer: ReturnType<typeof setTimeout> | undefined;

  const typed = (x: KnownHost) => `${x.user ? `${x.user}@` : ""}${x.destination}${x.port ? `:${x.port}` : ""}`;
  const resolve = (destination: string, user: string, port: string) =>
    invoke<Resolved>("resolve_ssh", { destination, user: user ?? "", port: port ?? "" });

  /** Reads ssh config and resolves each Host. Local only. */
  async function load() {
    version = await getVersion().catch(() => "");
    aliases = await invoke<Alias[]>("ssh_aliases").catch(() => []);
  }

  function open(page: HostsPage) {
    error = null;
    form = "add" in page ? newForm(null) : null;
    check = null;
    if ("check" in page) void (page.reinstall || page.update ? checkThenUpdate(page.check, !!page.reinstall) : runCheck(page.check));
  }

  /** Runs the read-only check list of a Host and shows it. */
  /** `result`: what a run just did, kept on its row through this check. */
  async function runCheck(id: string, result?: RowResult) {
    form = null;
    const c: Check = { id, report: null, error: null, result };
    check = c;
    redraw();
    try {
      c.report = await invoke<Report>("check_host", { id });
      await loadHosts();
      await load();
    } catch (e) {
      c.error = String(e);
    }
    if (check === c) redraw();
  }

  function newForm(x: KnownHost | null): Form {
    const draft: Draft = x
      ? { destination: x.destination, name: x.name, user: x.user ?? "", port: x.port ? String(x.port) : "",  }
      : { destination: "", name: "", user: "", port: "" };
    line = { for: "", text: "", bad: false };
    const more = !!x && (!!x.user || !!x.port || x.name !== defaultName(x.destination));
    return { id: x?.id ?? null, draft, more, error: null, busy: false };
  }

  const defaultName = (d: string) => d.split("@").pop() ?? d;

  function draw(): HTMLElement {
    // The Host it was about left the list: back to the list.
    const known = (id: string) => knownHosts().some((x) => x.id === id);
    if (check && !known(check.id)) check = null;
    if (form?.id && !known(form.id)) form = null;
    return form ? drawForm(form) : check ? drawCheck(check) : drawList();
  }

  function drawList(): HTMLElement {
    const main = h("div", "set-main");
    const head = h("div", "set-head set-head-row");
    const words = h("div", "set-head-words");
    const title = h("div", "set-h", "Hosts");
    title.id = "set-h-hosts";
    words.append(title);
    const add = button("set-add ready", "Add SSH Host…", () => {
      form = newForm(null);
      redraw();
      focusServer();
    });
    head.append(words, add);
    main.append(head);
    const table = h("div", "set-table g-hosts");
    table.setAttribute("role", "table");
    table.setAttribute("aria-labelledby", "set-h-hosts");
    const cols = h("div", "set-row set-cols");
    cols.append(h("span", "c-lead"), h("span", "", "NAME"), h("span", "", "SERVER"), h("span", "", "SKIFF"), h("span", "", "STATE"), h("span", ""));
    table.append(cols, hostRow(null));
    for (const x of knownHosts()) table.append(hostRow(x));
    main.append(table);
    if (!knownHosts().length) main.append(h("div", "set-note set-line", "No servers yet."));
    if (error) main.append(alert(error));
    return main;
  }

  /** One Host. `null`: this computer, which has nothing to set: no menu,
   *  no click, no hover. A Host row opens its check list on a click or
   *  Enter; its other actions are in the row menu (⋯, right-click, the
   *  menu key). */
  function hostRow(x: KnownHost | null): HTMLElement {
    const row = h("div", x ? "set-row set-host act" : "set-row set-host");
    row.setAttribute("role", "row");
    const shown = x ? shownHost() === x.id : shownHost() === null;
    const lead = h("span", "c-lead");
    if (shown) lead.append(rune("indicators-check"));
    lead.setAttribute("aria-label", shown ? "Shown in this window" : "");
    const skiff = h("span", "c-skiff mono" + (x?.update ? " update" : ""), x ? (x.skiff ?? "—") : version || "—");
    const more = h("span", "c-more");
    row.append(lead, h("span", "c-name", x ? x.name : localHostName()), h("span", "c-server mono", x ? typed(x) : localHostName()), skiff, stateCell(x), more);
    if (!x) return row;
    row.tabIndex = 0;
    row.setAttribute("aria-label", `${x.name}: open its check list`);
    const menu = button("c-menu", "⋯", () => {});
    menu.tabIndex = -1;
    menu.title = "More";
    menu.setAttribute("aria-label", `More for ${x.name}`);
    const openMenu = (at: { x: number; y: number }) => hostMenu(x, at.x, at.y, row);
    menu.addEventListener("click", (e: MouseEvent) => {
      e.stopPropagation();
      const b = menu.getBoundingClientRect();
      openMenu({ x: b.left, y: b.bottom });
    });
    more.append(menu);
    row.addEventListener("click", () => void runCheck(x.id));
    row.addEventListener("contextmenu", (e: MouseEvent) => {
      e.preventDefault();
      openMenu({ x: e.clientX, y: e.clientY });
    });
    row.addEventListener("keydown", (e: KeyboardEvent) => {
      if (e.key === "Enter") void runCheck(x.id);
      else if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
        const b = menu.getBoundingClientRect();
        openMenu({ x: b.left, y: b.bottom });
      } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        const rows = [...(row.parentElement?.querySelectorAll<HTMLElement>(".set-host.act") ?? [])];
        rows[rows.indexOf(row) + (e.key === "ArrowDown" ? 1 : -1)]?.focus();
      } else return;
      e.preventDefault();
    });
    return row;
  }

  /** The row menu: Check, Update Skiff… when offered, Edit…, Rename…, Remove…. */
  function hostMenu(x: KnownHost, left: number, top: number, back: HTMLElement) {
    const entries: MenuEntry[] = [
      { icon: "indicators-check", label: "Check", run: () => void runCheck(x.id) },
      ...(x.update ? [{ icon: "schedule-refresh-cw", label: "Update Skiff…", run: () => void checkThenUpdate(x.id) }] : []),
      {
        icon: "tools-pencil",
        label: "Edit…",
        run: () => {
          form = newForm(x);
          redraw();
          focusServer();
        },
      },
      { label: "Rename…", run: () => rename(x) },
      { sep: true },
      { icon: "indicators-x", label: "Remove…", danger: true, run: () => void remove(x) },
    ];
    ctxMenu.open(left, top, x.name, entries, () => back.focus());
  }

  /** Update Skiff… from the list: the check list first, for the version
   *  and platform the dialog names, then the dialog. `reinstall`: this
   *  app's build goes in even when the server has it. */
  async function checkThenUpdate(id: string, reinstall = false) {
    await runCheck(id);
    const x = knownHosts().find((k) => k.id === id);
    if (x && check?.id === id && check.report) updateSkiff(check, x, reinstall);
  }

  /** Dot and word, from the last check or connection in this app run. */
  function stateCell(x: KnownHost | null): HTMLElement {
    const [cls, word] = !x
      ? ["ok", "Connected"]
      : x.state === "ok"
        ? ["ok", "Connected"]
        : x.state === "attention"
          ? ["bad", "Needs attention"]
          : ["none", "Not checked"];
    const cell = h("span", `c-state ${cls}`);
    cell.append(h("span", "sq"), word);
    return cell;
  }

  function drawCheck(c: Check): HTMLElement {
    const x = knownHosts().find((k) => k.id === c.id);
    const name = x?.name ?? "this Host";
    const main = h("div", "set-main");
    const head = h("div", "set-head");
    head.append(
      button("set-back", "‹ Hosts", () => ((check = null), redraw())),
      h("div", "set-h", `Check ${name}`),
    );
    main.append(head);
    const table = h("div", "set-table g-check");
    table.setAttribute("role", "table");
    table.setAttribute("aria-label", `Check ${name}`);
    const rows: CheckLine[] = c.report?.rows ?? CHECK_ROWS.map((title, i) => ({ row: String(i), title, state: "skipped", text: i === 0 ? "Checking…" : "Waiting", fix: null }));
    for (const r of rows) {
      // The row a run just changed shows what the run did, until the next check.
      const res = c.result && r.title === CHECK_ROWS[4] ? c.result : null;
      const state = res ? (res.failed ? "failed" : "passed") : c.report ? r.state : "waiting";
      const row = h("div", `set-row set-check ${state}`);
      row.setAttribute("role", "row");
      const mark = h("span", "c-lead");
      if (state === "passed") mark.append(rune("indicators-check"));
      else mark.append(h("span", "sq"));
      mark.setAttribute("aria-label", state);
      const text = h("span", "c-text", res ? res.text : r.text);
      if (res?.secs !== undefined && !res.failed) text.append(h("span", "c-secs", `${res.secs} s`));
      const fix = res ? res.fix : c.report && (r.state === "failed" || r.state === "warning") ? r.fix : null;
      if (fix && x) text.append(button("set-rm set-fix", res?.failed ? "Try again" : FIX_LABEL[fix], () => void runFix(c, x, fix)));
      if (res?.output) {
        const out = res.output;
        text.append(button("set-rm set-fix", "Copy output", () => void copyText(out)));
      }
      row.append(mark, h("span", "c-name", r.title), text);
      table.append(row);
    }
    main.append(table);
    if (x) main.append(autoUpdate(x));
    if (c.error) main.append(alert(c.error));
    if (c.note) main.append(h("div", "set-note set-line", c.note));
    const acts = h("div", "set-form-acts");
    const running = !c.report && !c.error;
    const again = button("set-rm", running ? "Checking…" : "Check again", () => void runCheck(c.id));
    again.disabled = running;
    acts.append(again, h("span", "set-fill"));
    if (c.report?.ok && x && shownHost() !== x.id) acts.append(button("set-add ready", `Switch to ${name}`, () => pick(x.id)));
    acts.append(button("set-rm", "Done", () => ((check = null), redraw())));
    main.append(acts);
    return main;
  }

  /** One fix, after its confirm, then the check again. */
  async function runFix(c: Check, x: KnownHost, fix: Fix) {
    const again = () => void runCheck(x.id);
    c.note = undefined;
    try {
      if (fix === "trust_key" || fix === "review_key") return await trustKey(x, again);
      if (fix === "make_key") return makeKey(x, again);
      if (fix === "unlock_key") return secretDialog(x, "unlock", again);
      if (fix === "install_key") return secretDialog(x, "install", again);
      if (fix === "fix_permissions") return secretDialog(x, "fix", again);
      if (fix === "install_skiff") return installSkiff(c, x);
      if (fix === "start_skiff") {
        c.note = `Starting Skiff on ${x.name}…`;
        redraw();
        await invoke("start_skiff", { id: x.id });
        return again();
      }
      if (fix === "turn_on_linger") return turnOnLinger(x, again);
      if (fix === "update_skiff") return updateSkiff(c, x);
      if (fix === "copy_public_key") {
        await copyText(await invoke<string>("public_key", { id: x.id }));
        c.note = `Public key copied. Send it to the admin of ${x.name}, then check again.`;
        redraw();
      }
    } catch (e) {
      c.error = String(e);
      redraw();
    }
  }

  /** One confirm covers the install, the start and keeping it running.
   *  The dialog stays open while it runs and shows each step. */
  function installSkiff(c: Check, x: KnownHost) {
    const [os] = c.report?.platform ?? ["linux", ""];
    const v = c.report?.version ?? "";
    runDialog<{ version: string; cli_linked: boolean }>({
      id: x.id,
      title: `Install Skiff on ${x.name}?`,
      body:
        os === "macos"
          ? `Installs Skiff ${v} in your home folder on ${x.name}. It starts when you log in there.`
          : `Installs Skiff ${v} in your home folder on ${x.name}. It keeps running after a restart.`,
      action: "Install Skiff",
      runningTitle: `Installing Skiff on ${x.name}`,
      working: "Installing",
      failTitle: `Install failed on ${x.name}`,
      steps: runSteps(x, v, "Start skiffd and skiff-host"),
      tone: "go",
      run: () => invoke("install_skiff", { id: x.id }),
      done: (r, secs, output) => {
        const cli = r.cli_linked ? "" : " ~/.local/bin/skiff is taken, so Skiff left it.";
        void runCheck(x.id, { row: "skiff", text: `Installed Skiff ${r.version}.${cli}`, secs, output });
      },
      failed: (cause, output) => rowFailed(c, "install_skiff", cause, output),
    });
  }

  /** The six steps an install or update counts, before each runs. */
  const runSteps = (x: KnownHost, v: string, last: string) => [`Check ${x.name}`, `Get Skiff ${v}`, "Upload", "Unpack", `Switch to Skiff ${v}`, last];

  /** Close on a failed run: the check list row keeps the cause, with Try
   *  again and Copy output. */
  function rowFailed(c: Check, fix: Fix, cause: string, output: string) {
    if (check !== c) return;
    c.result = { row: "skiff", text: cause, failed: true, fix, output };
    redraw();
  }

  /** The Host's own setting: on, an older install Skiff made is updated as
   *  the app connects, when every session survives. */
  function autoUpdate(x: KnownHost): HTMLElement {
    const row = h("div", "set-row set-agent set-auto-update" + (x.auto_update ? " on" : ""));
    const sw = h("button", "c-on");
    sw.type = "button";
    sw.setAttribute("role", "switch");
    sw.setAttribute("aria-checked", String(x.auto_update));
    sw.setAttribute("aria-label", `Update Skiff here automatically ${x.auto_update ? "on" : "off"}`);
    sw.append(h("span", "switch"));
    const flip = async () => {
      sw.disabled = true;
      try {
        await invoke("set_host_auto_update", { id: x.id, on: !x.auto_update });
        await loadHosts();
        redraw();
      } catch (e) {
        sw.disabled = false;
        alertCheck(e);
      }
    };
    sw.addEventListener("click", () => void flip());
    const label = h("span", "c-text", "Update Skiff here automatically");
    label.addEventListener("click", () => void flip());
    row.append(sw, label);
    return row;
  }

  /** Update Skiff…: one confirm, open while it runs with each step. When
   *  skiffd cannot keep its sessions, the update stops, and a red dialog
   *  names the sessions it would end. */
  function updateSkiff(c: Check, x: KnownHost, reinstall = false) {
    updateDialog(c, x, {
      title: `Update Skiff on ${x.name}?`,
      body: `Updates Skiff on ${x.name} to ${c.report?.version ?? ""}${reinstall || c.report?.other_build ? ", this app's build" : ""}. Its sessions keep running.`,
      action: "Update Skiff",
      tone: "go",
      endSessions: false,
      reinstall,
    });
  }

  function updateDialog(
    c: Check,
    x: KnownHost,
    o: { title: string; body: string; action: string; tone: "go" | "danger"; endSessions: boolean; reinstall: boolean },
  ) {
    const v = c.report?.version ?? "";
    runDialog<Updated>({
      id: x.id,
      ...o,
      runningTitle: `Updating Skiff on ${x.name}`,
      working: "Updating",
      failTitle: `Update failed on ${x.name}`,
      steps: runSteps(x, v, "Start skiff-host"),
      run: () => invoke<Updated>("update_skiff", { id: x.id, endSessions: o.endSessions, reinstall: o.reinstall }),
      done: (r, secs, output) => {
        if (r.result === "no_handover") return endsSessions(c, x, r.from, r.sessions, o.reinstall);
        const text =
          r.result === "done"
            ? `Updated to Skiff ${r.version}.${r.restarted ? " skiffd restarted." : ""}`
            : r.result === "newer"
              ? `${x.name} runs a newer Skiff, ${r.from}.`
              : `Skiff ${r.version} runs already.`;
        void runCheck(x.id, { row: "skiff", text, secs, output });
      },
      failed: (cause, output) => rowFailed(c, "update_skiff", cause, output),
    });
  }

  /** The red dialog: this update ends the sessions it names. */
  function endsSessions(c: Check, x: KnownHost, from: string, sessions: string[], reinstall: boolean) {
    const n = sessions.length;
    updateDialog(c, x, {
      title: `Update Skiff on ${x.name} and end ${n === 1 ? "1 session" : `${n} sessions`}?`,
      body: `Skiff ${from} cannot hand its sessions over. This ends: ${sessions.join(", ")}. Save their work first.`,
      action: n === 1 ? "Update and end 1 session" : `Update and end ${n} sessions`,
      tone: "danger",
      endSessions: true,
      reinstall,
    });
  }

  function turnOnLinger(x: KnownHost, done: () => void) {
    actionDialog({
      title: `Keep Skiff running on ${x.name} after you log out?`,
      body: `Turns on lingering for your account. Skiff then keeps running after you log out.`,
      action: "Turn on",
      tone: "go",
      submit: async () => {
        await invoke<boolean>("turn_on_linger", { id: x.id });
        done();
      },
    });
  }

  /** Trust a new host key, or replace a changed one: the fingerprint first. */
  async function trustKey(x: KnownHost, done: () => void) {
    const key = await invoke<HostKey>("host_key_scan", { id: x.id });
    const prints = (title: string, fs: Fingerprint[], old = false) => {
      const box = h("div", "confirm-body set-prints" + (old ? " old" : ""));
      box.append(h("div", "set-prints-h", title), ...fs.map((f) => h("div", "mono", `${f.kind}  ${f.sha256}`)));
      return box;
    };
    if (!key.known.length) {
      return actionDialog({
        title: `Trust the host key of ${x.name}?`,
        body: `First connection to ${x.name}. Compare the fingerprint with the one from your provider, if you have it.`,
        content: [prints("Fingerprint", key.fingerprints)],
        action: "Trust this key",
        tone: "go",
        submit: async () => {
          await invoke("host_key_trust", { token: key.token, replace: false });
          done();
        },
      });
    }
    actionDialog({
      title: `The host key of ${x.name} changed`,
      body: `Normal after a server reinstall. It can also mean another machine answers here.`,
      content: [prints("Key that Skiff knows", key.known, true), prints("Key that answers now", key.fingerprints)],
      action: "Replace the key…",
      tone: "danger",
      submit: async () => {
        // Asked a second time, as a changed key can be an attack.
        setTimeout(async () => {
          const ok = await confirmAction({
            title: `Replace the key of ${x.name}?`,
            body: `Skiff replaces the old key in ${key.file}. Do this only if you know why it changed.`,
            action: "Replace the key",
          });
          if (!ok) return;
          await invoke("host_key_trust", { token: key.token, replace: true }).catch((e) => alertCheck(e));
          done();
        });
      },
    });
  }

  function alertCheck(e: unknown) {
    if (check) check.error = String(e);
    redraw();
  }

  function makeKey(x: KnownHost, done: () => void) {
    actionDialog({
      title: "Make an SSH key on this computer?",
      body: `Makes ~/.ssh/id_ed25519 with no passphrase, then installs it on ${x.name}.`,
      action: "Make key",
      tone: "go",
      submit: async () => {
        await invoke("make_key");
        // The new key goes to the server next, with one password.
        setTimeout(() => secretDialog(x, "install", done));
      },
    });
  }

  /** A dialog with one secret field. The field is cleared when it closes. */
  function secretDialog(x: KnownHost, what: "install" | "unlock" | "fix", done: () => void) {
    const input = h("input", "prompt-input");
    input.type = "password";
    input.autocomplete = "off";
    input.spellcheck = false;
    const user = x.user ?? (x.destination.includes("@") ? x.destination.split("@")[0] : "");
    const who = user ? `${user} on ${x.name}` : `your account on ${x.name}`;
    const copy = {
      install: {
        title: `Log in to ${x.name} once`,
        body: `Type the password of ${who}. Skiff uses it once and does not store it.`,
        label: "Password",
        action: "Install key",
        cmd: "install_key",
      },
      unlock: {
        title: "Unlock your SSH key",
        body: "Type the passphrase of your SSH key. Skiff does not store it.",
        label: "Passphrase",
        action: "Unlock",
        cmd: "unlock_key",
      },
      fix: {
        title: `Replace the key file on ${x.name}?`,
        body: `Another account owns your key file on ${x.name}. Skiff writes a new one with only this computer's key. The other keys stop working. Type the password of ${who}.`,
        label: "Password",
        action: "Replace the key file",
        cmd: "fix_permissions",
      },
    }[what];
    input.placeholder = copy.label;
    input.setAttribute("aria-label", copy.label);
    actionDialog({
      title: copy.title,
      body: copy.body,
      content: [input],
      action: copy.action,
      tone: what === "fix" ? "danger" : "go",
      focus: input,
      onClose: () => (input.value = ""),
      submit: async () => {
        if (!input.value) {
          input.focus();
          throw `Type the ${copy.label.toLowerCase()}.`;
        }
        const secret = input.value;
        input.value = "";
        const args = what === "unlock" ? { id: x.id, passphrase: secret } : { id: x.id, password: secret };
        try {
          await invoke(copy.cmd, args);
        } catch (e) {
          input.focus();
          throw String(e);
        }
        done();
      },
    });
  }

  function rename(x: KnownHost) {
    promptAction({
      title: `Rename ${x.name}`,
      body: "The server stays the same.",
      placeholder: "a name",
      value: x.name,
      action: "Rename",
      submit: async (name) => {
        await invoke("rename_host", { id: x.id, name });
        await loadHosts();
        redraw();
      },
    });
  }

  /**
   * Remove forgets the Host here. When Skiff installed Skiff there, a box
   * also stops and uninstalls it. The box shows only after the server says
   * so, and the copy names the sessions that end and what stays.
   */
  function remove(x: KnownHost) {
    const kept = `Its sessions keep running on ${x.name}.`;
    const body = h("div", "confirm-body", kept);
    const extra = h("div", "set-remove-box");
    const status = h("div", "set-sub", `Checking ${x.name}…`);
    extra.append(status);
    let uninstall = false;
    let ends = "";
    const box = h("button", "set-check-label");
    box.type = "button";
    box.setAttribute("role", "switch");
    box.setAttribute("aria-checked", "false");
    box.addEventListener("click", () => {
      uninstall = !uninstall;
      box.setAttribute("aria-checked", String(uninstall));
      box.classList.toggle("on", uninstall);
      body.textContent = uninstall
        ? `Skiff uninstalls itself from ${x.name}. ${ends}Your other files stay.`
        : kept;
    });
    actionDialog({
      title: `Remove ${x.name}?`,
      content: [body, extra],
      action: "Remove Host",
      tone: "danger",
      submit: async () => {
        error = null;
        await invoke("remove_host", { id: x.id, uninstall });
        await loadHosts().catch(() => {});
        redraw();
      },
    });
    invoke<{ managed: boolean; sessions: string[] }>("uninstall_preview", { id: x.id }).then(
      (p) => {
        if (!p.managed) return void (status.textContent = `Skiff on ${x.name} is its own install. It stays.`);
        const n = p.sessions.length;
        ends = n === 0 ? "" : `This ends: ${p.sessions.join(", ")}. `;
        box.append(h("span", "switch"), h("span", "", `Also uninstall Skiff on ${x.name}. ${ends}`.trim()));
        status.replaceWith(box);
      },
      () => void (status.textContent = `${x.name} does not answer. Skiff removes it here only.`),
    );
  }

  function drawForm(f: Form): HTMLElement {
    const main = h("div", "set-main");
    const head = h("div", "set-head");
    const back = button("set-back", "‹ Hosts", () => {
      form = null;
      redraw();
    });
    const editing = f.id ? knownHosts().find((x) => x.id === f.id) : null;
    head.append(
      back,
      h("div", "set-h", editing ? `Edit ${editing.name}` : "Add SSH Host"),
    );
    main.append(head);

    const list = h("datalist");
    list.id = "set-ssh-aliases";
    for (const a of aliases) {
      const o = h("option");
      o.value = a.name;
      if (a.goes_to) o.label = a.goes_to;
      list.append(o);
    }
    const dest = field(f, "destination", "Server", "devbox, devbox.lan or 10.0.0.5");
    dest.setAttribute("list", list.id);
    dest.addEventListener("input", () => lookup(f));
    main.append(fieldRow("Server", dest, list));
    const said = h("div", "set-note set-line set-resolve" + (line.bad ? " bad" : ""), line.text);
    said.id = "set-resolve";
    main.append(said);

    const toggle = button("set-rm", f.more ? "Less" : "More", () => {
      f.more = !f.more;
      redraw();
    });
    toggle.setAttribute("aria-expanded", String(f.more));
    if (f.more) {
      const r = line.for === key(f) ? lastResolved : null;
      const name = field(f, "name", "Name", defaultName(f.draft.destination.trim()) || "the server's name");
      const user = field(f, "user", "User", r?.user || "from SSH config");
      const port = field(f, "port", "Port", r ? String(r.port) : "from SSH config");
      port.inputMode = "numeric";
      user.addEventListener("input", () => lookup(f));
      port.addEventListener("input", () => lookup(f));
      main.append(fieldRow("Name", name), fieldRow("User", user), fieldRow("Port", port));
    }

    const acts = h("div", "set-form-acts");
    const save = button("set-add ready", f.busy ? "…" : "Connect", () => void submit(f));
    save.disabled = f.busy;
    acts.append(toggle, h("span", "set-fill"), button("set-rm", "Cancel", () => ((form = null), redraw())));
    // Only a new name needs no check.
    if (editing && onlyName(f, editing)) acts.append(button("set-rm", "Save", () => void submit(f, false)));
    acts.append(save);
    main.append(acts);
    if (f.error) main.append(alert(f.error));
    if (!line.for && f.draft.destination.trim()) lookup(f);
    return main;
  }

  let lastResolved: Resolved | null = null;
  const key = (f: Form) => `${f.draft.destination.trim()}\n${f.draft.user.trim()}\n${f.draft.port.trim()}`;

  /** The resolve line follows the fields, 250 ms after the last key. */
  function lookup(f: Form) {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      const k = key(f);
      if (!f.draft.destination.trim()) {
        line = { for: "", text: "", bad: false };
        return paint();
      }
      try {
        const r = await resolve(f.draft.destination.trim(), f.draft.user.trim(), f.draft.port.trim());
        if (k !== key(f)) return;
        lastResolved = r;
        const jump = r.jump ? `, through ${r.jump}` : "";
        line = { for: k, text: `${r.user}@${r.hostname}, port ${r.port}${jump}.`, bad: false };
      } catch (e) {
        if (k !== key(f)) return;
        line = { for: k, text: String(e), bad: true };
      }
      paint();
    }, 250);
  }

  /** The line alone: a redraw would move the caret. */
  function paint() {
    const el = document.getElementById("set-resolve");
    if (!el) return;
    el.textContent = line.text || "User and port come from your SSH config.";
    el.classList.toggle("bad", line.bad);
  }

  const onlyName = (f: Form, x: KnownHost) => {
    const d = f.draft;
    return d.destination.trim() === x.destination && d.user.trim() === (x.user ?? "") && d.port.trim() === (x.port ? String(x.port) : "");
  };

  /** Saves the form. Connect then runs the check list: it finds the socket. */
  async function submit(f: Form, connect = true) {
    if (f.busy) return;
    f.error = null;
    f.busy = true;
    redraw();
    let id = f.id;
    // Save is for a new name only: any other change checks again.
    const before = f.id ? knownHosts().find((k) => k.id === f.id) : undefined;
    connect ||= !before || !onlyName(f, before);
    try {
      if (id) await invoke("edit_host", { id, form: f.draft });
      else id = await invoke<string>("add_host", { form: f.draft });
      await loadHosts();
      await load();
      form = null;
    } catch (e) {
      f.error = String(e);
      id = null;
    } finally {
      f.busy = false;
    }
    if (id && connect) return void runCheck(id);
    redraw();
  }

  function field(f: Form, k: keyof Draft, label: string, placeholder: string): HTMLInputElement {
    const input = h("input", "c-cmd mono");
    input.type = "text";
    input.spellcheck = false;
    input.autocomplete = "off";
    input.value = f.draft[k];
    input.placeholder = placeholder;
    input.setAttribute("aria-label", label);
    input.addEventListener("input", () => (f.draft[k] = input.value));
    input.addEventListener("keydown", (e) => {
      if (e.key !== "Enter") return;
      e.preventDefault();
      void submit(f);
    });
    return input;
  }

  function fieldRow(label: string, ...content: HTMLElement[]): HTMLElement {
    const row = h("div", "set-row set-open set-host-field");
    row.append(h("span", "c-name", label), ...content);
    return row;
  }

  function focusServer() {
    document.querySelector<HTMLInputElement>('#settings input[aria-label="Server"]')?.focus();
  }

  function alert(text: string): HTMLElement {
    const err = h("div", "set-error set-line", text);
    err.setAttribute("role", "alert");
    return err;
  }

  return { load, open, draw, focusServer, get editing() { return form !== null; } };
}
