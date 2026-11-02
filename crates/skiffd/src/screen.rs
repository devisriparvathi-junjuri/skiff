//! A headless terminal per session. It keeps the screen and scrollback so a
//! client that attaches late can draw what it missed.

use std::{
    fmt::Write as _,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, Mutex},
};

use alacritty_terminal::{
    event::{Event as TermEvent, EventListener},
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
fn held_probes(b: &[u8]) -> (u32, usize) {
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
struct Listener(Arc<Mutex<Signals>>);

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

pub struct Screen {
    term: Term<Listener>,
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

    /// The last `n` rows of the screen that hold text, joined, in lower case
    /// and without white space. A narrow pane wraps a line anywhere, so the
    /// match ignores where the words break.
    fn bottom_text(&self, n: usize) -> String {
        let grid = self.term.grid();
        let mut rows = Vec::with_capacity(n);
        for line in (0..grid.screen_lines() as i32).rev() {
            let row = &grid[Line(line)];
            let text: String = (0..grid.columns())
                .map(|c| row[Column(c)].c)
                .filter(|c| !c.is_whitespace() && *c != '\0')
                .flat_map(char::to_lowercase)
                .collect();
            if !text.is_empty() {
                rows.push(text);
                if rows.len() == n {
                    break;
                }
            }
        }
        rows.reverse();
        rows.concat()
    }

    /// Whether `agent` shows a prompt that stops its turn until the user
    /// answers: an approval, a question, or the trust check at start. The
    /// agents ring no bell for it, so the daemon reads the screen.
    ///
    /// Each agent draws the prompt in place of its input box, with a line of
    /// key hints at the bottom of the screen. The match reads only those last
    /// rows, so the same words in the conversation above do not count.
    pub fn approval_prompt(&self, agent: &str) -> bool {
        match agent {
            // "Esc to cancel · Tab to amend", "Enter to confirm · Esc to cancel".
            "claude" => self.bottom_text(2).contains("esctocancel"),
            "codex" => {
                let t = self.bottom_text(2);
                t.contains("pressentertoconfirmoresctocancel") || t.contains("entercontinue·escskip")
            }
            // "Allow once   Allow always   Reject".
            "opencode" => {
                let t = self.bottom_text(4);
                t.contains("allowonce") && t.contains("reject")
            }
            "gemini" => {
                // Antigravity (agy): "↑/↓ Navigate · tab Amend", "↑/↓ Navigate · enter Confirm".
                // Its slash-command menu shows "↑/↓ Navigate · enter Select · tab Complete".
                // Gemini CLI: "Allow once", "Allow for this session", "No, suggest changes (esc)".
                let t = self.bottom_text(10);
                (t.contains("↑/↓navigate")
                    && (t.contains("tabamend") || t.contains("enterconfirm")))
                    || (t.contains("allowonce") && t.contains("suggestchanges"))
            }
            "grok" => {
                // "1/4:select │ Tab:next option │ ...", or a command's
                // "Allow once", "Always allow this command", "Reject".
                let t = self.bottom_text(10);
                let hints = self.bottom_text(2);
                (hints.contains(":select") && hints.contains("tab:nextoption"))
                    || (t.contains("allowonce") && t.contains("reject"))
            }
            _ => false,
        }
    }

    /// The agent shows the line it keeps up while it works, above its input
    /// box. Proof of a turn, however short. Grok and Hermes have no such line known yet.
    pub fn busy_line(&self, agent: &str) -> bool {
        if !has_busy_line(agent) {
            return false;
        }
        let t = self.bottom_text(6);
        match agent {
            // "esc to interrupt", "• Working (2s • esc to interrupt)". The key
            // follows the user's keymap, so the match leaves it out.
            "claude" | "codex" => t.contains("tointerrupt"),
            // "esc interrupt", and "esc again to interrupt" after one Esc.
            "opencode" => t.contains("escinterrupt") || t.contains("tointerrupt"),
            // "(esc to cancel, 12s)". Antigravity shows "esc to cancel" at
            // its prompts and its slash-command menu too, under a "↑/↓ Navigate" list.
            "gemini" => t.contains("esctocancel") && !t.contains("↑/↓navigate"),
            _ => false,
        }
    }

    /// The last `n` rows of text above the agent's input box, for mission
    /// control (#88). Reads the scrollback too, so a short screen still
    /// fills. An approval prompt replaces the input box, so then nothing is
    /// cut. Trailing blank rows are dropped.
    pub fn tail_text(&self, agent: Option<&str>, n: usize) -> Vec<String> {
        let grid = self.term.grid();
        let rows: Vec<String> = (grid.topmost_line().0..grid.screen_lines() as i32)
            .map(|l| row_text(&grid[Line(l)], grid.columns()))
            .collect();
        let screen_top = rows.len() - grid.screen_lines();
        let mut end = rows.len();
        if let Some(agent) = agent.filter(|a| !self.approval_prompt(a)) {
            if let Some(cut) = input_box_top(&rows[screen_top..], agent, grid.columns()) {
                end = screen_top + cut;
            }
        }
        while end > 0 && rows[end - 1].is_empty() {
            end -= 1;
        }
        rows[end.saturating_sub(n)..end].to_vec()
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(size(cols, rows));
    }

    /// Bytes that redraw scrollback, screen, cursor, and input modes on a
    /// terminal of the same size. Starts with a full reset. The redraw sits
    /// inside a synchronized update, so the client shows it in one frame
    /// instead of a blank or half-drawn screen.
    pub fn snapshot(&self) -> Vec<u8> {
        let grid = self.term.grid();
        let mode = *self.term.mode();
        let mut out = String::with_capacity(64 * 1024);
        // The reset ends any synchronized update, so the new one starts after it.
        out.push_str("\x1bc\x1b[?2026h");
        if mode.contains(TermMode::ALT_SCREEN) {
            // The primary screen and its history go first, so the client
            // still has them when the program leaves the alternate screen.
            let primary = self.term.inactive_grid();
            draw(&mut out, primary);
            let cursor = primary.cursor.point;
            let _ = write!(out, "\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1);
            out.push_str("\x1b[?1049h\x1b[H");
        }
        draw(&mut out, grid);

        let cursor = grid.cursor.point;
        let _ = write!(out, "\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1);
        for (flag, set, unset) in [
            (TermMode::APP_CURSOR, "\x1b[?1h", ""),
            (TermMode::APP_KEYPAD, "\x1b=", ""),
            (TermMode::BRACKETED_PASTE, "\x1b[?2004h", ""),
            (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h", ""),
            (TermMode::MOUSE_DRAG, "\x1b[?1002h", ""),
            (TermMode::MOUSE_MOTION, "\x1b[?1003h", ""),
            (TermMode::SGR_MOUSE, "\x1b[?1006h", ""),
            (TermMode::UTF8_MOUSE, "\x1b[?1005h", ""),
            (TermMode::FOCUS_IN_OUT, "\x1b[?1004h", ""),
            (TermMode::LINE_WRAP, "", "\x1b[?7l"),
            (TermMode::SHOW_CURSOR, "", "\x1b[?25l"),
        ] {
            out.push_str(if mode.contains(flag) { set } else { unset });
        }
        // Kitty keyboard flags, in the protocol's bit order.
        let kitty = (mode & TermMode::KITTY_KEYBOARD_PROTOCOL).bits() >> TermMode::DISAMBIGUATE_ESC_CODES.bits().trailing_zeros();
        if kitty != 0 {
            let _ = write!(out, "\x1b[={kitty};1u");
        }
        out.push_str("\x1b[?2026l");
        out.into_bytes()
    }
}

/// Writes the history and screen of `grid`, from the top, with their colors.
fn draw(out: &mut String, grid: &Grid<Cell>) {
    let cols = grid.columns();
    let top = grid.topmost_line().0;
    let bottom = grid.bottommost_line().0;
    let mut pen = Pen::default();
    for line in top..=bottom {
        let row = &grid[Line(line)];
        let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        // A wrapped row must fill every column so the next row wraps onto it.
        let end = if wrapped {
            cols
        } else {
            (0..cols)
                .rev()
                .find(|&c| !is_blank(&row[Column(c)]))
                .map_or(0, |c| c + 1)
        };
        for c in 0..end {
            let cell = &row[Column(c)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            pen.set(out, cell);
            out.push(if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c });
            if let Some(zw) = cell.zerowidth() {
                out.extend(zw);
            }
        }
        if !wrapped && line != bottom {
            pen.reset(out);
            out.push_str("\r\n");
        }
    }
    pen.reset(out);
}

/// What an agent's window title says about its turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TitleState {
    Busy,
    /// The agent waits for the user's answer.
    Action,
    Ready,
}

/// Reads the state an agent puts in its window title. `None` when the agent
/// has no known title state, or this title carries none.
///
/// - Claude Code: "◐ " or "◑ " while busy, "✳ " otherwise.
/// - Codex: a braille spinner while busy, "[ ! ] Action Required" for an
///   answer. Its default title has the spinner first.
/// - Gemini CLI: "✦ " or "⏲ " while busy, "✋ " for an answer, "◇ " when ready.
pub fn title_state(agent: &str, title: &str) -> Option<TitleState> {
    let first = title.trim_start().chars().next()?;
    match agent {
        "claude" => match first {
            '\u{25d0}'..='\u{25d3}' => Some(TitleState::Busy),
            '\u{2733}' => Some(TitleState::Ready),
            _ => None,
        },
        "codex" => {
            if title.contains("Action Required") {
                Some(TitleState::Action)
            } else if title.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)) {
                Some(TitleState::Busy)
            } else {
                Some(TitleState::Ready)
            }
        }
        "gemini" => match first {
            '\u{2726}' | '\u{23f2}' => Some(TitleState::Busy),
            '\u{270b}' => Some(TitleState::Action),
            '\u{25c7}' => Some(TitleState::Ready),
            _ => None,
        },
        // "⚠ Action Required - ⠋ - Running: … - grok" at a prompt,
        // "⠋ - Thinking - <topic> - grok" while it works, "<topic> - grok" after.
        "grok" => {
            if title.contains("Action Required") {
                Some(TitleState::Action)
            } else if ('\u{2801}'..='\u{28ff}').contains(&first) {
                Some(TitleState::Busy)
            } else {
                Some(TitleState::Ready)
            }
        }
        _ => None,
    }
}

/// The agents whose busy line `Screen::busy_line` knows. For them, only that
/// line makes a turn: text they print without it (a recap, a status line) is not one.
pub fn has_busy_line(agent: &str) -> bool {
    matches!(agent, "claude" | "codex" | "opencode" | "gemini")
}

/// A row's characters, trailing spaces dropped.
fn row_text(row: &alacritty_terminal::grid::Row<Cell>, cols: usize) -> String {
    let mut s = String::with_capacity(cols);
    for c in 0..cols {
        let cell = &row[Column(c)];
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        s.push(if cell.c == '\0' { ' ' } else { cell.c });
        if let Some(zw) = cell.zerowidth() {
            s.extend(zw);
        }
    }
    s.truncate(s.trim_end().len());
    s
}

/// Where the agent's input box starts among the screen's rows, if it shows.
/// Codex marks its input line with `›`. The others frame the box with a
/// line of box-drawing characters (Claude Code's `───`, Gemini's `╭──╮`);
/// the topmost such line near the bottom is the box's top.
fn input_box_top(rows: &[String], agent: &str, cols: usize) -> Option<usize> {
    let from = rows.len().saturating_sub(12);
    let near = rows.iter().enumerate().skip(from);
    if agent == "codex" {
        return near.filter(|(_, r)| r.trim_start().starts_with('›')).map(|(i, _)| i).next_back();
    }
    let rule = |r: &str| {
        let t = r.trim();
        let n = t.chars().count();
        n * 2 >= cols && t.chars().filter(|c| ('\u{2500}'..='\u{257f}').contains(c)).count() * 10 >= n * 8
    };
    near.filter(|(_, r)| rule(r)).map(|(i, _)| i).next()
}

fn size(cols: u16, rows: u16) -> TermSize {
    TermSize::new(cols.max(1) as usize, rows.max(1) as usize)
}

fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(Flags::INVERSE | Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
        && cell.zerowidth().is_none()
}

/// The SGR state last written, so each cell only emits what changed.
/// `None` means the default attributes.
#[derive(Default)]
struct Pen {
    attrs: Option<(Color, Color, Flags)>,
}

const SGR_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::STRIKEOUT);

impl Pen {
    fn reset(&mut self, out: &mut String) {
        if self.attrs.take().is_some() {
            out.push_str("\x1b[0m");
        }
    }

    fn set(&mut self, out: &mut String, cell: &Cell) {
        let want = (cell.fg, cell.bg, cell.flags & SGR_FLAGS);
        let default = (
            Color::Named(NamedColor::Foreground),
            Color::Named(NamedColor::Background),
            Flags::empty(),
        );
        if want == default {
            self.reset(out);
            return;
        }
        if self.attrs == Some(want) {
            return;
        }
        self.attrs = Some(want);
        out.push_str("\x1b[0");
        let f = want.2;
        for (flag, code) in [
            (Flags::BOLD, ";1"),
            (Flags::DIM, ";2"),
            (Flags::ITALIC, ";3"),
            (Flags::INVERSE, ";7"),
            (Flags::STRIKEOUT, ";9"),
        ] {
            if f.contains(flag) {
                out.push_str(code);
            }
        }
        if f.contains(Flags::UNDERCURL) {
            out.push_str(";4:3");
        } else if f.contains(Flags::DOUBLE_UNDERLINE) {
            out.push_str(";21");
        } else if f.contains(Flags::DOTTED_UNDERLINE) {
            out.push_str(";4:4");
        } else if f.contains(Flags::DASHED_UNDERLINE) {
            out.push_str(";4:5");
        } else if f.contains(Flags::UNDERLINE) {
            out.push_str(";4");
        }
        color(out, want.0, 30);
        color(out, want.1, 40);
        out.push('m');
    }
}

/// `base` is 30 for foreground, 40 for background.
fn color(out: &mut String, c: Color, base: u8) {
    match c {
        Color::Spec(rgb) => {
            let _ = write!(out, ";{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b);
        }
        Color::Indexed(i) => {
            let _ = write!(out, ";{};5;{}", base + 8, i);
        }
        Color::Named(n) => {
            let n = match n {
                NamedColor::Foreground
                | NamedColor::Background
                | NamedColor::Cursor
                | NamedColor::BrightForeground
                | NamedColor::DimForeground => return,
                // Dim variants map back to their base color; DIM is a flag.
                n if (n as usize) > 15 => n.to_bright() as usize,
                n => n as usize,
            };
            let code = if n < 8 { base as usize + n } else { base as usize + 60 + n - 8 };
            let _ = write!(out, ";{code}");
        }
    }
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

    /// A screen of 60 columns that ends with these rows.
    fn screen_with(rows: &[&str]) -> Screen {
        let mut s = Screen::new(60, 16);
        s.feed(b"an earlier line of the conversation\r\n\r\n");
        s.feed(rows.join("\r\n").as_bytes());
        s
    }

    #[test]
    fn tail_text_cuts_the_input_box() {
        let rule = "─".repeat(60);
        let claude = screen_with(&["● Done. Tests pass.", "", &rule, "❯ ", &rule, "  ? for shortcuts"]);
        assert_eq!(claude.tail_text(Some("claude"), 3), ["an earlier line of the conversation", "", "● Done. Tests pass."]);
        let codex = screen_with(&["• Ran cargo test", "", "› Ask Codex to do anything", "", "  ? for shortcuts"]);
        assert_eq!(codex.tail_text(Some("codex"), 1), ["• Ran cargo test"]);
        // An approval prompt keeps the bottom rows.
        let ask = screen_with(&[&rule, " Do you want to proceed?", " ❯ 1. Yes", "", " Esc to cancel · Tab to amend"]);
        assert_eq!(ask.tail_text(Some("claude"), 1), [" Esc to cancel · Tab to amend"]);
        // A shell keeps everything.
        assert_eq!(claude.tail_text(None, 1), ["  ? for shortcuts"]);
    }

    // The rows below come from real screens: Claude Code 2.1.287, Codex
    // 0.159.3, OpenCode 1.18.34, Antigravity CLI 1.2.14 and Grok 1.0.46.
    #[test]
    fn recognizes_each_agents_approval_prompt() {
        let claude = [" Do you want to proceed?", " ❯ 1. Yes", "   3. No", "", " Esc to cancel · Tab to amend"];
        assert!(screen_with(&claude).approval_prompt("claude"));
        let trust = [" ❯ No, exit", "   Yes, I trust this folder", "", " Enter to confirm · Esc to cancel"];
        assert!(screen_with(&trust).approval_prompt("claude"));

        let codex = ["  Would you like to run the following command?", "› 1. Yes, proceed (y)", "  Press enter to confirm or esc to cancel"];
        assert!(screen_with(&codex).approval_prompt("codex"));

        let opencode = ["  ┃  △ Permission required", "  ┃  $ touch probe.txt", "  ┃", "  ┃   Allow once   Allow always   Reject      ⇆ select", "  ┃"];
        assert!(screen_with(&opencode).approval_prompt("opencode"));

        let gemini = ["Run this command?", "> 1. Yes, run command", "  4. No, cancel", "  ↑/↓ Navigate · tab Amend", "esc to cancel          Gemini 3.8 Flash"];
        assert!(screen_with(&gemini).approval_prompt("gemini"));

        let grok = ["  ┃  2 (○) Yes, proceed", "  ┃  3 (○) No, reject (type to add feedback)", "  ┃", "  1/4:select  │  Tab:next option  │  Ctrl+c:cancel"];
        assert!(screen_with(&grok).approval_prompt("grok"));

        // A prompt of one agent is not a prompt of another, or of a shell.
        assert!(!screen_with(&claude).approval_prompt("codex"));
        assert!(!screen_with(&claude).approval_prompt("bash"));
    }

    #[test]
    fn codex_update_prompt_waits_for_input() {
        let rows = [
            "  Update available · 0.164.0 → 0.165.0",
            "  Release notes: https://github.com/openai/codex/releases/latest",
            "› 1. Update now (runs `npm install -g @openai/codex`)",
            "  2. Skip",
            "  3. Skip until next version",
            "  enter continue · esc skip",
        ];
        assert!(screen_with(&rows).approval_prompt("codex"));
        let mut quoted = rows.to_vec();
        quoted.extend(["› Ask Codex to do anything", "  ? for shortcuts"]);
        assert!(!screen_with(&quoted).approval_prompt("codex"));
    }

    #[test]
    fn a_wrapped_prompt_still_matches() {
        let mut s = Screen::new(24, 8);
        s.feed(b"Would you like to run the following command?\r\n\r\nPress enter to confirm or esc to cancel");
        assert!(s.approval_prompt("codex"));
    }

    #[test]
    fn idle_and_working_screens_are_no_prompt() {
        let claude_idle = ["────────────", "❯ ", "────────────", "", "  ⏸ manual mode on · ← for agents"];
        assert!(!screen_with(&claude_idle).approval_prompt("claude"));
        let claude_work = ["✻ Scurrying… (3s · ↓ 147 tokens)", "────────────", "❯ ", "────────────", "  ⏸ manual mode on"];
        assert!(!screen_with(&claude_work).approval_prompt("claude"));
        let codex_work = ["• Working (2s • esc to interrupt)", "", "› Ask Codex to do anything", "", "  ← for agents · ? for shortcuts"];
        assert!(!screen_with(&codex_work).approval_prompt("codex"));
        assert!(screen_with(&codex_work).busy_line("codex"));
        assert!(!screen_with(&["› Ask Codex to do anything", "", "  ← for agents · ? for shortcuts"]).busy_line("codex"));
        let opencode_work = ["  ┃  Build · Big Pickle OpenCode Zen", "  ╹▀▀▀▀▀▀▀▀▀", "   ⬝⬝⬝⬝  esc interrupt       tab agents  ctrl+p commands"];
        assert!(!screen_with(&opencode_work).approval_prompt("opencode"));
        assert!(screen_with(&opencode_work).busy_line("opencode"));
        let gemini_work = ["⣟  Generating...", "────────────", ">", "────────────", "esc to cancel          Gemini 3.8 Flash"];
        assert!(!screen_with(&gemini_work).approval_prompt("gemini"));
        assert!(screen_with(&gemini_work).busy_line("gemini"));
        // Antigravity's slash-command menu and its file prompt, as captured.
        let agy_menu = [
            "> /add-dir             Add a directory to the workspace",
            "  /agents              List available custom agents",
            "  ↑/↓ Navigate · enter Select · tab Complete",
            "esc to cancel          Gemini 3.8 Flash · medium",
        ];
        assert!(!screen_with(&agy_menu).approval_prompt("gemini"));
        assert!(!screen_with(&agy_menu).busy_line("gemini"));
        let agy_file = [
            "Allow creation of this file?",
            "> 1. Yes, allow creation",
            "  2. No, deny creation",
            "  ↑/↓ Navigate · tab Amend · f full diff",
            "esc to cancel          Gemini 3.8 Flash · medium",
        ];
        assert!(screen_with(&agy_file).approval_prompt("gemini"));
        assert!(!screen_with(&agy_file).busy_line("gemini"));
        let gemini_cli = ["Allow execution of: 'ls'?", "● 1. Allow once", "  2. Allow for this session", "  3. No, suggest changes (esc)"];
        assert!(screen_with(&gemini_cli).approval_prompt("gemini"));
        let grok_cmd = ["$ rm -rf build", "  Allow once", "  Always allow this command", "  Reject"];
        assert!(screen_with(&grok_cmd).approval_prompt("grok"));
        assert!(screen_with(&["  esc again to interrupt"]).busy_line("opencode"));
        assert!(screen_with(&["• Working (3s • ctrl+c to interrupt)"]).busy_line("codex"));
        let grok_idle = ["  │ ❯                              │", "  ╰──── Grok 4.7 (high) · always-approve ─╯", "  Shift+Tab:mode  │  Ctrl+.:shortcuts"];
        assert!(!screen_with(&grok_idle).approval_prompt("grok"));
    }

    #[test]
    fn prompt_words_in_the_conversation_do_not_count() {
        // The agent quotes a prompt, then draws its input box under it.
        let quoted = [" Do you want to proceed?", " Esc to cancel · Tab to amend", "────────────", "❯ ", "────────────", "  ⏸ manual mode on"];
        assert!(!screen_with(&quoted).approval_prompt("claude"));
    }

    #[test]
    fn snapshot_redraws_screen() {
        let mut s = Screen::new(20, 4);
        s.feed(b"one\r\n\x1b[31mred\x1b[0m\r\nthree\x1b[?2004h");
        let snap = String::from_utf8(s.snapshot()).unwrap();
        assert!(snap.starts_with("\x1bc"));
        assert!(snap.contains("one\r\n"));
        assert!(snap.contains("\x1b[0;31mred"));
        assert!(snap.contains("three"));
        assert!(snap.contains("\x1b[3;6H"), "cursor after 'three': {snap:?}");
        assert!(snap.contains("\x1b[?2004h"));
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
    fn title_states() {
        use super::{title_state, TitleState::*};
        assert_eq!(title_state("claude", "◐ Fix the bug"), Some(Busy));
        assert_eq!(title_state("claude", "◑ Fix the bug"), Some(Busy));
        assert_eq!(title_state("claude", "✳ Fix the bug"), Some(Ready));
        assert_eq!(title_state("claude", "Fix the bug"), None);
        assert_eq!(title_state("codex", "⠋ Fix the bug | skiff"), Some(Busy));
        assert_eq!(title_state("codex", "Fix the bug | skiff"), Some(Ready));
        assert_eq!(title_state("codex", "[ ! ] Action Required | skiff"), Some(Action));
        assert_eq!(title_state("codex", "[ . ] Action Required | skiff"), Some(Action));
        assert_eq!(title_state("gemini", "✦  Working… (skiff)"), Some(Busy));
        assert_eq!(title_state("gemini", "⏲  Working… (skiff)"), Some(Busy));
        assert_eq!(title_state("gemini", "✋  Action Required (skiff)"), Some(Action));
        assert_eq!(title_state("gemini", "◇  Ready (skiff)"), Some(Ready));
        // Grok 1.0.70, as captured.
        assert_eq!(
            title_state("grok", "⠋ - Waiting for response… - grok"),
            Some(Busy)
        );
        assert_eq!(
            title_state(
                "grok",
                "⚠ Action Required - ⠋ - Running: Write `x` - Greeting - grok"
            ),
            Some(Action)
        );
        assert_eq!(title_state("grok", "Greeting - grok"), Some(Ready));
        assert_eq!(title_state("grok", "grok"), Some(Ready));
        assert_eq!(title_state("opencode", "OC | Fix the bug"), None);
    }
}
