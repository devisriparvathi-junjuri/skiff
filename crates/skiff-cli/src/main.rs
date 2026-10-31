//! `skiff`: drive skiffd from a shell. Sessions, groups, agents and projects.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use skiff_client::Client;
use skiff_core::{group::SplitDir, session::Role};
use layout::Side;

mod attach;
mod drops;
mod callsign;
mod daemon;
mod events;
mod groups;
mod info;
mod layout;
mod ledger;
mod open_with;
mod peek;
mod print;
mod projects;
mod resolve;
mod sessions;
mod skipper;
mod theme;
mod wait;
mod watch;

#[derive(Parser)]
#[command(
    name = "skiff",
    version,
    about = "Command-line client for skiffd",
    after_help = "A session is a full id, a unique id prefix, or the name the sidebar shows. A group is an id, prefix or name.\nInside a Skiff pane, `.` is this pane's session, or the group that holds it."
)]
struct Cli {
    /// The skiffd socket. Default: $SKIFF_SOCKET, else the daemon's own default.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show this skiff's version and path, and the daemon's version,
    /// protocol, pid and socket. Warns when another skiff comes first on
    /// PATH, or skiffd's version is not the one this skiff was built for.
    #[command(after_help = "--json prints one object and no warnings:\n  {\"skiff\":VERSION,\"protocol\":N,\"socket\":PATH,\"skiffd\":{\"version\":V,\"protocol\":N,\"pid\":PID,\"host_id\":ID}}\nskiffd is null when no skiffd answers on the socket; the exit code is then 1.")]
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Manage the daemon itself.
    #[command(subcommand)]
    Daemon(DaemonCmd),
    /// List sessions.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Start a session and print its id.
    #[command(
        after_help = "--type starts a task type from Settings › Task types ([skiff_agent.tasks] in projects.toml): its agent, with its model flag. With no --name, the session takes the type's name, numbered when one runs (review, review 2).\nFrom a Skipper (Admiral, a project or a fleet Skipper), new and `group new` need --name: with none, they exit 2 and start nothing. Name a session for one issue `#N feature`, such as `#136 kanban and calendar`; any other session 2 to 4 words that say the task, without the project name; a group after the work it holds.\nWith no row for the type, it exits 1 and starts nothing.\n--model replaces the row's model for this session. A custom agent takes its model in its command, so it refuses --model.\nThe type's instruction file, types/TYPE.md in the agent folder, goes to the agent at start: claude --append-system-prompt-file, codex -c developer_instructions, grok --rules, opencode OPENCODE_CONFIG_CONTENT, kimi --agent-file. An empty file is none. gemini, hermes and custom agents cannot take one: --type exits 1 and starts nothing.\n`skiff agents` lists the types with the command each one starts."
    )]
    New {
        /// The session's name.
        #[arg(long)]
        name: Option<String>,
        /// Working directory. Default: the current directory.
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
        /// Start this agent, as the app's + menu does.
        #[arg(long, value_name = "ID", conflicts_with = "command")]
        agent: Option<String>,
        /// Start this task type's agent with its model and instruction file. Not with --agent.
        #[arg(long = "type", value_name = "TYPE", conflicts_with_all = ["agent", "command"])]
        task_type: Option<String>,
        /// With --type: this model in place of the type's model.
        #[arg(long, value_name = "MODEL", requires = "task_type")]
        model: Option<String>,
        /// Default: agent with --agent or --type, else shell.
        #[arg(long, value_enum)]
        role: Option<RoleArg>,
        /// Put the new session into this group.
        #[arg(long, value_name = "GROUP")]
        group: Option<String>,
        /// With --group: the side of the focused pane it goes on. Default:
        /// the group's next template shape.
        #[arg(long, value_enum, requires = "group")]
        split: Option<Side>,
        /// Print the new session as JSON.
        #[arg(long)]
        json: bool,
        /// Program and arguments, after `--`. Default: the login shell.
        #[arg(last = true, value_name = "CMD")]
        command: Vec<String>,
    },
    /// Type text into an idle session. `-` reads the text from stdin. Put
    /// text that starts with `-` after `--`.
    #[command(
        after_help = "send checks the session before it types any bytes. Exit codes:\n  0  sent\n  1  error\n  3  the session is waiting (approval prompt or bell). A person answers it in the pane. No flag overrides this.\n  4  the session is working. Skiff does not type into a busy session.\n  5  a shell or an unknown program is in front. Use --shell only to type a shell command. A program started with `skiff new -- CMD` or a custom agent shows its launch shell, so a send to it needs --shell.\n  6  the session is done\n  7  the session is your own ($SKIFF_SESSION)\n  8  --enter went to a known agent, but no turn started in 5 seconds. The text can be in its input box. Run `skiff peek` on the session.\n  9  a known agent shows text in its input box or its busy line, or the session took input after the check. Nothing was typed. A second send after exit 8 gets this code while the text is still in the box."
    )]
    Send {
        session: String,
        #[arg(required = true)]
        text: Vec<String>,
        /// Press Enter after the text.
        #[arg(long)]
        enter: bool,
        /// Send the bytes as they are. Default: newlines become Enter.
        #[arg(long)]
        raw: bool,
        /// Permit an idle session with a shell or an unknown program in
        /// front. It does not override any other refusal.
        #[arg(long)]
        shell: bool,
    },
    /// Rename a session. An empty name clears it.
    Rename { session: String, name: String },
    /// Run a session in this terminal. Ctrl-\ detaches. The session
    /// takes this terminal's size, in the app too, and gets its own size
    /// back on detach. A quick second Ctrl-C is held back once.
    Attach {
        session: String,
        /// A remote Host from Settings › Hosts: its name, id or ssh destination.
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
    },
    /// Attach to the Skiff agent (Admiral). Start it first if it does not
    /// run. Ctrl-\ detaches; the agent keeps running.
    #[command(
        after_help = "Reads [skiff_agent] in projects.toml: the folder, the name, the agent and whether it is on.\nWith no Skiff agent session, it sets up the folder (missing files come back, existing ones stay), starts the agent, and starts `skiff watch` for it. One watcher runs per daemon.\nA Skiff agent session that is a bare shell (after a restore, or after its agent quit) gets the agent again: its last conversation when Settings continues on restore, else a new one.\nWith no skiffd running, it starts one, as the app does.\nThe app and this command find the same session: skiffd marks it, and two starts at once give one.\nAttach gives the session this terminal's size, in the app too, until you detach. Then the pane gets its own size back.\n--move-to resolves a relative PATH against this shell's folder and stores the full path. The old folder keeps every file and gets MOVED.md; Skiff never deletes it. The conversation does not move. Admiral's session ends first, and tasks that still run stop the change unless you confirm."
    )]
    Skipper {
        /// Do not attach. Print the session id.
        #[arg(long)]
        no_attach: bool,
        /// Change the Skiff agent's folder to PATH, as Settings › Skippers ›
        /// Folder does: copy, check and switch to a new or empty folder, or
        /// switch to a Skiff folder. It shows the preview and asks first.
        #[arg(long, value_name = "PATH", conflicts_with = "no_attach")]
        move_to: Option<std::path::PathBuf>,
        /// The project Skipper of PROJECT in place of Admiral: find it,
        /// or start it in the project's folder with SKIPPER.md as its first
        /// message.
        #[arg(long, value_name = "PROJECT", conflicts_with = "move_to")]
        project: Option<String>,
        /// The fleet Skipper of FLEET: find it, or start it in its folder
        /// with SKIPPER.md as its first message. When project Skippers run
        /// in the fleet, it names them and asks: the fleet Skipper takes
        /// their work over and they stop when idle.
        #[arg(long, value_name = "FLEET", conflicts_with_all = ["move_to", "project"])]
        fleet: Option<String>,
        /// With --fleet: take the project Skippers over without asking.
        #[arg(long, requires = "fleet")]
        yes: bool,
        /// With --move-to: set up a new ledger in place of a copy.
        #[arg(long, requires = "move_to")]
        empty: bool,
        /// With --move-to: show the preview and change nothing.
        #[arg(long, requires = "move_to")]
        dry_run: bool,
    },
    /// Print a session's screen as plain text.
    Peek {
        session: String,
        /// The last N lines of history and screen. Default: the screen.
        #[arg(long, short = 'n', value_name = "N")]
        lines: Option<usize>,
    },
    /// Wait until sessions reach a state. Exit 0 when all do, 1 on timeout,
    /// 2 when one closes or exits first, 3 when one is waiting.
    #[command(
        after_help = "Exit codes:\n  0  every session reached the state. With --any: one session reached it. Its id, name and state print on stdout.\n  1  timeout\n  2  a session closed or exited before it reached the state. With --any: every session did.\n  3  a session is waiting (approval prompt or bell). A person answers it in its pane.\nWith --for idle, done or any-not-working, wait stops at once when any session is waiting. --for waiting is not changed.\nWith --for idle, a known agent that is quiet but shows its busy line (\"esc to interrupt\") is not idle yet.\nIn a Skiff pane ($SKIFF_SESSION), or with no terminal on stdout, the timeout defaults to 45 seconds and is at most 60.\nAfter exit 0, read the screen with `skiff peek`: idle does not prove that an agent finished."
    )]
    Wait {
        #[arg(required = true)]
        sessions: Vec<String>,
        #[arg(long = "for", value_enum, value_name = "STATE")]
        target: wait::Target,
        /// Stop when the first session reaches the state.
        #[arg(long)]
        any: bool,
        /// Give up after this many seconds. For an agent: default 45, at most 60.
        #[arg(long, value_name = "SECS")]
        timeout: Option<f64>,
    },
    /// Print each state change, exit and removal as one line, for every
    /// session or for the named ones. Runs until Ctrl+C or --timeout.
    #[command(
        after_help = "Lines: a state change prints the new state, an exit prints `exit` and the code, a closed session prints `removed`. No output or screen text prints.\nWith --json, each line is one JSON object:\n  {\"event\":\"state\",\"session\":ID,\"name\":NAME,\"state\":\"idle\"}\n  {\"event\":\"exit\",\"session\":ID,\"name\":NAME,\"code\":0}\n  {\"event\":\"removed\",\"session\":ID,\"name\":NAME}\nExit codes:\n  0  --timeout ran out, or every named session was removed\n  1  error\n  2  a name matches no session\nRun it in the background so your own turn stays free. idle means quiet, not done. After any line, read the screen with `skiff peek`."
    )]
    Events {
        /// Default: every session, also the ones started later.
        sessions: Vec<String>,
        /// One JSON object for each line.
        #[arg(long)]
        json: bool,
        /// Stop after this many seconds.
        #[arg(long, value_name = "SECS")]
        timeout: Option<f64>,
    },
    /// Follow the open tasks in the Skiff agent's ledger for that agent.
    /// Skiff starts it with the agent's session; it stops when that session
    /// ends. One runs per daemon.
    #[command(
        after_help = "A pane in an open ledger row (sent, pending, blocked) that:\n  waits (approval or bell): the person gets a desktop notification. The agent gets nothing.\n  ends a turn, goes idle after a prompt, exits or is gone: the agent gets one line, typed while it is idle and its input box is empty. Lines that wait for it are joined.\n  is idle with a pending row: the line names the oldest pending uid and its `skiff ledger send` command, once per idle stretch. The row is read again just before the line is typed, and the line is dropped if the task left pending.\n  restarted since the send: the agent gets one line that names the task as interrupted, once per run.\n  is gone with open rows: the line names their uids. A gone pane cannot be marked seen: reassign each task with `ledger update UID --pane ID --state pending`, or mark it done.\nskiffd types a line only if the person typed nothing in the agent's pane since the check. If no turn starts, the log says undelivered and Enter is not pressed again.\nA pane stays unread until the agent or the person marks it seen, so a new run tells it again, as it does a pending row.\nIt logs one line for each thing it does."
    )]
    Watch {
        /// The Skiff agent's session.
        skipper: String,
        /// Do not send desktop notifications.
        #[arg(long)]
        no_notify: bool,
    },
    /// Mark a session seen, as focusing its pane does.
    Seen { session: String },
    /// Stop sessions.
    #[command(
        after_help = "kill refuses a session that has an open ledger task (pending, sent, blocked or interrupted), from any Skipper. It names each such session with the task's uid and title, and stops none of the sessions. Close the task with `skiff ledger update UID --state done`, or use --force.\nExit codes:\n  0  killed\n  1  error\n  10  a session has an open ledger task. Nothing was killed."
    )]
    Kill {
        #[arg(required = true)]
        sessions: Vec<String>,
        /// Kill even a session that has an open ledger task.
        #[arg(long)]
        force: bool,
    },
    /// Named splits of sessions.
    #[command(subcommand, visible_alias = "g")]
    Group(GroupCmd),
    /// List the agents skiffd knows.
    Agents {
        #[arg(long)]
        json: bool,
    },
    /// List the projects in projects.toml, or add or remove one.
    #[command(
        args_conflicts_with_subcommands = true,
        after_help = "add and remove write projects.toml, as Add project and Remove do in the app. The app shows the change within a second or two."
    )]
    Projects {
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<ProjectsCmd>,
    },
    /// List and assign terminal color themes.
    #[command(after_help = "A session theme overrides its project's theme. A project theme overrides the app theme. Clear returns to the next setting.")]
    Theme {
        #[command(subcommand)]
        cmd: ThemeCmd,
    },
    /// Show and set the command that opens a diff or a file.
    #[command(
        name = "open-with",
        after_help = "Keys: diff, text, markdown, html. `{path}` in a command becomes the file; without it the file goes at the end. `{target}` in the diff command becomes what to compare.\nA terminal tool shows in the peek over the panes. An app with its own window just starts. A file with no command opens in the system's default app.\nOPENS IN is what selecting a file does: peek runs the command, app shows an Open button. An explicit Open always runs it. diff always opens in the peek.\nChanges apply to the next file or diff the app opens."
    )]
    OpenWith {
        #[command(subcommand)]
        cmd: OpenWithCmd,
    },
    /// The Skiff agent's task ledger: ledger.db in its folder.
    #[command(after_help = "States: sent, pending (not delivered: the pane was working), blocked (the pane needs the user), done.\nls and show print interrupted for a sent or blocked task whose pane restarted since the send: a restore runs a new shell under the old pane id. They print gone for an open task whose pane no longer exists. A task sent on another daemon shows elsewhere in LIVE and its socket: it is never gone from here, and send refuses it. A plan with tasks shows done when every task is done, else sent.\nID is the row number or the 8-character uid that add prints. Neither is ever reused.\nWork folders: add and handoff make one folder for each row, <uid>-<slug>/, in results/ next to the ledger, or in projects/<project>/results/ for a project Skipper's rows and a hand-off to it, or in fleets/<fleet>/results/ for a fleet Skipper's. A task in a plan, or under a hand-off, gets its folder in that row's folder. The result is result.md in the folder (plan.md for a plan). add prints the folder and the path, and makes no file in the folder. dir prints the folder. A row from before work folders keeps its result path and has no folder. rm and prune keep the files; with --flush they flush the work folders by the Disk use rule.\nWork: a plan with its tasks, or a task with no plan. Work is working while an agent is on one of its tasks (the task is sent, and its pane works or ended its turn), open otherwise, and finished once closed. close stamps it on the person's word; reopen clears the stamp. A task added or moved under closed work, or set from done to another state, reopens it.\nReported: report stamps a done task once the person was told. An update to a state other than done clears the stamp. ls --unreported lists the done tasks not yet reported.\nThe ledger is ~/skiff-agent/ledger.db, or ledger.db in the [skiff_agent] folder override. On a socket other than the live one, it is in the socket's private state. SKIFF_LEDGER overrides the path.\nOwners: each row is Admiral's (grand), a project Skipper's (its project) or a fleet Skipper's (fleet:<fleet>). add, send, update, report, close, reopen, prune and rm refuse unless the caller owns the row: the Skiff agent session that owns the ledger for Admiral's rows, a project or fleet Skipper for its own. A project or fleet Skipper may also set the state and result of the hand-off it was given. A fleet Skipper's add and update --pane refuse a pane outside its fleet's projects. A Skipper a fleet Skipper took over is refused. skiff skipper and the agent's watcher record the Skipper's session; a new Skiff agent session on the owner's daemon, or on the live daemon, takes it over. The refusal names the owner. A shell pane is a person and may change rows. A shell outside Skiff has no SKIFF_SESSION, and an agent there looks the same, so it is refused too: a person there passes --person. ls and show work from anywhere.\nA ledger from before version 3 moves aside to ledger.db.old on first use, and a new one starts empty.")]
    Ledger {
        /// From a shell outside Skiff: I am a person, not an agent. Without
        /// it, add, send, update, report, close, reopen, prune and rm refuse
        /// there.
        #[arg(long, global = true)]
        person: bool,
        #[command(subcommand)]
        cmd: LedgerCmd,
    },
}

