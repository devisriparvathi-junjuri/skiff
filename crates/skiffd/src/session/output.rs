use super::*;

/// Unix time in microseconds, for the latency trace.
pub(super) fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64)
}


pub(super) struct ScreenState {
    pub(super) screen: Screen,
    pub(super) pending: Vec<u8>,
    /// When the first byte of `pending` came. Stale while it is empty.
    pub(super) held_since: Instant,
    /// When the last byte of `pending` came. Stale while it is empty.
    pub(super) last_push: Instant,
    pub(super) last_scan: Instant,
    /// Output came since the last scan.
    pub(super) unscanned: bool,
    /// A chunk since the last scan printed text.
    pub(super) printed: bool,
    /// A chunk since the last scan printed text that was not an echo.
    pub(super) printed_work: bool,
    /// `pending` holds the first output after input: when it was written and read.
    pub(super) echo: Option<EchoTimes>,
}

/// What a scan of the screen found.
#[derive(Default)]
pub(super) struct Scan {
    /// The scan ran. The fields below say nothing when it did not.
    pub(super) scanned: bool,
    /// New text on screen that is not an echo.
    pub(super) work: bool,
    /// The agent in front shows its approval prompt.
    pub(super) approval: bool,
    /// The agent in front shows its busy line.
    pub(super) busy: bool,
}

impl ScreenState {
    pub(super) fn flush(&mut self, output: &broadcast::Sender<Chunk>) {
        if !self.pending.is_empty() {
            let _ = output.send(Arc::new(Piece {
                data: std::mem::take(&mut self.pending),
                read: self.held_since,
                echo: self.echo.take(),
            }));
        }
    }

    /// When the held output goes out: after a quiet moment, or `HOLD` from
    /// its first byte. `None` with nothing held.
    pub(super) fn flush_due(&self) -> Option<Instant> {
        (!self.pending.is_empty()).then(|| (self.last_push + QUIET).min(self.held_since + HOLD))
    }

    /// When output that came after the last scan is scanned. `None` with
    /// nothing to scan.
    pub(super) fn scan_due(&self) -> Option<Instant> {
        self.unscanned.then(|| self.last_scan + SCAN)
    }

    pub(super) fn scan(&mut self, front: Front) -> Scan {
        let changed = self.printed && self.screen.text_changed();
        let (approval, busy) = match front {
            Front::Agent(agent) => (self.screen.approval_prompt(agent), self.screen.busy_line(agent)),
            _ => (false, false),
        };
        let work = changed && self.printed_work;
        self.last_scan = Instant::now();
        self.unscanned = false;
        self.printed = false;
        self.printed_work = false;
        Scan { scanned: true, work, approval, busy }
    }
}


impl Session {
    /// Feeds the emulator and queues the bytes. Scans the screen when the last
    /// scan is older than `SCAN`; the reader scans what is left at its
    /// deadline. The last value is true when the chunk answers a key.
    pub(super) fn push_output(&self, chunk: &[u8]) -> (Signals, Scan, bool) {
        let echoing = self.echoing();
        let front = self.front();
        let mut st = self.screen.lock().unwrap();
        // A synchronized update past its timeout ends before more bytes
        // come in, as in alacritty's event loop: steady output would keep
        // the reader from its deadline.
        let ended = if st
            .screen
            .sync_due()
            .is_some_and(|due| due <= Instant::now())
        {
            st.screen.end_sync()
        } else {
            Signals::default()
        };
        let signals = ended.then(st.screen.feed(chunk));
        // The probe is answered before these bytes reach a client. A
        // client answers the DA1 query a program sends right after it, and
        // the program takes OSC 7501 as supported only when this reply comes
        // first. The writer thread keeps the order of its queue.
        for _ in 0..signals.probes {
            self.reply(program_status::PROBE_REPLY);
        }
        st.unscanned = true;
        if prints(chunk) {
            st.printed = true;
            st.printed_work |= !echoing;
        }
        let scan = if st.last_scan.elapsed() >= SCAN {
            st.scan(front)
        } else {
            Scan::default()
        };
        let now = Instant::now();
        if st.pending.is_empty() {
            st.held_since = now;
        }
        st.last_push = now;
        st.pending.extend_from_slice(chunk);
        // The first read after a write to the PTY: the program's answer.
        let wrote = self.wrote.swap(0, Ordering::Relaxed);
        if wrote != 0 {
            st.echo = Some(EchoTimes { wrote, read: now_us() });
        }
        // The first answer to a keystroke goes out now, even while the
        // program streams output. The reads after that answer merge again,
        // so a redraw does not go out as a stream of small chunks. The rest
        // waits for the reader's deadline, up to `HOLD` from its first byte;
        // a program that streams without a pause never lets the reader wait,
        // so the hold is checked here as well.
        let answers_key = self.key_pending.swap(false, Ordering::Relaxed);
        if answers_key || wrote != 0 || st.pending.len() >= MAX_CHUNK || now >= st.held_since + HOLD {
            st.flush(&self.output);
        }
        (signals, scan, echoing)
    }

