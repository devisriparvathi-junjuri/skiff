import { open as openFolder } from "@tauri-apps/plugin-dialog";
import { isMac } from "../app/keys";
import { hostInvoke } from "../app/hostCaps";
import { hostName, isRemote } from "../app/hosts";
import { openFolderPicker } from "../workspace/folderPicker";
import { skiffAgentName } from "../app/skiffAgent";
import { reveal } from "../platform/fileActions";
import { confirmAction } from "../ui/confirm";
import { dialogFrame } from "../ui/dialogParts";
import { h } from "../ui/dom";
import { editField, fieldEditKey, textField } from "../ui/fieldClipboard";
import { type MessageKey, t, tn } from "../i18n";

/** A folder's contents, as `agent_folder::Counts` sends them. */
export interface FolderCounts {
  tasks: number;
  open: number;
  done: number;
  results: number;
  bytes: number;
  agents_edited: boolean;
  other: string[];
}

type Kind = "current" | "new" | "empty" | "skiff" | "refused";
type Mode = "copy" | "empty" | "switch";

/** `preview_skiff_folder`: what a change to the chosen folder would do. */
interface Preview {
  from: string;
  to: string;
  kind: Kind;
  refused: string | null;
  can_copy: boolean;
  copy_refused: string | null;
  from_counts: FolderCounts | null;
  to_counts: FolderCounts | null;
  moved: string | null;
  running: { uid: string; task: string; pane_id: string }[];
  ledger_env: string | null;
  skipper_runs: boolean;
}

/** `change_skiff_folder`: what the change did. */
interface Done {
  from: string;
  to: string;
  mode: Mode;
  copied: FolderCounts | null;
  moved_note: string | null;
  warning: string | null;
}

const KIND: Record<Kind, MessageKey> = {
  current: "folder.kind.current",
  new: "folder.kind.new",
  empty: "folder.kind.empty",
  skiff: "folder.kind.skiff",
  refused: "folder.kind.refused",
};

