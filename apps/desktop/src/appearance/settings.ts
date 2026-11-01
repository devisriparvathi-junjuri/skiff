import { DEFAULT_THEME } from "./colors";
import { escapeButton } from "../ui/dialogParts";
import { h } from "../ui/dom";
import { invoke } from "@tauri-apps/api/core";
import type { AgentInfo, Appearance, TerminalTheme } from "../platform/types";
import { catalogFor, language, languageName, languages, locale, type MessageKey, systemLanguage, t, upper, wordCount } from "../i18n";
import { agentNameList, setAgentNameList } from "./agentNames";
import { nextCard, themeGrid, type ThemeFilters } from "./themeCards";
import { type Action, actionFor, isMac } from "../app/keys";
import { editField, fieldEditKey, textField } from "../ui/fieldClipboard";
import { mainName, type InstructionFile, type SkiffAgentSettings, type TaskType } from "../app/skiffAgent";
import { avatarsSection, instructionsSection, type SkiffAgentEdit, projectSkippersSection, skiffAgentSection, taskTypesSection } from "./skiffAgentSettings";
import { peekText } from "../terminal/peek";
import { fileSize } from "./taskTypes";
import { confirmAction } from "../ui/confirm";
import { changeSkiffFolder } from "./skiffFolderDialog";
import { rowTable, sectionHead, tableState, tableWords } from "./rowTable";
import { type HostsPage, hostName, isRemote } from "../app/hosts";
import { createHostsTab } from "./hostSettings";
import { hostCan, hostInvoke } from "../app/hostCaps";

import { countDisk, diskState, diskUseSection } from "./diskUse";
import { imagesSection, imagesState } from "./imagesPage";
import { onImagesChanged } from "../terminal/images";
import { activityState, daemonActivitySection, startActivity, stopActivity } from "./daemonActivity";
import { revealSession } from "../workspace/view";
import { locate } from "../workspace/model";
import { openClosedProject } from "../workspace/projectClose";
import { S } from "../app/state";

/**
 * Settings over the sidebar and terminal: agents, Skippers, task types,
 * appearance, language, Open with, images, disk use and Hosts.
 */
export interface AppearanceHooks {
  themes: () => TerminalTheme[];
  /** A theme id, or null for the default Foot theme. */
  current: () => string | null;
  load: () => Promise<void>;
  set: (id: string | null) => void;
  /** Themes that replace the app theme on screen, each with a label that names its scope. */
  overrides?: () => { id: string; label: string }[];
}