    /// Sends the output still held when the reader ends. It waits for the
    /// screen lock: no reader deadline comes after this, so a skipped flush
    /// would lose the child's last bytes.
    pub(super) fn flush_output(&self) {
        let mut st = self.screen.lock().unwrap();
        st.flush(&self.output);
    }

    /// When the reader must wake with no output: to send the held output,
    /// or to scan output that came after the last scan, so the end of a
    /// burst counts. `None` while nothing is due, so an idle session never
    /// wakes.
    pub(super) fn next_due(&self) -> Option<Instant> {
        let st = self.screen.lock().unwrap();
        [st.flush_due(), st.scan_due(), st.screen.sync_due()]
            .into_iter()
            .flatten()
            .min()
    }

    /// Does what is due at the reader's deadline: ends a synchronized update
    /// that timed out, sends the held output, and scans the screen. Work not
    /// due yet waits for the next deadline.
    pub(super) fn on_due(&self) -> (Signals, Scan) {
        let front = self.front();
        let now = Instant::now();
        let mut st = self.screen.lock().unwrap();
        let mut signals = Signals::default();
        // A program that dies mid-frame sends no `?2026l`. Without this the
        // screen, its title and its reports would wait for megabytes more.
        if st.screen.sync_due().is_some_and(|due| due <= now) {
            signals = st.screen.end_sync();
            for _ in 0..signals.probes {
                self.reply(program_status::PROBE_REPLY);
            }
            // The held frame drew now: the next scan reads it. It is no work:
            // the program went quiet.
            st.unscanned = true;
            st.printed = true;
        }
        if st.flush_due().is_some_and(|due| due <= now) {
            st.flush(&self.output);
        }
        let scan = if st.scan_due().is_some_and(|due| due <= now) {
            st.scan(front)
        } else {
            Scan::default()
        };
        (signals, scan)
    }
}

/// True when the bytes put text on the screen. Escape sequences and control
/// characters alone do not. A sequence split across reads may misjudge one chunk.
pub(super) fn prints(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b => {
                i += 1;
                match bytes.get(i) {
                    // CSI: parameters, then one final byte in 0x40..=0x7e.
                    Some(b'[') => {
                        i += 1;
                        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                            i += 1;
                        }
                    }
                    // OSC, DCS, APC, PM, SOS: up to BEL or ST.
                    Some(b']' | b'P' | b'_' | b'^' | b'X') => {
                        while i < bytes.len() && bytes[i] != 0x07 && !(bytes[i] == b'\\' && bytes[i - 1] == 0x1b) {
                            i += 1;
                        }
                    }
                    // Charset designation: one more byte.
                    Some(b'(' | b')' | b'*' | b'+' | b'#' | b'%') => i += 1,
                    _ => {}
                }
            }
            b if b < 0x20 || b == 0x7f => {}
            _ => return true,
        }
        i += 1;
    }
    false
}



