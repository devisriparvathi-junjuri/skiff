//! A headless terminal per session. It keeps the screen and scrollback so a
//! client that attaches late can draw what it missed.

use std::{
    fmt::Write as _,
    hash::{DefaultHasher, Hash, Hasher},
};

use alacritty_terminal::{
    grid::{Dimensions, Grid},
    index::{Column, Line},
    term::{
        cell::{Cell, Flags},
        test::TermSize,
        Config, Term, TermMode, TermSave, TermSaveOwned,
    },
    vte::ansi::{Color, NamedColor, Processor, ProcessorSave},
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// Lines of history the daemon keeps per session.
pub const SCROLLBACK: usize = 2000;

mod signals;

mod inspect;

mod snapshot;

pub use inspect::{has_busy_line, title_state, TitleState};

pub use signals::{Signals, StatusSignal};
use signals::{held_probes, Listener};

pub struct Screen {
    pub(in crate::screen) term: Term<Listener>,
    parser: Processor,
    signals: Listener,
    /// Hash of the text at the last `text_changed`.
    text: u64,
    /// Probes answered from a synchronized update's held bytes, not parsed
    /// yet, and how far those bytes were searched. See [`Self::feed`].
    early: u32,
    held_from: usize,
}

/// The whole terminal for a reload: both screens and their history, cursors,
/// modes, keyboard stacks, and the parser in the middle of a sequence.
#[derive(Serialize)]
struct SaveRef<'a> {
    term: TermSave<&'a Grid<Cell>>,
    parser: ProcessorSave,
    text: u64,
}

/// [`SaveRef`] as it reads back. Same fields, same order.
#[derive(Deserialize)]
struct SaveOwned {
    term: TermSaveOwned,
    parser: ProcessorSave,
    text: u64,
}

fn config() -> Config {
    Config {
        scrolling_history: SCROLLBACK,
        // Track the program's kitty keyboard stack, so a snapshot and a
        // reload carry it. The daemon encodes no keys, and answers no query
        // but OSC 7501's probe (see `Session::push_output`).
        kitty_keyboard: true,
        ..Config::default()
    }
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        let signals = Listener::default();
        Self {
            term: Term::new(config(), &size(cols, rows), signals.clone()),
            parser: Processor::new(),
            signals,
            text: 0,
            early: 0,
            held_from: 0,
        }
    }

    /// Everything [`Self::restore`] needs to carry on as this screen would.
    pub fn save(&self) -> Result<Vec<u8>> {
        let save = SaveRef {
            term: self.term.save(),
            parser: self.parser.save(),
            text: self.text,
        };
        Ok(postcard::to_stdvec(&save)?)
    }

    pub fn restore(bytes: &[u8]) -> Result<Self> {
        let save: SaveOwned = postcard::from_bytes(bytes)?;
        let signals = Listener::default();
        let parser = Processor::restore(save.parser);
        // The daemon that saved answered the probes an open update holds.
        // Before it did so, the DA1 reply after such a probe had long gone.
        let (early, held_from) = held_probes(parser.sync_bytes());
        Ok(Self {
            term: Term::restore(save.term, config(), signals.clone()).map_err(|e| anyhow!("terminal state: {e}"))?,
            parser,
            signals,
            text: save.text,
            early,
            held_from,
        })
    }

    /// The program drew on the alternate screen: it is full-screen.
    pub fn alt(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// The program wants pastes marked with ESC [200~ and ESC [201~.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Signals {
        self.parser.advance(&mut self.term, bytes);
        self.take_signals()
    }

    /// When an open synchronized update (`?2026h`) times out: the program
    /// sent no `?2026l` in time, as when it dies mid-frame. `None` while
    /// none is open. The deadline is the parser's own, which alacritty's
    /// event loop waits on too.
    pub fn sync_due(&self) -> Option<std::time::Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Ends the open synchronized update, as alacritty's event loop does at
    /// its timeout: the held bytes apply now, with what they signal.
    pub fn end_sync(&mut self) -> Signals {
        self.parser.stop_sync(&mut self.term);
        self.take_signals()
    }

    /// What the parser signalled since the last call. A probe answered while
    /// a synchronized update held it is not answered again.
    fn take_signals(&mut self) -> Signals {
        let mut s = std::mem::take(&mut *self.signals.0.lock().unwrap());
        let parsed = s
            .status
            .iter()
            .filter(|x| matches!(x, StatusSignal::Report(b) if crate::program_status::is_probe(b)))
            .count() as u32;
        let answered = parsed.min(self.early);
        self.early -= answered;
        s.probes = parsed - answered;
        // A synchronized update holds its bytes until it ends, but a client
        // answers the DA1 after a probe at once: the probe is answered now,
        // so its reply still comes first.
        let held = self.parser.sync_bytes();
        if held.is_empty() {
            self.early = 0;
            self.held_from = 0;
        } else {
            if self.held_from > held.len() {
                self.held_from = 0;
            }
            let (n, searched) = held_probes(&held[self.held_from..]);
            self.held_from += searched;
            self.early += n;
            s.probes += n;
        }
        s
    }

    /// True when the characters on screen or the scrollback changed since the
    /// last call. Colors and cursor moves do not count, so an idle program
    /// that animates a logo's colors is not at work.
    pub fn text_changed(&mut self) -> bool {
        let grid = self.term.grid();
        let mut h = DefaultHasher::new();
        grid.history_size().hash(&mut h);
        for line in 0..grid.screen_lines() as i32 {
            let row = &grid[Line(line)];
            for c in 0..grid.columns() {
                row[Column(c)].c.hash(&mut h);
            }
        }
        let text = h.finish();
        let changed = text != self.text;
        self.text = text;
        changed
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(size(cols, rows));
    }

}