#[derive(Subcommand)]
pub enum LedgerCmd {
    /// Add a task, or a plan with --plan, and make its work folder. Prints
    /// its id, uid, folder and result path, and the plan's uid for a task in
    /// a plan.
    Add {
        /// The pane the task went to: an id, a unique prefix or a name. It
        /// must exist. A plan has no pane.
        #[arg(long, required_unless_present = "plan", conflicts_with = "plan")]
        pane: Option<String>,
        #[arg(long)]
        task: String,
        /// A short title for the app's Ledger panel: 3 to 6 words. Default:
        /// the panel takes one from the task text.
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        /// A plan: a row with no pane that groups tasks under it.
        #[arg(long)]
        plan: bool,
        /// The work this task joins: a plan or any task in it, or a task with
        /// no plan, which a new plan then wraps. Its id or uid. Closed work
        /// opens again.
        #[arg(long, value_name = "ID", conflicts_with = "plan")]
        parent: Option<String>,
        /// Default: the project that holds the pane's folder.
        #[arg(long)]
        project: Option<String>,
        /// Default: pending for a task, sent for a plan.
        #[arg(long)]
        state: Option<String>,
        /// Where the child writes its result. Default: result.md in the row's work folder.
        #[arg(long, value_name = "PATH")]
        result: Option<String>,
        /// Free text that names the work elsewhere: a URL, a ticket key,
        /// anything. Skiff stores it and never reads it.
        #[arg(long = "ref", value_name = "TEXT")]
        reference: Option<String>,
        /// The task type the task went out as, from `skiff agents`. Skiff
        /// stores it and never acts on it.
        #[arg(long = "type", value_name = "TYPE")]
        task_type: Option<String>,
        /// Add the task as pending and send it at once, as `ledger send` does.
        #[arg(long, conflicts_with_all = ["plan", "state"])]
        send: bool,
        /// With --send: press Enter after the text.
        #[arg(long, requires = "send")]
        enter: bool,
        /// With --send: as `send --shell`.
        #[arg(long, requires = "send")]
        shell: bool,
    },
    /// Hand a task to a project's or a fleet's Skipper: add a hand-off row,
    /// start that Skipper if none runs, and send it the task. Exits with
    /// `send`'s code.
    #[command(
        after_help = "SCOPE is a project or a fleet, by its name in `skiff projects`. A name that is both is refused: use project:NAME or fleet:NAME. Both spellings always work.\nThe hand-off row is Admiral's: its pane is the project or fleet Skipper, and its result path is in that Skipper's results folder. That Skipper adds its own tasks under it with --parent UID, and reports it with `skiff ledger update UID --state done --result PATH`. A project or fleet Skipper cannot hand off.\nA project in a fleet whose Skipper runs is refused: hand off to the fleet. A fleet with project Skippers and no fleet Skipper is refused: the person starts the fleet Skipper, which takes them over.\nA new Skipper gets SKIPPER.md as its first message, with the task after it. A busy one keeps the row pending: `skiff watch` names it when that Skipper is idle."
    )]
    Handoff {
        /// The project or the fleet: NAME, project:NAME or fleet:NAME.
        #[arg(value_name = "SCOPE")]
        project: String,
        task: String,
        /// A short title for the app's Ledger panel: 3 to 6 words.
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        /// The work it joins: a plan of yours, or any task in it.
        #[arg(long, value_name = "ID")]
        parent: Option<String>,
    },
    /// Send a pending or interrupted task to its pane, then mark it sent.
    /// Exits with `send`'s code.
    #[command(
        after_help = "Only a pending or interrupted task with a live pane is sent. Else it exits 1 and sends nothing.\nThe text defaults to the task and its result path. `-` reads the text from stdin.\nIt runs every `send` check. On exit 0 the task is sent. On exit 8 (no turn started) it is blocked: the text can be in the input box. On a refusal it keeps its state, and `skiff watch` names it again when the pane is idle."
    )]
    Send {
        id: String,
        /// Default: the task and its result path.
        text: Vec<String>,
        /// Press Enter after the text.
        #[arg(long)]
        enter: bool,
        /// As `send --shell`.
        #[arg(long)]
        shell: bool,
    },
    /// Change a task. Only the given fields change. A new pane, or the state
    /// sent, records the pane's current generation.
    Update {
        id: String,
        #[arg(long)]
        state: Option<String>,
        #[arg(long, value_name = "PATH")]
        result: Option<String>,
        #[arg(long)]
        task: Option<String>,
        /// The short title for the app's Ledger panel.
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        #[arg(long)]
        pane: Option<String>,
        #[arg(long)]
        project: Option<String>,
        /// Move the task into this work, as add --parent does.
        #[arg(long, value_name = "ID")]
        parent: Option<String>,
        #[arg(long = "ref", value_name = "TEXT")]
        reference: Option<String>,
        #[arg(long = "type", value_name = "TYPE")]
        task_type: Option<String>,
    },
    /// List the work that is not closed: working first, then open, oldest
    /// first, each plan's tasks under it. WORK names the group. LIVE says if
    /// the pane still exists. An open task whose pane is gone is set done
    /// first, and shows `gone`.
    #[command(after_help = "--state filters inside the list. --json adds bucket (working, open or finished), closed_at and gone_at to each row.")]
    Ls {
        /// sent, pending, blocked, done, interrupted or gone.
        #[arg(long)]
        state: Option<String>,
        /// Every row, closed work included.
        #[arg(long, conflicts_with_all = ["closed", "unreported"])]
        all: bool,
        /// Only closed work.
        #[arg(long, conflicts_with = "unreported")]
        closed: bool,
        /// Only done tasks not yet reported, under their plans.
        #[arg(long)]
        unreported: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show one task.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Mark done tasks as told to the person.
    #[command(
        after_help = "A task that is not done is refused, and nothing changes. A reported task keeps its first stamp.\nA plan's ID reports its done tasks and leaves its open ones."
    )]
    Report {
        #[arg(required_unless_present = "all", value_name = "ID")]
        ids: Vec<String>,
        /// Every done task not yet reported.
        #[arg(long, conflicts_with = "ids")]
        all: bool,
    },
    /// Close work on the person's word: a plan, or a task with no plan. It
    /// stamps the done tasks not yet reported too.
    #[command(after_help = "Work with a task that is not done is refused, and nothing changes: set the task done or remove it first. A task in a plan is refused: close its plan. Closed work keeps its first stamp.")]
    Close {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
    },
    /// Open closed work again: a plan, or a task with no plan.
    Reopen {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
    },
    /// Remove closed work, with its tasks. Open work stays. Without --yes, it
    /// prints the rows and the size of their work folders, and changes nothing.
    #[command(after_help = "The work folders stay unless you add --flush. --flush removes each folder that Settings › Disk use would flush: the work is done, its pane is not working, nothing in it changed in the last 15 minutes, and no git checkout in it has changes or unpushed commits. A folder in use stays, and the output says why.")]
    Prune {
        /// Remove work closed longer ago than this: 7d, 12h, 30m.
        #[arg(long, value_name = "AGE", default_value = "7d")]
        older_than: String,
        #[arg(long)]
        yes: bool,
        /// Also flush the rows' work folders, by the Disk use rule.
        #[arg(long, requires = "yes")]
        flush: bool,
    },
    /// Take a row out of the ledger now. Its work folder and result file stay
    /// unless you add --flush. For finished work, use close, and later prune.
    Rm {
        id: String,
        /// Also flush the row's work folder, by the Disk use rule.
        #[arg(long)]
        flush: bool,
    },
    /// Print a row's work folder: everything for the task goes in it.
    #[command(after_help = "A row from before work folders has none: it exits 1 and names its result path.")]
    Dir { id: String },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Move skiffd onto a new binary and keep every session, with its
    /// process, screen and scrollback. Checks afterwards that each one survived.
    Reload {
        /// The skiffd to run. Default: the running daemon's executable as it
        /// is on disk now, for example after a rebuild.
        #[arg(long, value_name = "PATH")]
        binary: Option<PathBuf>,
    },
    /// Stop skiffd. It saves the layout and ends every session. From a
    /// pane, this is the way to stop it: skiffd ignores SIGTERM and SIGINT
    /// from its own sessions.
    Stop,
}