/** Bytes, as the Rust side shows them. */
export function size(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/** "3 tasks, 2 open · 3 results, 1.2 KB". */
export function countsLine(c: FolderCounts): string {
  return t("folder.counts", { tasks: tn("folder.tasks", c.tasks), open: c.open, results: tn("folder.results", c.results), size: size(c.bytes) });
}

/** The "Skiff will" line for a preview. */
function verdict(p: Preview): string {
  switch (p.kind) {
    case "current":
      return t("folder.verdict.current", { name: skiffAgentName() });
    case "refused":
      return p.refused ?? t("folder.verdict.refused");
    case "skiff":
      return t("folder.verdict.skiff");
    default:
      return t(p.can_copy ? "folder.verdict.copy" : "folder.verdict.new");
  }
}

/** The preview's sections, one paragraph each. */
function details(p: Preview): string[] {
  const out = [t("folder.fromTo", { from: p.from, to: p.to })];
  if (p.kind === "new" || p.kind === "empty") {
    const c = p.from_counts;
    if (p.can_copy && c) {
      const other = c.other.length ? t("folder.yourFiles", { files: c.other.join(", ") }) : "";
      out.push(
        t("folder.copyDetails", {
          edited: c.agents_edited ? t("folder.edited") : "",
          tasks: tn("folder.tasks", c.tasks), open: c.open, done: c.done,
          files: tn("disk.files", c.results), size: size(c.bytes), other,
        }),
        t("folder.setupLinks"),
        t("folder.oldStays"),
      );
    }
    out.push(t("folder.startEmpty"));
    if (p.copy_refused) out.push(t("folder.copyOff", { why: p.copy_refused }));
  }
  if (p.kind === "skiff" && p.to_counts) {
    const now = p.from_counts;
    out.push(t("folder.thisFolder", { counts: countsLine(p.to_counts) }) + (now ? `\n${t("folder.currentFolder", { counts: countsLine(now) })}` : ""));
    if (now?.open) out.push(tn("folder.openStay", now.open));
    if (p.moved) out.push(t("folder.movedBefore", { name: skiffAgentName() }));
  }
  if (p.kind !== "current" && p.kind !== "refused") {
    out.push(t("folder.conversation", { name: skiffAgentName() }));
    if (p.skipper_runs) out.push(t("folder.runsIn", { name: skiffAgentName(), from: p.from }));
    if (p.ledger_env) out.push(t("folder.ledgerEnv", { path: p.ledger_env }));
  }
  return out;
}

/**
 * Settings › Skippers › Folder › Change…: the person picks where, and the
 * folder there decides what Skiff offers. Nothing changes before the action.
 * `current` is the folder now; the picker opens beside it. `changed` runs
 * after a change, so Settings reads the folder again. The keys
 * go back to what opened the dialog. On a remote Host the Host reads and
 * changes the folder, and Choose… opens the folder picker on that Host:
 * there is no path to type.
 */
export function changeSkiffFolder(o: { current: string; changed: () => void }) {
  const back = document.activeElement as HTMLElement | null;
  const remote = isRemote();
  // The selected Host reads and changes the folder: This Mac itself, a remote Host through its skiff-host.
  const call = <T>(cmd: string, args: Record<string, unknown>) => hostInvoke<T>("skipper_config", cmd, args);
  const { overlay, panel } = dialogFrame("confirm-panel folder-panel", "dialog", t("folder.title"));
  overlay.appendChild(panel);
  document.body.appendChild(overlay);

  let preview: Preview | null = null;
  let busy = false;
  let open = true;

  const close = () => {
    if (!open || busy) return;
    open = false;
    overlay.remove();
    back?.focus?.();
  };

  const title = h("div", "confirm-title", t("folder.title"));
  const choices = h("div", "folder-choices");
  const typed = h("input", "prompt-input");
  typed.type = "text";
  typed.spellcheck = false;
  typed.placeholder = t("folder.typePath");
  typed.setAttribute("aria-label", t("folder.pathAria"));
  const body = h("div", "folder-preview");
  const error = h("div", "prompt-error");
  error.setAttribute("role", "alert");
  const foot = h("div", "confirm-foot");

  const pick = (label: string, path: string, hint: string) => {
    const b = h("button", "folder-choice");
    b.type = "button";
    b.append(h("span", "folder-choice-name mono", label), h("span", "folder-choice-hint", hint));
    b.addEventListener("click", () => void show(path));
    return b;
  };
  const chooser = h("button", "folder-choice");
  chooser.type = "button";
  chooser.append(
    h("span", "folder-choice-name", t("folder.choose")),
    h("span", "folder-choice-hint", remote ? t("folder.pickerRemote", { host: hostName() }) : t(isMac ? "folder.pickerMac" : "folder.picker")),
  );
  chooser.addEventListener("click", async () => {
    if (remote) return openFolderPicker({ action: t("folder.useThis"), pick: (path) => show(path) });
    const dir = await openFolder({ directory: true, title: t("folder.chooseTitle"), defaultPath: o.current.replace(/\/[^/]+\/?$/, "") || undefined }).catch(() => null);
    if (typeof dir === "string") await show(dir);
  });
  choices.append(
    pick("~/skiff-agent", "~/skiff-agent", t("folder.visible")),
    pick("~/.skiff-agent", "~/.skiff-agent", t("folder.hidden")),
    chooser,
  );
  // A typed, pasted or cut path is a new choice: the preview and its actions go
  // until Enter reads the folder there. An action never runs on a path the field no longer shows.
  typed.addEventListener("input", () => {
    if (!preview || busy) return;
    preview = null;
    error.textContent = "";
    draw();
  });
  typed.addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    e.preventDefault();
    e.stopPropagation();
    if (typed.value.trim()) void show(typed.value);
  });

  /** Reads the chosen folder and draws what Skiff would do. */
  async function show(path: string) {
    if (busy) return;
    error.textContent = "";
    try {
      preview = await call<Preview>("preview_skiff_folder", { path });
    } catch (e) {
      preview = null;
      error.textContent = String(e);
    }
    draw();
  }

  function draw() {
    body.replaceChildren();
    foot.replaceChildren();
    const cancel = h("button", "confirm-cancel", t(preview ? "folder.cancel" : "folder.close"));
    cancel.type = "button";
    cancel.addEventListener("click", close);
    foot.append(cancel);
    if (!preview) return;
    const p = preview;
    const head = h("div", "folder-resolved");
    head.append(h("span", "folder-kind" + (p.kind === "refused" ? " bad" : ""), t(KIND[p.kind])), h("span", "mono", p.to));
    body.append(head, h("div", "folder-verdict" + (p.kind === "refused" ? " bad" : ""), verdict(p)));
    const text = h("div", "confirm-body", details(p).join("\n\n"));
    body.append(text);
    if (p.kind === "current" || p.kind === "refused") return;

    // Open tasks whose pane still runs report to the old ledger. Wait by default.
    const running = p.running.length;
    if (running) {
      const list = p.running.map((r) => `${r.uid}  ${r.task}`).join("\n");
      body.append(h("div", "folder-wait", `${tn("folder.stillRun", running)}\n${list}`));
    }
    const act = (label: string, mode: Mode, primary: boolean) => {
      const b = h("button", primary ? "confirm-act go" : "confirm-cancel", label);
      b.type = "button";
      b.disabled = !!running;
      b.dataset.mode = mode;
      b.addEventListener("click", () => void run(mode, false));
      return b;
    };
    const acts: HTMLButtonElement[] = [];
    if (p.kind === "skiff") acts.push(act(t("folder.switch"), "switch", true));
    else {
      acts.push(act(t("folder.empty"), "empty", !p.can_copy || !!p.copy_refused));
      if (p.can_copy && !p.copy_refused) acts.push(act(t("folder.copy"), "copy", true));
    }
    if (running) {
      const primary = acts[acts.length - 1];
      const anyway = h("button", "confirm-cancel", t("folder.anywayAsk"));
      anyway.type = "button";
      anyway.addEventListener("click", async () => {
        const ok = await confirmAction({
          title: t("folder.anywayTitle"),
          body: tn("folder.anywayBody", running, { from: p.from, name: skiffAgentName() }),
          action: t("folder.anyway"),
        });
        if (ok) void run(primary.dataset.mode as Mode, true);
      });
      foot.append(anyway);
    }
    foot.append(...acts);
  }

  /** Runs the change. A failure says that nothing changed and keeps the dialog. */
  async function run(mode: Mode, anyway: boolean) {
    if (!preview || busy) return;
    busy = true;
    error.textContent = "";
    for (const b of foot.querySelectorAll("button")) b.disabled = true;
    const lit = foot.querySelector<HTMLButtonElement>(`[data-mode="${mode}"]`);
    if (lit) lit.textContent = "…";
    try {
      const done = await call<Done>("change_skiff_folder", { path: preview.to, mode, endSkipper: preview.skipper_runs, anyway });
      busy = false;
      o.changed();
      finished(done);
    } catch (e) {
      busy = false;
      // Read the folders again, so Try again offers what is valid now.
      const from = preview.from;
      await show(preview.to);
      error.textContent = `${t("folder.stopped", { name: skiffAgentName(), from })}\n${String(e)}`;
    }
  }

  /** The done step: where the main Skipper is now, what was copied, what stays. */
  function finished(d: Done) {
    title.textContent = t("folder.nowUses", { name: skiffAgentName(), to: d.to });
    choices.remove();
    typed.remove();
    const lines: string[] = [];
    if (d.copied) lines.push(t("folder.copied", { counts: countsLine(d.copied) }));
    if (d.mode === "empty") lines.push(t("folder.doneEmpty"));
    if (d.mode === "switch") lines.push(t("folder.doneSwitch"));
    if (d.moved_note) lines.push(t("folder.doneOldStays"));
    lines.push(t("folder.doneStarts", { name: skiffAgentName() }));
    lines.push(t("folder.goBack", { from: d.from }));
    if (d.warning) lines.push(d.warning);
    body.replaceChildren(h("div", "confirm-body", lines.join("\n\n")));
    foot.replaceChildren();
    // A remote Host's old folder is on that Host: no file manager here opens it.
    if (d.mode !== "switch" && !remote) {
      const old = h("button", "confirm-cancel", t("folder.showOld"));
      old.type = "button";
      old.addEventListener("click", () => reveal(d.from));
      foot.append(old);
    }
    const ok = h("button", "confirm-act go", t("folder.done"));
    ok.type = "button";
    ok.addEventListener("click", close);
    foot.append(ok);
    ok.focus();
  }

  panel.append(title, choices, ...(remote ? [] : [typed]), body, error, foot);
  draw();

  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });
  overlay.addEventListener("keydown", (e) => {
    e.stopPropagation();
    // ⌘X and ⌘A in the path field on macOS, as Settings does them. Copy and paste come through the menu.
    const edit = fieldEditKey(e, isMac);
    const field = edit && textField(e.target as Element);
    if (edit && field) {
      e.preventDefault();
      return void editField(field, edit);
    }
    if (e.key === "Escape") close();
    else if (e.key === "Enter") {
      // Enter runs the lit action, never Cancel and never a disabled one.
      const at = document.activeElement as HTMLElement | null;
      if (at instanceof HTMLButtonElement) return;
      foot.querySelector<HTMLButtonElement>(".confirm-act:not(:disabled)")?.click();
    } else if (e.key === "Tab") {
      const stops = [...panel.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)")];
      const i = stops.indexOf(document.activeElement as HTMLElement);
      stops[(i + (e.shiftKey ? -1 : 1) + stops.length) % stops.length]?.focus();
    } else return;
    e.preventDefault();
  });
  choices.querySelector<HTMLButtonElement>("button")?.focus();
}