fn size(cols: u16, rows: u16) -> TermSize {
    TermSize::new(cols.max(1) as usize, rows.max(1) as usize)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_only_redraw_is_not_a_text_change() {
        let mut s = Screen::new(20, 4);
        s.feed(b"\x1b[1;1H\x1b[38;2;1;2;3mlogo");
        assert!(s.text_changed());
        s.feed(b"\x1b[1;1H\x1b[38;2;9;9;9mlogo");
        assert!(!s.text_changed());
        s.feed(b"\x1b[1;1Hlogs");
        assert!(s.text_changed());
    }

    #[test]
    fn title_bel_is_not_a_bell() {
        let mut s = Screen::new(20, 4);
        let sig = s.feed(b"\x1b]0;fix worktrees\x07$ ");
        assert!(!sig.bell);
        assert_eq!(sig.title, Some(Some("fix worktrees".into())));
        assert!(s.feed(b"\x07").bell);
    }

    /// A synchronized update the program never ends times out, as in
    /// alacritty: its frame, its title and its report apply then. A probe
    /// in it is answered once. A save carries the open update. An update
    /// that ends applies at once, as before.
    #[test]
    fn a_synchronized_update_left_open_ends_at_its_timeout() {
        let frozen = b"\x1b[?2026h\x1b]0;Frozen\x07\x1b]7501;state=working\x07\x1b]7501;?\x07hello";
        let mut screen = Screen::new(80, 24);
        let held = screen.feed(frozen);
        assert_eq!((held.title, held.status.len(), held.probes), (None, 0, 1));
        let due = screen.sync_due().expect("no deadline");
        let wait = due.saturating_duration_since(std::time::Instant::now());
        assert!(wait <= std::time::Duration::from_millis(150), "{wait:?}");
        // A reload in the middle keeps the update and its deadline.
        let mut screen = Screen::restore(&screen.save().unwrap()).unwrap();
        assert!(screen.sync_due().is_some());
        assert!(!screen.tail_text(None, 3).concat().contains("hello"));
        let ended = screen.end_sync();
        assert_eq!(ended.title, Some(Some("Frozen".into())));
        assert_eq!(
            ended.status,
            [
                StatusSignal::Report(b"state=working".to_vec()),
                StatusSignal::Report(b"?".to_vec())
            ]
        );
        assert_eq!(
            ended.probes, 0,
            "the probe was answered twice across the reload"
        );
        assert!(screen.tail_text(None, 3).concat().contains("hello"));
        assert_eq!(screen.sync_due(), None);

        let mut screen = Screen::new(80, 24);
        let whole = screen.feed(b"\x1b[?2026h\x1b]0;Whole\x07hi\x1b[?2026l");
        assert_eq!(
            (whole.title, screen.sync_due()),
            (Some(Some("Whole".into())), None)
        );
    }

    /// A probe in a synchronized update is answered while the update holds
    /// it, once, even split across reads; one outside is answered as parsed.
    #[test]
    fn a_probe_held_in_a_synchronized_update_is_answered_at_once() {
        let mut screen = Screen::new(80, 24);
        assert_eq!(screen.feed(b"\x1b]7501;?\x07").probes, 1);
        let held = screen.feed(b"\x1b[?2026h\x1b]7501");
        assert_eq!((held.probes, held.status.len()), (0, 0));
        assert_eq!(screen.feed(b";?\x1b").probes, 0);
        let held = screen.feed(b"\\\x1b[c x");
        assert_eq!((held.probes, held.status.len()), (1, 0));
        let done = screen.feed(b"\x1b[?2026l");
        assert_eq!(done.probes, 0, "answered twice");
        assert_eq!(done.status, [StatusSignal::Report(b"?".to_vec())]);
        assert_eq!(screen.feed(b"\x1b]7501;?\x1b\\").probes, 1);
    }

    /// The queries a client answers and this terminal does not are counted,
    /// so their replies are not taken for typing.
    #[test]
    fn xtversion_and_decrqss_are_open_queries() {
        let mut screen = Screen::new(80, 24);
        assert_eq!(screen.feed(b"\x1b[>q").queries, 1);
        assert_eq!(screen.feed(b"\x1b[>0q").queries, 1);
        assert_eq!(screen.feed(b"\x1bP$qm\x1b\\").queries, 1);
        assert_eq!(screen.feed(b"\x1b[ q\x1bPq#0\x1b\\").queries, 0);
    }

    #[test]
    fn osc_7501_is_heard_in_order_across_reads() {
        use super::StatusSignal::{Report, Reset};
        let mut s = Screen::new(20, 4);
        // Ended by BEL, and by ST split across three reads.
        let sig = s.feed(b"a\x1b]7501;?\x07b\x1b]75");
        assert_eq!(sig.status, [Report(b"?".to_vec())]);
        assert!(!sig.bell, "the BEL that ends OSC 7501 is not a bell");
        assert!(s.feed(b"01;state=work").status.is_empty());
        // The ESC of ST ends the sequence.
        assert_eq!(
            s.feed(b"ing:app=codex\x1b").status,
            [Report(b"state=working:app=codex".to_vec())]
        );
        assert!(s.feed(b"\\c").status.is_empty());
        // A full reset comes in its place among the reports.
        let sig = s.feed(b"\x1b]7501;state=idle\x1b\\\x1bc\x1b]7501;state=done\x07");
        assert_eq!(
            sig.status,
            [
                Report(b"state=idle".to_vec()),
                Reset,
                Report(b"state=done".to_vec())
            ]
        );
        // Other OSC numbers are not reports.
        assert!(s.feed(b"\x1b]75011;x\x07\x1b]0;t\x07").status.is_empty());
    }


    #[test]
    fn restore_carries_on_where_the_save_stopped() {
        let mut s = Screen::new(20, 4);
        s.feed(b"p-1\r\np-2\r\np-3\r\np-4\r\np-5\x1b[2;3r\x1b[?2004h\x1b[?1002h\x1b[>1u\x1b[?1049h\x1b[>5uALT\x1b[3");
        let mut back = Screen::restore(&s.save().unwrap()).unwrap();
        assert_eq!(back.snapshot(), s.snapshot());
        // The parser finishes the sequence the save cut in half.
        back.feed(b"1mRED\x1b[0m");
        let snap = String::from_utf8(back.snapshot()).unwrap();
        assert!(snap.contains("ALT\x1b[0;31mRED"), "{snap:?}");
        assert!(snap.contains("p-1") && snap.contains("\x1b[=5;1u"), "{snap:?}");
        // The primary screen keeps its history and its own kitty flags.
        back.feed(b"\x1b[?1049l");
        let snap = String::from_utf8(back.snapshot()).unwrap();
        assert!(snap.contains("p-1\r\np-2") && !snap.contains("ALT"), "{snap:?}");
        assert!(snap.contains("\x1b[=1;1u") && snap.contains("\x1b[?2004h\x1b[?1002h"), "{snap:?}");
    }

    #[test]
    fn postcard_fixture_restores_a_saved_screen() {
        // Pre-refactor postcard image of a 2-by-1 screen containing `A` and
        // a parser stopped after ESC [. It pins SaveRef/SaveOwned field order.
        const FIXTURE: &[u8] = &[0, 1, 2, 1, 65, 0, 16, 0, 17, 0, 0, 1, 32, 0, 16, 0, 17, 0, 0, 1, 0, 1, 1, 2, 1, 0, 208, 15, 0, 1, 1, 32, 0, 16, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 32, 0, 16, 0, 17, 0, 0, 0, 0, 0, 0, 0, 1, 2, 1, 32, 0, 16, 0, 17, 0, 0, 1, 32, 0, 16, 0, 17, 0, 0, 0, 0, 1, 1, 2, 1, 0, 0, 0, 0, 1, 32, 0, 16, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 32, 0, 16, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 129, 129, 10, 0, 2, 141, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 65, 0, 0, 0];
        let mut restored = Screen::restore(FIXTURE).unwrap();
        restored.feed(b"31mB");
        assert!(String::from_utf8(restored.snapshot()).unwrap().contains("A\x1b[0;31mB"));
    }

}