#[derive(Subcommand)]
enum GroupCmd {
    /// List groups with their panes. `*` marks the focused pane.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Make a group and print its id. With no sessions it holds one empty pane.
    New {
        /// Default: its agents' names, as the app names a group.
        #[arg(long)]
        name: Option<String>,
        sessions: Vec<String>,
        /// Put every pane in one line. Default: the app's shapes.
        #[arg(long, value_enum)]
        dir: Option<DirArg>,
        /// The group's folder, where its empty panes start sessions.
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Put a session into a group. It fills an empty pane, else the group
    /// takes its next template shape. The session leaves any other group.
    Add {
        group: String,
        session: String,
        /// Split the target pane in half, on this side. Default: the next
        /// template shape.
        #[arg(long, value_enum)]
        split: Option<Side>,
        /// Split this pane instead of the focused one.
        #[arg(long, value_name = "SESSION")]
        of: Option<String>,
    },
    /// Take a session out of a group. The session keeps running.
    Rm { group: String, session: String },
    /// Rename a group.
    Rename { group: String, name: String },
    /// Delete groups. Their sessions keep running.
    Delete {
        #[arg(required = true)]
        groups: Vec<String>,
    },
}

#[derive(Subcommand)]
enum OpenWithCmd {
    /// List each key and its command.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Set a key's command, where it opens, or both.
    #[command(override_usage = "skiff open-with set <KEY> [COMMAND] [--peek | --app]", group = clap::ArgGroup::new("what").required(true).multiple(true).args(["command", "peek", "app"]))]
    Set {
        #[arg(value_enum)]
        key: OpenKey,
        /// The command, quoted as one argument.
        command: Option<String>,
        /// Selecting the file runs the command in the peek.
        #[arg(long, conflicts_with = "app")]
        peek: bool,
        /// Selecting the file shows an Open button.
        #[arg(long)]
        app: bool,
    },
    /// Clear a key. The default applies.
    Clear {
        #[arg(value_enum)]
        key: OpenKey,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum OpenKey {
    Diff,
    Text,
    Markdown,
    Html,
    Image,
}

impl OpenKey {
    fn name(self) -> &'static str {
        match self {
            OpenKey::Diff => "diff",
            OpenKey::Text => "text",
            OpenKey::Markdown => "markdown",
            OpenKey::Html => "html",
            OpenKey::Image => "image",
        }
    }
}

#[derive(Subcommand)]
enum ProjectsCmd {
    /// Add the git repository that holds PATH, or the folder itself when it
    /// is in no repository. A folder in a linked worktree adds the main
    /// worktree. Defaults: the folder's name, a free color, a theme of its
    /// own. A closed project for the folder opens again with its own
    /// settings; --name and --fleet do not apply to it.
    Add {
        path: PathBuf,
        /// The project's name. Default: the folder's name.
        #[arg(long)]
        name: Option<String>,
        /// Put the project in this fleet on the rail.
        #[arg(long)]
        fleet: Option<String>,
        /// Print the project as one item of `skiff projects --json`.
        #[arg(long)]
        json: bool,
    },
    /// Remove the project and its settings. The folder and its worktrees
    /// stay on disk. With running sessions in the project, it refuses and
    /// names them.
    #[command(visible_alias = "rm")]
    Remove {
        name: String,
        /// End the project's sessions first, then remove it.
        #[arg(long)]
        end_sessions: bool,
    },
}

#[derive(Subcommand)]
enum ThemeCmd {
    /// List available themes.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Set or clear the app theme.
    App {
        #[command(subcommand)]
        cmd: AppThemeCmd,
    },
    /// Set or clear a project's theme.
    Project {
        #[command(subcommand)]
        cmd: ProjectThemeCmd,
    },
    /// Set or clear a session's theme override.
    #[command(visible_alias = "terminal")]
    Session {
        #[command(subcommand)]
        cmd: SessionThemeCmd,
    },
}

#[derive(Subcommand)]
enum AppThemeCmd {
    /// Set the app theme.
    Set { theme: String },
    /// Clear the app theme. The default applies.
    Clear,
}

#[derive(Subcommand)]
enum ProjectThemeCmd {
    /// Set the project theme.
    Set { project: String, theme: String },
    /// Clear the project theme. The app theme applies.
    Clear { project: String },
}

#[derive(Subcommand)]
enum SessionThemeCmd {
    /// Set the session theme override.
    Set { session: String, theme: String },
    /// Clear the session theme override. The project or app theme applies.
    Clear { session: String },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DirArg {
    /// Side by side.
    Row,
    /// Stacked.
    Col,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum RoleArg {
    Agent,
    Task,
    Server,
    Shell,
}

impl From<RoleArg> for Role {
    fn from(r: RoleArg) -> Self {
        match r {
            RoleArg::Agent => Role::Agent,
            RoleArg::Task => Role::Task,
            RoleArg::Server => Role::Server,
            RoleArg::Shell => Role::Shell,
        }
    }
}

fn main() {
    // Rust ignores SIGPIPE, so `skiff ls | head` would panic on a closed pipe.
    // The default action ends the process quietly, like other CLI tools.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(run(cli)) {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("skiff: {e:#}");
            std::process::exit(1);
        }
    }
}

/// The exit code. Only `wait`, `events`, `send`, `kill`, and `new` or
/// `group new` from a Skipper, return other than 0.
async fn run(cli: Cli) -> Result<i32> {
    let socket = cli.socket.unwrap_or_else(skiff_core::socket::socket_path);
    // The state follows the socket: only the live one uses the person's files.
    skiff_core::state::set_socket(socket.clone());
    // Check before command-specific settings or ledger work. Local settings
    // and recovery commands remain available with a different daemon.
    if !matches!(&cli.cmd,
        Cmd::Status { .. } | Cmd::Daemon(_) | Cmd::Projects { cmd: None, .. }
        | Cmd::Projects { cmd: Some(ProjectsCmd::Add { .. }), .. }
        | Cmd::Theme { cmd: ThemeCmd::Ls { .. } | ThemeCmd::App { .. } | ThemeCmd::Project { .. } }
        | Cmd::OpenWith { .. }
    ) {
        if let Ok(c) = Client::connect(&socket).await {
            check_daemon(c).await?;
        }
    }
    match cli.cmd {
        // Read locally: no daemon needed.
        Cmd::Projects { json, cmd: None } => return info::projects(json).map(|_| 0),
        Cmd::Projects { cmd: Some(ProjectsCmd::Add { path, name, fleet, json }), .. } => {
            return projects::add(&path, name, fleet, json).map(|_| 0)
        }
        Cmd::Projects { cmd: Some(ProjectsCmd::Remove { name, end_sessions }), .. } => {
            return projects::remove(&socket, &name, end_sessions).await.map(|_| 0)
        }
        Cmd::Theme { cmd: ThemeCmd::Ls { json } } => return theme::ls(json).map(|_| 0),
        Cmd::Theme { cmd: ThemeCmd::App { cmd } } => return theme::app(cmd).map(|_| 0),
        Cmd::Theme { cmd: ThemeCmd::Project { cmd } } => return theme::project(cmd).map(|_| 0),
        Cmd::OpenWith { cmd } => return open_with::run(cmd).map(|_| 0),
        Cmd::Ledger { person, cmd } => return ledger::run(&socket, cmd, person).await,
        Cmd::Status { json: true } => return Ok(info::status_json(&socket).await),
        Cmd::Status { json: false } => return info::status(&socket).await.map(|_| 0),
        Cmd::Daemon(DaemonCmd::Reload { binary }) => return daemon::reload(&socket, binary).await.map(|_| 0),
        Cmd::Daemon(DaemonCmd::Stop) => return daemon::stop(&socket).await.map(|_| 0),
        // Starts skiffd when it does not run.
        Cmd::Skipper { move_to: Some(to), empty, dry_run, .. } => return skipper::move_to(&socket, &to, empty, dry_run).await.map(|_| 0),
        Cmd::Skipper { project: Some(p), no_attach, .. } => return skipper::run_project(&socket, &p, no_attach).await.map(|_| 0),
        Cmd::Skipper { fleet: Some(f), no_attach, yes, .. } => return skipper::run_fleet(&socket, &f, no_attach, yes).await.map(|_| 0),
        Cmd::Skipper { no_attach, .. } => return skipper::run(&socket, no_attach).await.map(|_| 0),
        Cmd::Attach { session, host: Some(host) } => return attach::attach_remote(&host, &session).await.map(|_| 0),
        // It connects again after a daemon reload, so it holds no connection here.
        Cmd::Watch { skipper, no_notify } => {
            return watch::watch(&socket, watch::Options { skipper, notify: !no_notify }).await
        }
        _ => {}
    }
    let c = connect(&socket).await?;
    if let Cmd::Wait { sessions, target, any, timeout } = &cli.cmd {
        let t = wait::timeout_for(*timeout, wait::agent_caller());
        if timeout.is_some_and(|asked| t != Some(asked)) {
            eprintln!("skiff: --timeout capped at {}s for an agent", t.unwrap_or_default());
        }
        return wait::wait(&c, sessions, *target, *any, t).await;
    }
    if let Cmd::Events { sessions, json, timeout } = &cli.cmd {
        return events::events(&c, sessions, *json, *timeout).await;
    }
    if let Cmd::Send { session, text, enter, raw, shell } = &cli.cmd {
        let o = sessions::Send { text, enter: *enter, raw: *raw, shell: *shell };
        return sessions::send(&c, session, o).await;
    }
    if let Cmd::Kill { sessions: ids, force } = &cli.cmd {
        return sessions::kill(&c, &socket, ids, *force).await;
    }
    let unnamed = match &cli.cmd {
        Cmd::New { name: None, .. } => Some(false),
        Cmd::Group(GroupCmd::New { name: None, .. }) => Some(true),
        _ => None,
    };
    if let Some(group) = unnamed {
        if sessions::coordinator_calls(&c).await? {
            eprintln!("skiff: {}", sessions::name_rule(group));
            return Ok(sessions::UNNAMED);
        }
    }
    match cli.cmd {
        Cmd::Ls { json } => sessions::ls(&c, json).await,
        Cmd::New { name, cwd, agent, task_type, model, role, group, split, json, command } => {
            let opts = sessions::New { name, cwd, agent, task_type, model, role: role.map(Role::from), command };
            sessions::new(&c, opts, group.as_deref(), split, json).await
        }
        Cmd::Group(g) => group(&c, g).await,
        Cmd::Rename { session, name } => sessions::rename(&c, &session, &name).await,
        Cmd::Attach { session, .. } => attach::attach(&c, &session, None).await,
        Cmd::Peek { session, lines } => peek::peek(&c, &session, lines).await,
        Cmd::Seen { session } => sessions::seen(&c, &session).await,
        Cmd::Agents { json } => info::agents(&c, json).await,
        Cmd::Theme { cmd: ThemeCmd::Session { cmd } } => theme::session(&c, cmd).await,
        Cmd::Status { .. } | Cmd::Daemon(_) | Cmd::Projects { .. } | Cmd::Theme { .. } | Cmd::OpenWith { .. } | Cmd::Ledger { .. } | Cmd::Wait { .. } | Cmd::Events { .. } | Cmd::Watch { .. } | Cmd::Send { .. } | Cmd::Kill { .. } | Cmd::Skipper { .. } => {
            unreachable!()
        }
    }
    .map(|_| 0)
}

async fn group(c: &Client, cmd: GroupCmd) -> Result<()> {
    match cmd {
        GroupCmd::Ls { json } => groups::ls(c, json).await,
        GroupCmd::New { name, sessions, dir, cwd, json } => {
            let dir = dir.map(|d| match d {
                DirArg::Row => SplitDir::Row,
                DirArg::Col => SplitDir::Col,
            });
            groups::new(c, groups::NewGroup { name, sessions, dir, cwd }, json).await
        }
        GroupCmd::Add { group, session, split, of } => {
            groups::add(c, &group, &session, groups::Place { split, of }).await
        }
        GroupCmd::Rm { group, session } => groups::rm(c, &group, &session).await,
        GroupCmd::Rename { group, name } => groups::rename(c, &group, &name).await,
        GroupCmd::Delete { groups: ids } => groups::delete(c, &ids).await,
    }
}

/// Connects, with a plain message when no daemon listens.
pub async fn connect(socket: &std::path::Path) -> Result<Client> {
    if !socket.exists() {
        anyhow::bail!("skiffd is not running (no socket at {})", socket.display());
    }
    Client::connect(socket)
        .await
        .with_context(|| format!("skiffd is not running at {}", socket.display()))
}

/// Refuse a different protocol; warn once when only the daemon version differs.
async fn check_daemon(c: Client) -> Result<Client> {
    let info = c.daemon_info().await?;
    if info.protocol != skiff_core::PROTOCOL {
        let cli = c.daemon_pid
            .and_then(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).ok())
            .map(|p| p.with_file_name("skiff"));
        let matching = cli.map_or_else(|| "unknown".to_string(), |p| p.display().to_string());
        anyhow::bail!(
            "skiffd is {} (protocol {}), and this skiff was built for skiffd {} (protocol {}). Use the matching CLI: {}",
            info.version, info.protocol, skiff_core::VERSION, skiff_core::PROTOCOL, matching
        );
    }
    if info.version != skiff_core::VERSION {
        static WARNING: std::sync::Once = std::sync::Once::new();
        WARNING.call_once(|| eprintln!(
            "skiff: warning: skiffd is {}, and this skiff was built for skiffd {}",
            info.version, skiff_core::VERSION
        ));
    }
    Ok(c)
}
