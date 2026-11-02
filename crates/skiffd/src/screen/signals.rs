use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener};

/// What one `feed` saw besides screen changes.
#[derive(Default, Debug, PartialEq)]
pub struct Signals {
    /// A real BEL, not the terminator of an OSC sequence.
    pub bell: bool,
    /// `Some(None)` when the program reset its title.
    pub title: Option<Option<String>>,
    /// OSC 7501 bodies and full resets, in the order they came.
    pub status: Vec<StatusSignal>,
    /// Queries the program sent that a terminal answers (DA1, DSR, colors,
    /// XTVERSION, DECRQSS). A client answers them; see `session::replies`.
    pub queries: u32,
    /// OSC 7501 probes to answer now: those parsed, and those a synchronized
    /// update still holds. A held one is not answered again once parsed.
    pub probes: u32,
}

impl Signals {
    /// These, then `later`'s: two feeds in a row as one.
    pub fn then(mut self, later: Signals) -> Signals {
        self.bell |= later.bell;
        if later.title.is_some() {
            self.title = later.title;
        }
        let room = MAX_STATUS.saturating_sub(self.status.len());
        self.status.extend(later.status.into_iter().take(room));
        self.queries = self.queries.saturating_add(later.queries);
        self.probes = self.probes.saturating_add(later.probes);
        self
    }
}

/// What the program status records hear from the emulator.
#[derive(Debug, PartialEq)]
pub enum StatusSignal {
    /// An OSC 7501 body, unread. See `program_status::parse`.
    Report(Vec<u8>),
    /// A full reset (RIS): every record goes.
    Reset,
}

/// Whole OSC 7501 probes in `b`, and how far `b` is searched: up to the
/// start of one that is not whole yet.
pub(super) fn held_probes(b: &[u8]) -> (u32, usize) {
    const PROBE: &[u8] = b"\x1b]7501;?";
    let (mut n, mut at) = (0, 0);
    loop {
        let Some(i) = b[at..].windows(PROBE.len()).position(|w| w == PROBE) else {
            return (n, b.len().saturating_sub(PROBE.len() - 1).max(at));
        };
        let start = at + i;
        let end = start + PROBE.len();
        match (b.get(end), b.get(end + 1)) {
            (Some(0x07), _) => (n, at) = (n + 1, end + 1),
            (Some(0x1b), Some(b'\\')) => (n, at) = (n + 1, end + 2),
            (None, _) | (Some(0x1b), None) => return (n, start),
            _ => at = end,
        }
    }
}

/// OSC 7501 bodies kept from one feed. A program that floods more is cut off;
/// a reset always counts.
const MAX_STATUS: usize = 64;

#[derive(Clone, Default)]
pub(super) struct Listener(pub(super) Arc<Mutex<Signals>>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let mut s = self.0.lock().unwrap();
        match event {
            TermEvent::Bell => s.bell = true,
            TermEvent::Title(t) => s.title = Some(Some(t)),
            TermEvent::ResetTitle => s.title = Some(None),
            TermEvent::ProgramStatus(body, _) if s.status.len() < MAX_STATUS => {
                s.status.push(StatusSignal::Report(body))
            }
            TermEvent::Reset if s.status.last() != Some(&StatusSignal::Reset) => {
                s.status.push(StatusSignal::Reset)
            }
            TermEvent::PtyWrite(_)
            | TermEvent::ColorRequest(..)
            | TermEvent::TextAreaSizeRequest(_)
            | TermEvent::UnansweredQuery => s.queries = s.queries.saturating_add(1),
            _ => {}
        }
    }
}