export function createSettings(onAgents: (a: AgentInfo[]) => void, onClose: () => void, look: AppearanceHooks, onShowChange: () => void = () => {}) {
  const root = h("div");
  root.id = "settings";
  root.hidden = true;
  root.setAttribute("role", "dialog");
  root.setAttribute("aria-label", t("cmd.settings"));
  document.getElementById("body")!.appendChild(root);

  let agents: AgentInfo[] = [];
  const TABS = ["agents", "skippers", "task-types", "appearance", "language", "open", "images", "disk", "activity", "hosts"] as const;
  let tab: (typeof TABS)[number] = "agents";
  const TAB_NAMES: Record<(typeof TABS)[number], MessageKey> = {
    agents: "settings.tab.agents", skippers: "settings.tab.skippers", "task-types": "settings.tab.taskTypes", images: "settings.tab.images",
    disk: "settings.tab.disk", activity: "settings.tab.activity", appearance: "settings.tab.appearance", language: "settings.tab.language", open: "settings.tab.open", hosts: "settings.tab.hosts",
  };
  const themeFilters: ThemeFilters = { query: "", family: null };
  type OpenKey = "diff" | "text" | "markdown" | "html" | "image";
  type OpenSettings = Record<OpenKey, string> & { default_diff: string; peek: string[] };
  let openSet: OpenSettings | null = null;
  type MenuLayout = { available: boolean; chosen: string | null; auto: string; current: string };
  let menuSet: MenuLayout | null = null;
  /** `[appearance] language`: a code, or null for the system language. */
  let langSet: string | null = null;
  let agentSet: SkiffAgentSettings | null = null;
  // Settings › Skippers › Instructions, and one error line per row.
  let files: InstructionFile[] = [];
  const fileErrors = new Map<string, string>();
  /** Task types › Edit faults, by type. */
  const typeErrors = new Map<string, string>();
  let error: string | null = null;
  let saving = false;
  // The draft rows and their error lines survive every redraw.
  const agentsTable = tableState();
  const tasksTable = tableState();
  const hostsTab = createHostsTab(() => render());

  /** The agents, from the selected Host. A remote Host's come with the revision its next change is made from. */
  function loadAgents(): Promise<AgentInfo[]> {
    if (isRemote() && hostCan("agent_config")) return hostInvoke<AgentInfo[]>("agent_config", "agent_config");
    return hostInvoke<AgentInfo[]>("session_agents", "list_agents");
  }

  /** One agent change on the selected Host: This Mac's config, or a remote Host's through its skiff-host. */
  const agentCall = (cmd: string, args: Record<string, unknown> = {}) => hostInvoke<AgentInfo[]>("agent_config", cmd, args);
  const disk = diskState();
  const gallery = imagesState();
  const activity = activityState();
  // A drop, a paste or a delete elsewhere redraws the gallery.
  onImagesChanged(() => {
    if (!root.hidden && tab === "images") render();
  });

  async function toggle(id: string) {
    if (saving) return;
    const enabled = agents.filter((a) => (a.id === id ? !a.enabled : a.enabled)).map((a) => a.id);
    saving = true;
    error = null;
    // Show the change at once; the daemon's answer replaces it.
    agents = agents.map((a) => (a.id === id ? { ...a, enabled: !a.enabled } : a));
    render();
    try {
      agents = await agentCall("set_agents", { enabled });
      onAgents(agents);
    } catch (e) {
      error = String(e);
      agents = await loadAgents().catch(() => agents);
    } finally {
      saving = false;
      render();
    }
  }

  /** Sends an agent edit to the daemon and shows the list it answers with. */
  async function editAgents(cmd: string, args: Record<string, string> = {}) {
    error = null;
    try {
      agents = await agentCall(cmd, args);
      onAgents(agents);
    } catch (e) {
      error = String(e);
      agents = await loadAgents().catch(() => agents);
    }
    render();
  }

  /** An agent edit from its table. Answers with the error to show; the table redraws. */
  async function agentEdit(cmd: string, args: Record<string, string>): Promise<string | null> {
    try {
      agents = await agentCall(cmd, args);
      onAgents(agents);
    } catch (e) {
      // A conflict forgot the revision: read the Host's agents again.
      agents = await loadAgents().catch(() => agents);
      return String(e);
    }
    // A renamed agent takes its task types with it.
    agentSet = await hostInvoke<SkiffAgentSettings>("skipper", "get_skiff_agent").catch(() => agentSet);
    return null;
  }

  function render() {
    // A rebuild keeps the list where it was scrolled.
    const scroll = root.querySelector(".set-main")?.scrollTop ?? 0;
    // So does the focused field: a save on blur finishes after the click that moved focus to the next field.
    const a = document.activeElement;
    const field = a instanceof HTMLInputElement && root.contains(a) ? { label: a.getAttribute("aria-label"), at: a.selectionStart, to: a.selectionEnd } : null;
    // Disk use redraws under its buttons after a count, and skiffd activity under a sort head on a sort or
    // a refresh. The keys stay on the same button, else the first one on.
    const button = a instanceof HTMLButtonElement && a.closest(".set-main") && (tab === "disk" || a.classList.contains("set-sort") || a.classList.contains("set-sink")) ? a.dataset.key ?? a.textContent : null;
    // A selected skiffd session row keeps the keys too; an ended one hands them to the current row.
    const row = a instanceof HTMLElement && a.classList.contains("set-daemon-row") ? a.dataset.id ?? null : null;
    // A Disk use tree row keeps the keys on the current row: a move, an open or a count.
    const diskRow = a instanceof HTMLElement && a.classList.contains("set-disk-row") && tab === "disk";
    draw();
    if (row !== null) (root.querySelector<HTMLElement>(`.set-daemon-row[data-id="${CSS.escape(row)}"]`) ?? root.querySelector<HTMLElement>(`.set-daemon-row[tabindex="0"]`))?.focus();
    if (diskRow) root.querySelector<HTMLElement>(`.set-disk-row[tabindex="0"]`)?.focus();
    if (button !== null) {
      const stops = [...root.querySelectorAll<HTMLButtonElement>(".set-main button:not(:disabled, .set-x)")];
      (stops.find((b) => (b.dataset.key ?? b.textContent) === button) ?? stops[0])?.focus();
    }
    const main = root.querySelector(".set-main");
    if (main) main.scrollTop = scroll;
    if (field?.label) {
      const next = [...root.querySelectorAll<HTMLInputElement>("input")].find((i) => i.getAttribute("aria-label") === field.label);
      next?.focus();
      if (next && field.at !== null && field.to !== null) next.setSelectionRange(field.at, field.to);
    }
  }

  function draw() {
    const nav = h("div", "set-nav");
    const head = h("div", "set-nav-head");
    head.append(h("div", "set-title", t("cmd.settings")), h("div", "set-path mono", "~/.config/skiff/projects.toml"));
    const tabs = TABS.map((id) => {
      const b = h("button", "set-tab" + (tab === id ? " active" : ""), t(TAB_NAMES[id]));
      b.type = "button";
      b.addEventListener("click", () => {
        if (tab === "activity") stopActivity(activity);
        tab = id;
        draw();
        if (tab === "skippers") void loadFiles();
        if (tab === "disk") void countDisk(disk, render);
        if (tab === "activity") startActivity(activity, render);
        if (tab === "appearance") root.querySelector<HTMLInputElement>(".tc-search")?.focus();
      });
      return b;
    });
    nav.append(head, ...tabs);
    if (tab === "appearance") {
      root.replaceChildren(nav, pinClose(appearance()));
      return;
    }
    if (tab === "language") {
      root.replaceChildren(nav, pinClose(languagePage()));
      return;
    }
    if (tab === "open") {
      root.replaceChildren(nav, pinClose(openWith()));
      return;
    }
    if (tab === "skippers") {
      root.replaceChildren(nav, pinClose(skippers()));
      return;
    }
    if (tab === "task-types") {
      const main = h("div", "set-main");
      // A Host whose Skiff is too old to take changes: its task types, read-only.
      if (isRemote() && !hostCan("skipper_config")) {
        main.append(h("div", "set-head"));
        main.firstElementChild!.append(h("div", "set-h", t("settings.tab.taskTypes")), h("div", "set-sub mono", hostName()));
        for (const task of agentSet?.tasks ?? []) main.append(h("div", "set-note mono", `${task.type}: ${task.command ?? task.agent}${task.error ? ` (${task.error})` : ""}`));
      } else if (agentSet) main.append(...taskTypesSection(agentSet, agents, tasksTable, saveTask, render, { edit: editTypeFile, errors: typeErrors }));
      root.replaceChildren(nav, pinClose(main));
      return;
    }
    if (tab === "hosts") {
      root.replaceChildren(nav, pinClose(hostsTab.draw()));
      return;
    }
    if (tab === "images") {
      root.replaceChildren(nav, pinClose(imagesSection(gallery, close)));
      return;
    }
    if (tab === "disk") {
      const main = h("div", "set-main");
      // A file opens as Settings › Open with says. In the peek, Settings
      // closes and opens again on this tab when the peek ends.
      const opener = {
        local: !isRemote(),
        peek: (script: string, cwd: string, name: string) => {
          close();
          peekText(script, cwd, `${cwd}/${name}`, () => void settings.open("disk"));
        },
      };
      main.append(...diskUseSection(disk, render, (days) => void setCacheDays(days), opener));
      root.replaceChildren(nav, pinClose(main));
      return;
    }
    if (tab === "activity") {
      const main = h("div", "set-main");
      main.append(...daemonActivitySection(activity, render, async (id, closed) => {
        if (closed) {
          const session = activity.list.find((x) => x.id === id);
          const at = session && locate(S.closedProjects, session.cwd);
          if (at) await openClosedProject(at.project);
        }
        close(); revealSession(id);
      }));
      root.replaceChildren(nav, pinClose(main));
      return;
    }

    const main = h("div", "set-main");
    const intro = h("div", "set-head");
    const title = h("div", "set-h", t("settings.tab.agents"));
    title.id = "set-h-agents";
    intro.append(title);
    // A Host whose Skiff is too old to take changes: its agents, read-only.
    if (isRemote() && !hostCan("agent_config")) {
      const rows = agents.map((a) => h("div", "set-note mono", `${a.id}: ${a.command} (${t(a.installed ? (a.enabled ? "settings.remote.on" : "settings.remote.off") : "settings.remote.notOnPath")})`));
      main.append(intro, h("div", "set-note", t("settings.remote.agentsOld", { host: hostName() })), ...(rows.length ? rows : [h("div", "set-note", t("settings.remote.noAgents"))]));
      if (error) main.appendChild(h("div", "set-error", error));
      root.replaceChildren(nav, pinClose(main));
      return;
    }
    // Only a removed default can come back.
    if (DEFAULT_AGENTS.some((d) => !agents.some((a) => a.id === d))) {
      intro.classList.add("set-head-row");
      const back = h("button", "set-rm", t("settings.restoreDefaults"));
      back.type = "button";
      back.addEventListener("click", () => void editAgents("restore_agents"));
      intro.appendChild(back);
    }
    main.append(intro, agentTable());
    if (error) {
      const err = h("div", "set-error", error);
      err.setAttribute("role", "alert");
      main.appendChild(err);
    }
    main.appendChild(callSigns());
    root.replaceChildren(nav, pinClose(main));
  }

  /** The agents Skiff ships with. Restore defaults brings back any the user removed. */
  const DEFAULT_AGENTS = ["claude", "codex", "gemini", "grok", "opencode", "hermes", "kimi"];

  /** One row per agent: its switch, name, command and status. The draft row adds a custom agent. */
  function agentTable(): HTMLElement {
    return rowTable<AgentInfo>(
      {
        grid: "g-agents",
        words: tableWords("agent"),
        labelledBy: "set-h-agents",
        endHead: upper(t("settings.colStatus")),
        rows: agents,
        id: (a) => a.id,
        rowClass: (a) => "set-agent" + (a.enabled ? " on" : "") + (a.installed ? "" : " missing"),
        cols: [
          {
            key: "on",
            head: "",
            name: t("settings.agents.on"),
            kind: "lead",
            draw: (a) => {
              const sw = h("button", "c-on");
              sw.type = "button";
              sw.disabled = !a.installed;
              sw.setAttribute("role", "switch");
              sw.setAttribute("aria-checked", String(a.enabled));
              sw.setAttribute("aria-label", t(a.enabled ? "settings.agents.isOn" : "settings.agents.isOff", { id: a.id }));
              sw.title = a.installed ? "" : t("settings.agents.notOnPath", { command: a.command.split(/\s+/)[0] });
              sw.appendChild(h("span", "switch"));
              sw.addEventListener("click", () => void toggle(a.id));
              return sw;
            },
          },
          { key: "name", head: upper(t("settings.agents.colAgent")), name: t("settings.agents.name"), kind: "key", required: true, get: (a) => a.id },
          {
            key: "command",
            head: upper(t("settings.agents.colCommand")),
            name: t("settings.agents.command"),
            field: t("settings.agents.commandField"),
            kind: "text",
            mono: true,
            required: true,
            // A known agent shows its default as the placeholder. A custom one has no default.
            placeholder: (a) => (a ? a.default_command : t("settings.agents.command")),
            get: (a) => (a.custom || a.command !== a.default_command ? a.command : ""),
          },
        ],
        status: (a) => {
          const status = h("span", "c-state");
          status.append(h("span", "sq"), t(a.installed ? "settings.agents.installed" : "settings.agents.missing"));
          return status;
        },
        // − takes any agent away that nothing starts: a default goes to `[agents] removed`. An empty command does not remove.
        // "Used by Admiral and 3 more" shows as "Admiral +3" in the STATUS cell.
        kept: (a) => a.used_by?.replace(/^Used by /, "").replace(/ and (\d+) more$/, " +$1") ?? null,
        checkEdit: (a, key, v) => (key === "command" && a.custom && !v ? t("settings.agents.needsCommand", { id: a.id }) : null),
        add: async (d) => {
          const err = await agentEdit("set_agent_command", { agent: d.name, command: d.command });
          // A skiffd from before custom agents saves the line but does not list it.
          if (err || agents.some((a) => a.id === d.name)) return err;
          return t("settings.agents.savedRestart", { name: d.name });
        },
        edit: (a, key, v) =>
          key === "name" ? agentEdit("rename_agent", { agent: a.id, name: v }) : agentEdit("set_agent_command", { agent: a.id, command: v }),
        remove: (a) => agentEdit("remove_agent", { agent: a.id }),
      },
      agentsTable,
      render,
    );
  }

  /** Esc in the top band of the page's head, which stays at the top as the page scrolls. A click closes. */
  function pinClose(main: HTMLElement): HTMLElement {
    (main.querySelector(":scope > .set-head") ?? main).append(escapeButton("set-x", t("settings.close"), close));
    return main;
  }

  /** The words that make up the names of new agent sessions. */
  function callSigns(): HTMLElement {
    const box = h("div", "set-names-box");
    box.appendChild(sectionHead(t("settings.agentNames")));
    const cols = h("div", "set-names-cols");
    for (const [list, label, aria] of [["ranks", t("settings.agentNames.titles"), t("settings.agentNames.titlesAria")], ["nouns", t("settings.agentNames.names"), t("settings.agentNames.namesAria")]] as const) {
      const col = h("div", "set-names-col");
      const area = h("textarea", "mono");
      area.spellcheck = false;
      area.value = agentNameList(list).join("\n");
      area.setAttribute("aria-label", aria);
      area.addEventListener("blur", () => {
        setAgentNameList(list, area.value.split("\n"));
        area.value = agentNameList(list).join("\n");
      });
      col.append(h("div", "set-names-label", upper(label)), area);
      cols.appendChild(col);
    }
    box.appendChild(cols);
    return box;
  }

  /** App colors made from a terminal theme. A click applies it. */
  function appearance(): HTMLElement {
    const main = h("div", "set-main");
    const intro = h("div", "set-head");
    intro.append(
      h("div", "set-h", t("settings.tab.appearance")),
      h("div", "set-sub", t("settings.appTheme")),
    );
    const cur = look.current() ?? null;
    const cards = h("div", "set-cards");
    cards.appendChild(
      themeGrid(look.themes(), {
        active: look.themes().some((t) => t.id === cur) ? cur : DEFAULT_THEME,
        filters: themeFilters,
        scopes: look.overrides?.(),
        reload: async () => { await look.load(); return look.themes(); },
        pick: (id) => {
          look.set(id === DEFAULT_THEME ? null : id);
        },
      }),
    );
    main.append(intro);
    if (menuSet?.available) main.appendChild(menuLayout(menuSet));
    main.appendChild(cards);
    return main;
  }

  /**
   * A row of choices that the window reads when it opens: Menu and Language.
   * `pending` shows "On restart" until the window opens again.
   */
  function segRow<V>(name: string, aria: string, choices: [V, string][], chosen: V, pending: boolean, pick: (v: V) => void): HTMLElement {
    const row = h("div", "set-row set-opacity set-menu");
    const seg = h("div", "c-cmd set-seg");
    seg.setAttribute("role", "radiogroup");
    seg.setAttribute("aria-label", aria);
    for (const [value, label] of choices) {
      const b = h("button", "set-seg-item" + (chosen === value ? " on" : ""), label);
      b.type = "button";
      b.setAttribute("role", "radio");
      b.setAttribute("aria-checked", String(chosen === value));
      b.addEventListener("click", () => pick(value));
      seg.appendChild(b);
    }
    row.append(h("span", "c-name", name), seg, h("span", "c-status", pending ? t("settings.onRestart") : ""));
    return row;
  }

  /** Linux: the GNOME header bar or the menu bar. GTK sets it when the window opens. */
  function menuLayout(m: MenuLayout): HTMLElement {
    const auto = t(m.auto === "header-bar" ? "settings.menuAutoHeader" : "settings.menuAutoMenu");
    const choices: [string | null, string][] = [[null, auto], ["header-bar", t("settings.headerBar")], ["menu-bar", t("settings.menuBar")]];
    const pending = (m.chosen ?? m.auto) !== m.current;
    return segRow(t("settings.menu"), t("settings.menuLayout"), choices, m.chosen, pending, (v) => void setMenuLayout(v));
  }

  /** The app's language, on its own page. */
  function languagePage(): HTMLElement {
    const main = h("div", "set-main");
    const intro = h("div", "set-head");
    intro.append(h("div", "set-h", t("settings.tab.language")), h("div", "set-sub", t("settings.appLanguage")));
    main.append(intro, languagePick());
    if (error) {
      const err = h("div", "set-error", error);
      err.setAttribute("role", "alert");
      main.appendChild(err);
    }
    return main;
  }

  /**
   * The app's language as a table: Auto, then each catalog. LANGUAGE | CODE |
   * WORDS | STATE. The window reads the choice when it opens. A set code
   * lights the catalog it picks: "es-MX" lights Español, "fr" lights English.
   */
  function languagePick(): HTMLElement {
    const table = h("div", "set-lang");
    table.setAttribute("role", "radiogroup");
    table.setAttribute("aria-label", t("settings.language"));
    const cols = h("div", "set-row set-cols");
    cols.append(...(["lang.colLanguage", "lang.colCode", "lang.colWords", "lang.colState"] as const).map((k, i) => h("span", i === 2 ? "c-num" : "", upper(t(k)))));
    table.append(cols);
    const chosen = langSet === null ? null : catalogFor(langSet);
    const choices: [string | null, string, string][] = [
      [null, t("settings.languageAuto", { name: languageName(systemLanguage()) }), navigator.language],
      ...languages().map((code): [string, string, string] => [code, languageName(code), code]),
    ];
    const n = (x: number) => x.toLocaleString(locale());
    for (const [value, name, code] of choices) {
      const on = chosen === value;
      // The catalog this row gives, and whether the window shows it now.
      const gives = value ?? systemLanguage();
      const shown = on && gives === language();
      const r = h("button", "set-row set-lang-row" + (on ? " cur" : ""));
      r.type = "button";
      r.dataset.key = `lang:${value ?? "auto"}`;
      r.setAttribute("role", "radio");
      r.setAttribute("aria-checked", String(on));
      const w = wordCount(gives);
      const state = h("span", "c-state");
      if (on) state.append(h("i", "lang-mk" + (shown ? "" : " pending")), h("span", "", t(shown ? "lang.inUse" : "settings.onRestart")));
      r.append(h("span", "c-name", name), h("span", "mono dim", code), h("span", "c-num", t("lang.words", { has: n(w.has), all: n(w.all) })), state);
      r.addEventListener("click", () => {
        if (!on) void setLanguageSetting(value);
      });
      table.append(r);
    }
    return table;
  }

  async function setLanguageSetting(code: string | null) {
    error = null;
    try {
      await invoke("set_language", { language: code });
      langSet = code;
    } catch (e) {
      error = String(e);
    }
    render();
  }

  async function setMenuLayout(layout: string | null) {
    error = null;
    try {
      menuSet = await invoke<MenuLayout>("set_menu_layout", { layout });
    } catch (e) {
      error = String(e);
    }
    render();
  }

  async function setOpen(key: OpenKey, command: string) {
    error = null;
    try {
      openSet = await invoke<OpenSettings>("set_open", { key, command });
    } catch (e) {
      error = String(e);
    }
    render();
  }

  async function setOpenPeek(key: OpenKey, peek: boolean) {
    error = null;
    try {
      openSet = await invoke<OpenSettings>("set_open_peek", { key, peek });
    } catch (e) {
      error = String(e);
    }
    render();
  }

  const OPEN_ROWS: { key: OpenKey; label: MessageKey }[] = [
    { key: "diff", label: "settings.open.diff" },
    { key: "text", label: "settings.open.text" },
    { key: "markdown", label: "settings.open.markdown" },
    { key: "html", label: "settings.open.html" },
    { key: "image", label: "settings.open.image" },
  ];

  /** One Skiff agent setting on the selected Host: This Mac, or a remote Host through its skiff-host. */
  const skiffAgentEdit = (edit: SkiffAgentEdit) => hostInvoke<SkiffAgentSettings>("skipper_config", "set_skiff_agent", { edit });

  /** A task type edit. Answers with the error to show; the table redraws. */
  async function saveTask(edit: SkiffAgentEdit): Promise<string | null> {
    try {
      agentSet = await skiffAgentEdit(edit);
      return null;
    } catch (e) {
      return String(e);
    }
  }

  async function setSkiffAgent(edit: SkiffAgentEdit) {
    error = null;
    try {
      agentSet = await skiffAgentEdit(edit);
    } catch (e) {
      error = String(e);
    }
    render();
  }

  /** Settings › Disk use › Auto flush: `[skiff_agent] cache_days`. */
  async function setCacheDays(days: number) {
    try {
      await invoke("set_cache_days", { days });
      if (disk.usage) disk.usage.cache_days = days;
      disk.error = null;
    } catch (e) {
      disk.error = String(e);
    }
    render();
  }

  /** Settings › Skippers › Instructions: each file's status, read again on each show. */
  async function loadFiles() {
    try {
      files = await invoke<InstructionFile[]>("skipper_files");
      // A save of a remote Host's copy that did not go back: one line under its row.
      const host = hostName();
      for (const f of await invoke<{ file: string; conflict: boolean; message: string }[]>("skipper_file_errors")) {
        fileErrors.set(f.file, f.conflict ? t("settings.remote.fileConflict", { host }) : f.message);
      }
    } catch (e) {
      files = [];
      error = String(e);
    }
    if (!root.hidden && tab === "skippers") render();
  }

  /**
   * Edit: the file opens as Open with › Text files says, written from the
   * template first when there is none. In the peek, Settings closes and
   * opens again on this tab when the peek ends.
   */
  async function editFile(f: InstructionFile) {
    fileErrors.delete(f.file);
    type Opened = { kind: "run"; script: string; cwd: string } | { kind: "default" } | { kind: "no_app"; message: string };
    let o: Opened;
    try {
      o = await invoke<Opened>("edit_skipper_file", { scope: f.scope });
    } catch (e) {
      fileErrors.set(f.file, String(e));
      return void loadFiles();
    }
    if (o.kind === "run") {
      close();
      return peekText(o.script, o.cwd, `${o.cwd}/${f.file.split("/").pop()}`, () => void settings.open("skippers"));
    }
    if (o.kind === "no_app") fileErrors.set(f.file, t("settings.noApp", { file: f.file }));
    void loadFiles();
  }

  /** Task types: each row's file size, read again on each show. */
  async function loadTypes() {
    agentSet = (await invoke<SkiffAgentSettings>("get_skiff_agent").catch(() => null)) ?? agentSet;
    if (!root.hidden && tab === "task-types") render();
  }

  /**
   * Task types › Edit: the file opens as Open with › Text files says, with
   * its `# <type>` line written first when there is none. In the peek,
   * Settings closes and opens again on this tab when the peek ends.
   */
  async function editTypeFile(task: TaskType) {
    typeErrors.delete(task.type);
    type Opened = { kind: "run"; script: string; cwd: string } | { kind: "default" } | { kind: "no_app"; message: string };
    let o: Opened;
    try {
      o = await invoke<Opened>("edit_task_type_file", { name: task.type });
    } catch (e) {
      typeErrors.set(task.type, String(e));
      return void loadTypes();
    }
    if (o.kind === "run") {
      close();
      return peekText(o.script, o.cwd, task.file, () => void settings.open("task-types"));
    }
    if (o.kind === "no_app") typeErrors.set(task.type, t("settings.noApp", { file: `${task.type}.md` }));
    void loadTypes();
  }

  /** Reset…: asks, with the file's date and size, then writes the template again. */
  async function resetFile(f: InstructionFile) {
    const base = f.file.split("/").pop()!;
    const date = f.modified ? new Date(f.modified).toISOString().slice(0, 10) : "";
    const who = f.scope.kind === "skipper" ? t("settings.reset.everySkipper") : t("settings.reset.oneSkipper", { name: f.scope.name });
    const ok = await confirmAction({
      title: t("settings.reset.title", { file: f.file }),
      body: t("settings.reset.body", { date, size: fileSize(f.size ?? 0), base, who }),
      action: t("settings.reset.action"),
    });
    if (!ok) return;
    fileErrors.delete(f.file);
    try {
      await invoke("reset_skipper_file", { scope: f.scope });
    } catch (e) {
      fileErrors.set(f.file, String(e));
    }
    void loadFiles();
  }

  /** Settings › Skippers › Folder › Change…. The page reads the folder again after a change. */
  function changeFolder() {
    if (!agentSet) return;
    changeSkiffFolder({
      current: agentSet.configured,
      changed: async () => {
        agentSet = await hostInvoke<SkiffAgentSettings>("skipper", "get_skiff_agent").catch(() => agentSet);
        render();
      },
    });
  }

  /** Settings › Skippers: the Skipper, project and fleet Skippers, and their instruction files. */
  function skippers(): HTMLElement {
    const main = h("div", "set-main");
    const intro = h("div", "set-head");
    intro.append(h("div", "set-h", t("settings.tab.skippers")));
    if (isRemote()) intro.append(h("div", "set-sub mono", hostName()));
    main.append(intro);
    // A Host whose Skiff is too old to take changes: its Skiff agent, read-only.
    if (isRemote() && !hostCan("skipper_config")) {
      main.append(h("div", "set-note", agentSet
        ? t("settings.remote.skipperOld", { host: hostName() })
        : t("settings.remote.noSkipper", { host: hostName() })));
      if (agentSet) {
        const a = agentSet;
        main.append(
          h("div", "set-note mono", t("settings.remote.skipperName", { name: mainName(a.name), agent: a.agent ?? t("settings.remote.default") })),
          h("div", "set-note mono", t(a.folder_error ? "settings.remote.folderRefused" : "settings.remote.folder", { folder: a.folder ?? a.configured, error: a.folder_error ?? "" })),
        );
      }
      return main;
    }
    if (agentSet) main.append(...skiffAgentSection(agentSet, agents, (e) => void setSkiffAgent(e), changeFolder));
    if (error) {
      const err = h("div", "set-error", error);
      err.setAttribute("role", "alert");
      main.appendChild(err);
    }
    if (agentSet) main.append(...projectSkippersSection(agentSet, agents, (e) => void setSkiffAgent(e)));
    if (agentSet?.folder) main.append(...avatarsSection(agentSet, draw));
    if (agentSet?.folder) main.append(...instructionsSection(agentSet, { files, errors: fileErrors, editable: hostCan("skipper_file"), edit: (f) => void editFile(f), reset: (f) => void resetFile(f) }));
    return main;
  }

  /** Which apps Skiff hands things to. */
  function openWith(): HTMLElement {
    const main = h("div", "set-main");
    const intro = h("div", "set-head");
    intro.append(h("div", "set-h", t("settings.tab.open")));
    const cols = h("div", "set-row set-open set-cols");
    cols.append(h("span", "c-name", upper(t("settings.open.colShows"))), h("span", "c-cmd", upper(t("settings.agents.colCommand"))), h("span", "c-mode", upper(t("settings.open.colOpensIn"))));
    main.append(intro, cols);
    const o = openSet;
    if (o) for (const { key, label: labelKey } of OPEN_ROWS) {
      const label = t(labelKey);
      const row = h("div", "set-row set-open set-agent on");
      const cmd = h("input", "c-cmd mono");
      cmd.value = o[key];
      cmd.placeholder = key === "diff" ? o.default_diff : t("settings.open.defaultApp");
      cmd.spellcheck = false;
      cmd.setAttribute("aria-label", t("settings.open.commandAria", { kind: label }));
      cmd.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          cmd.blur();
        } else if (e.key === "Escape") {
          cmd.value = o[key];
          cmd.blur();
        }
      });
      cmd.addEventListener("blur", () => {
        if (cmd.value.trim() !== o[key].trim()) void setOpen(key, cmd.value);
      });
      row.append(h("span", "c-name", label), cmd);
      if (key === "diff") row.append(h("span", "c-mode", t("settings.open.peek")));
      else if (key === "image") row.append(h("span", "c-mode", t("settings.open.app")));
      else {
        const mode = h("select", "c-mode");
        mode.setAttribute("aria-label", t("settings.open.modeAria", { kind: label }));
        for (const [value, label] of [["app", t("settings.open.app")], ["peek", t("settings.open.peek")]]) {
          const option = h("option", "", label);
          option.value = value;
          mode.append(option);
        }
        mode.value = o.peek.includes(key) ? "peek" : "app";
        mode.addEventListener("change", () => void setOpenPeek(key, mode.value === "peek"));
        row.append(mode);
      }
      main.appendChild(row);
    }
    if (error) {
      const err = h("div", "set-error", error);
      err.setAttribute("role", "alert");
      main.appendChild(err);
    }
    return main;
  }

  function close() {
    // Leaving a name list saves it.
    if (document.activeElement instanceof HTMLTextAreaElement) document.activeElement.blur();
    root.hidden = true;
    stopActivity(activity);
    gallery.keys = null;
    onClose();
    onShowChange();
  }

  /** Ctrl+Shift+Up/Down: the previous or next section, as in the tree. The keys go to its tab. */
  function stepTab(dir: 1 | -1) {
    // Leaving a field saves it, as a click on a tab does.
    (document.activeElement as HTMLElement | null)?.blur();
    tab = TABS[(TABS.indexOf(tab) + dir + TABS.length) % TABS.length];
    if (tab !== "activity") stopActivity(activity);
    draw();
    if (tab === "skippers") void loadFiles();
    if (tab === "disk") void countDisk(disk, render);
    if (tab === "activity") startActivity(activity, render);
    document.body.classList.add("kbd");
    root.querySelector<HTMLButtonElement>(".set-tab.active")?.focus();
  }

  /** Ctrl+Shift+Right: into the section, at its first control. Ctrl+Shift+Left: back to its tab. */
  function enterSection(into: boolean) {
    document.body.classList.add("kbd");
    if (!into) return root.querySelector<HTMLButtonElement>(".set-tab.active")?.focus();
    if (tab === "appearance") return root.querySelector<HTMLInputElement>(".tc-search")?.focus();
    root.querySelector<HTMLElement>(".set-main :is(button, input, select, textarea, [tabindex]):not(:disabled, .set-x)")?.focus();
  }

  /**
   * Plain arrows. On a tab, Up/Down change the section and Right goes into it.
   * In a section, Up/Down go to the nearest control above or below, and Left/Right
   * along the row. Text keeps its own keys: Left/Right move the caret, and a name
   * list lets Up/Down out only from its first or last line.
   */
  root.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.altKey || e.metaKey || e.shiftKey || !e.key.startsWith("Arrow")) return;
    const el = document.activeElement as HTMLElement | null;
    if (!el || !root.contains(el)) return;
    const up = e.key === "ArrowUp";
    const vertical = up || e.key === "ArrowDown";
    if (el.classList.contains("set-tab")) {
      if (vertical) stepTab(up ? -1 : 1);
      else if (e.key === "ArrowRight") enterSection(true);
      else return;
      return e.preventDefault();
    }
    if (!el.closest(".set-main")) return;
    if (el instanceof HTMLSelectElement) return;
    const text = el instanceof HTMLInputElement && el.type !== "range";
    // A text field moves its caret, and lets Left or Right out only at its start or end.
    if (!vertical && (el instanceof HTMLTextAreaElement || (el instanceof HTMLInputElement && !text))) return;
    if (!vertical && text) {
      const edge = e.key === "ArrowLeft" ? 0 : el.value.length;
      if (el.selectionStart !== edge || el.selectionEnd !== edge) return;
    }
    if (el instanceof HTMLTextAreaElement) {
      const v = el.value;
      if (up ? v.lastIndexOf("\n", el.selectionStart - 1) >= 0 : v.indexOf("\n", el.selectionEnd) >= 0) return;
    }
    // The current skiffd session row is a stop too: Left/Right go between it and its Reveal.
    const stops = [...root.querySelectorAll<HTMLElement>(".set-main :is(button, input, select, textarea, .set-daemon-row[tabindex='0']):not(:disabled, .set-x)")];
    const at = stops.indexOf(el);
    if (at < 0) return;
    e.preventDefault();
    // A control's line is its row: a small button sits lower in its cell than a field.
    const top = (c: HTMLElement) => (c.closest(".set-row") ?? c).getBoundingClientRect().top;
    let to = nextCard(stops, at, up ? "up" : e.key === "ArrowDown" ? "down" : e.key === "ArrowLeft" ? "left" : "right", top);
    // Left and Right keep to the row.
    if (!vertical && Math.abs(top(stops[to]) - top(el)) > 1) to = at;
    // Left from the start of a row goes back to the tab.
    if (to === at && e.key === "ArrowLeft") return enterSection(false);
    const next = stops[to];
    next.focus();
    // Into a text field from the right, the caret starts at its end.
    if (next instanceof HTMLInputElement && next.type !== "range" && e.key === "ArrowLeft") next.setSelectionRange(next.value.length, next.value.length);
  });

  /**
   * The text field with the keys, in Settings or in a dialog over it, such as
   * Folder › Change…. The native menu sends copy and paste here while a dialog is open.
   */
  const field = () => {
    const f = textField(document.activeElement);
    return f && (root.contains(f) || f.closest(".confirm-overlay")) ? f : null;
  };

  /** The list keys, and copy and paste in a text field, as Settings reads them. True when Settings used the key. */
  function runKey(a: Action | null): boolean {
    const f = a === "copy" || a === "paste" ? field() : null;
    if (f) void editField(f, a as "copy" | "paste");
    else if (a === "session-next" || a === "session-prev") stepTab(a === "session-next" ? 1 : -1);
    else if (a === "list-project" || a === "list-back") enterSection(a === "list-back");
    else return false;
    return true;
  }

  // Capture on the window so a terminal that still has focus while Settings
  // loads cannot stop Escape before Settings sees it. Menus and dialogs over
  // settings stop the key first.
  window.addEventListener("keydown", (e) => {
    if (root.hidden || e.defaultPrevented || document.querySelector("#ctx-menu:not([hidden]), #launch-menu:not([hidden]), .confirm-overlay")) return;
    if (runKey(actionFor(e))) return e.preventDefault();
    // Cut and select all on macOS. The webview does them only through an Edit menu item.
    const edit = fieldEditKey(e, isMac);
    const f = edit && field();
    if (edit && f) {
      e.preventDefault();
      return void editField(f, edit);
    }
    if (e.key !== "Escape") return;
    e.preventDefault();
    close();
  }, true);

  // An external app saved a file: its status shows when the window has the keys again.
  window.addEventListener("focus", () => {
    if (!root.hidden && tab === "skippers") void loadFiles();
    if (!root.hidden && tab === "task-types") void loadTypes();
  });

  const settings = {
    /** A new read of the Host list redraws an open Hosts tab. main.ts wires it to
     *  onHostsChanged: panels.ts makes Settings while hosts.ts is still loading. */
    hostsChanged() {
      if (!root.hidden && tab === "hosts") render();
    },
    /** `at`: the section to show, else the last one. A Hosts page opens
     *  Settings › Hosts at its list or its Add form. */
    async open(at?: HostsPage | (typeof TABS)[number]) {
      if (typeof at === "string") tab = at;
      else if (at) {
        tab = "hosts";
        hostsTab.open(at);
      }
      root.hidden = false;
      onShowChange();
      await look.load();
      openSet = await invoke<OpenSettings>("open_settings").catch(() => null);
      menuSet = await invoke<MenuLayout>("menu_layout").catch(() => null);
      langSet = (await invoke<Appearance>("get_appearance").catch(() => null))?.language ?? null;
      agentSet = await hostInvoke<SkiffAgentSettings>("skipper", "get_skiff_agent").catch(() => null);
      agents = await loadAgents().catch((e) => {
        error = String(e);
        return [];
      });
      await hostsTab.load();
      render();
      if (tab === "hosts") hostsTab.focusServer();
      if (tab === "skippers") void loadFiles();
      if (tab === "disk") void countDisk(disk, render);
      if (tab === "activity") startActivity(activity, render);
      if (tab === "appearance") root.querySelector<HTMLInputElement>(".tc-search")?.focus();
      else if (tab === "images") root.querySelector<HTMLInputElement>(".img-page .lib-search")?.focus();
      else root.querySelector<HTMLButtonElement>(".set-agent .c-on:not(:disabled)")?.focus();
    },
    close,
    /** The native menu runs its accelerators here, not through the keydown above. */
    runKey: (a: Action) => !root.hidden && runKey(a),
    get isOpen() {
      return !root.hidden;
    },
  };
  return settings;
}
