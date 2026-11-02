use skiff_core::session::{SessionInfo, SessionState};

use crate::print;

pub struct Options {
    /// The Skiff agent's session.
    pub skipper: String,
    pub notify: bool,
}


/// What a ledger pane is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Now {
    Working,
    Waiting,
    Idle { unread: bool },
    /// The program, or the agent in front of the launch shell, ended.
    Exit,
    Gone,
    /// The pane restarted since the task was sent.
    Interrupted,
}

impl Now {
    /// `turned`: the daemon marked a turn's end since the last step, though
    /// the app may have marked it seen already.
    pub(super) fn of(s: Option<&SessionInfo>, turned: bool) -> Now {
        let Some(s) = s else { return Now::Gone };
        match s.state {
            SessionState::Done => Now::Exit,
            _ if s.agent_exit.is_some() => Now::Exit,
            SessionState::Working => Now::Working,
            SessionState::Waiting => Now::Waiting,
            SessionState::Idle => Now::Idle { unread: s.unread || turned },
        }
    }
}

/// What the watcher tells.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fact {
    Waiting,
    Idle,
    /// Idle, with a pending task for the pane.
    Pending,
    Exit,
    Gone,
    Interrupted,
    /// Gone, and skiff set its open tasks done.
    Closed,
    /// A project Skipper set its hand-off done.
    Reported,
}

impl Fact {
    pub(super) fn phrase(self) -> &'static str {
        match self {
            Fact::Waiting => "is waiting",
            Fact::Idle | Fact::Pending => "is idle",
            Fact::Exit => "exited",
            Fact::Gone | Fact::Closed => "is gone",
            Fact::Interrupted => "restarted",
            Fact::Reported => "reported",
        }
    }
}

/// What this run saw of one ledger pane. Lost on a restart, by design: the
/// unread flag and the pending rows then bring back what still needs
/// attention.
#[derive(Default)]
pub struct Pane {
    /// The last fact told, so a fact is told once.
    pub(super) told: Option<Fact>,
    pub(super) name: String,
    /// The pane showed a prompt and has not been told idle since.
    pub(super) waited: bool,
    /// The pending task told for this idle stretch.
    pub(super) pending_told: Option<String>,
    /// The pane's generation when last seen. See [`SessionInfo::generation`].
    pub(super) generation: Option<u64>,
}

impl Pane {
    /// The fact to tell now, if any. Idle counts after work: the daemon marks
    /// the end of a turn unread, and it stays so until the agent or the
    /// person marks it seen. A start-up screen is no turn. Idle also counts
    /// once after a prompt, answered or dismissed, though no turn followed:
    /// the pane can take the next step.
    pub(super) fn observe(&mut self, now: Now) -> Option<Fact> {
        let fact = match now {
            Now::Working => {
                self.told = None;
                return None;
            }
            // The prompt left the screen. Told though an older idle was.
            Now::Idle { .. } if self.waited => {
                self.waited = false;
                self.told = Some(Fact::Idle);
                return Some(Fact::Idle);
            }
            Now::Idle { unread: false } => return None,
            Now::Idle { .. } => Fact::Idle,
            Now::Waiting => {
                self.waited = true;
                Fact::Waiting
            }
            Now::Exit => Fact::Exit,
            Now::Gone => Fact::Gone,
            Now::Interrupted => Fact::Interrupted,
        };
        if self.told == Some(fact) {
            return None;
        }
        self.told = Some(fact);
        if fact != Fact::Waiting {
            self.waited = false;
        }
        Some(fact)
    }
}

/// One line for the agent, held until it can take it.
#[derive(Debug, Clone, PartialEq)]
pub struct Wake {
    pub fact: Fact,
    pub pane: String,
    pub name: String,
    pub id: String,
    /// The task's uid, for an interrupted or a pending task. For a gone
    /// pane, its open uids, or the uids skiff closed, joined with ", ".
    pub task: Option<String>,
    /// An idle pane whose program reported its turn ended in an error (OSC 7501).
    pub failed: bool,
}

impl Wake {
    pub(super) fn phrase(&self) -> &'static str {
        if self.failed { "failed" } else { self.fact.phrase() }
    }

    pub(super) fn tail(&self) -> String {
        match (&self.task, self.fact) {
            (Some(t), Fact::Pending) => format!(": task {t} is pending: skiff ledger send {t} --enter"),
            (Some(t), Fact::Gone) if t.contains(", ") => format!(": tasks {t} are open"),
            (Some(t), Fact::Gone) => format!(": task {t} is open"),
            (Some(t), Fact::Closed) if t.contains(", ") => format!(": tasks {t} are done with it"),
            (Some(t), Fact::Closed) => format!(": task {t} is done with it"),
            (Some(t), _) => format!(": task {t} is interrupted"),
            (None, _) => String::new(),
        }
    }
}

/// The one line the agent gets for all held wakes.
pub fn wake_line(wakes: &[Wake]) -> String {
    let told_fact = |w: &&Wake| matches!(w.fact, Fact::Closed | Fact::Reported);
    let parts: Vec<String> = wakes
        .iter()
        .filter(|w| !told_fact(w))
        .chain(wakes.iter().filter(told_fact))
        .map(|w| {
            if w.fact == Fact::Reported {
                return format!("the {} reported hand-off {}", w.name, w.task.as_deref().unwrap_or_default());
            }
            let who = if w.name.is_empty() { String::new() } else { format!("{} ", w.name) };
            format!("pane {who}{} {}{}", print::short_id(&w.id), w.phrase(), w.tail())
        })
        .collect();
    // A task skiff closed, and a hand-off a project Skipper reported, is
    // told to the person, then reported.
    let closed: Vec<&str> = wakes.iter().filter(told_fact).filter_map(|w| w.task.as_deref()).collect();
    let read = if wakes.iter().any(|w| w.fact == Fact::Reported) { "Read its result. " } else { "" };
    let told = (!closed.is_empty()).then(|| format!("{read}Tell the person, then run skiff ledger report {}.", closed.join(" ").replace(',', "")));
    let wakes: Vec<&Wake> = wakes.iter().filter(|w| !told_fact(w)).collect();
    if wakes.is_empty() {
        return format!("[watch] {}. {}", parts.join("; "), told.unwrap_or_default());
    }
    // A gone pane cannot be marked seen. Its tasks are reassigned or marked done.
    let gone = |w: &&&Wake| w.fact == Fact::Gone && w.task.is_some();
    let lost = wakes.iter().filter(gone).count();
    let tail = if lost == 0 {
        if wakes.len() == 1 { "Read it, then mark it seen." } else { "Read them, then mark each seen." }
    } else if lost < wakes.len() {
        "Read the others, then mark each seen. Reassign each task of a gone pane or mark it done."
    } else if lost == 1 && !wakes.iter().any(|w| w.task.as_deref().is_some_and(|t| t.contains(", "))) {
        "Reassign it or mark it done."
    } else {
        "Reassign each task or mark it done."
    };
    match told {
        Some(t) => format!("[watch] {}. {tail} {t}", parts.join("; ")),
        None => format!("[watch] {}. {tail}", parts.join("; ")),
    }
}
