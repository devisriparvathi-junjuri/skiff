use super::*;

impl Session {
    /// Queues input for the writer thread, in order.
    pub(super) fn send(&self, id: &str, items: Vec<Input>) -> Result<()> {
        // Stored before the send: the reader can see the echo before this
        // thread runs again, and it must count as an echo, not as work.
        self.last_input.store(now_ms(), Ordering::Relaxed);
        self.key_pending.store(true, Ordering::Relaxed);
        for item in items {
            self.queued.fetch_add(1, Ordering::AcqRel);
            if self.input.send(item).is_err() {
                self.queued.fetch_sub(1, Ordering::AcqRel);
                bail!("session {id} no longer takes input");
            }
        }
        Ok(())
    }

    /// Writes an answer to a query from the program: the daemon's own, or a
    /// client's (see [`Self::take_replies`]). It is not the user's input: it
    /// opens no echo window, answers no bell and counts no input.
    pub(super) fn reply(&self, data: &[u8]) {
        self.queued.fetch_add(1, Ordering::AcqRel);
        if self.input.send(Input::Reply(data.to_vec())).is_err() {
            self.queued.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Takes `data` from a client as its answer to the program's queries:
    /// a query is open and `data` holds only replies. An answer is no input
    /// from the user: it opens no echo window, answers no bell and counts no
    /// input, so a `skiff send` that checks for typing is not refused by it.
    pub(super) fn take_replies(&self, data: &[u8]) -> bool {
        let open = self.queries.load(Ordering::Relaxed);
        if open == 0
            || now_ms().saturating_sub(self.queried_at.load(Ordering::Relaxed)) > REPLY_WINDOW_MS
        {
            return false;
        }
        let Some(n) = replies(data) else { return false };
        self.queries
            .store(open.saturating_sub(n), Ordering::Relaxed);
        true
    }

    /// True shortly after input: output now is the program answering a key.
    pub(super) fn echoing(&self) -> bool {
        now_ms().saturating_sub(self.last_input.load(Ordering::Relaxed)) < ECHO_MS
    }

}

impl SessionPool {
    pub fn write(&self, id: &str, data: &[u8]) -> Result<()> {
        let _change = self.change()?;
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        if session.take_replies(data) {
            session.reply(data);
            return Ok(());
        }
        {
            // The info lock orders this write against a submit's check.
            let mut info = session.info.lock().unwrap();
            info.input_gen += 1;
            session.send(id, vec![Input::Bytes(data.to_vec())])?;
        }
        // Input answers a bell. An agent's prompt goes when the screen says so.
        if session.belled.swap(false, Ordering::Relaxed)
            && session.info.lock().unwrap().state == SessionState::Waiting
        {
            self.set_state(&session, SessionState::Working);
        }
        Ok(())
    }

    /// Types `data` and, with `enter`, presses Enter [`ENTER_DELAY`] later,
    /// only if the session took no input since the client read
    /// `input_gen == expect`. Input that comes during the delay waits behind
    /// the Enter. `Ok(false)`: refused, and nothing typed.
    pub fn submit(&self, id: &str, data: &[u8], paste: bool, enter: bool, expect: u64) -> Result<bool> {
        let _change = self.change()?;
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        let data = if paste && session.screen.lock().unwrap().screen.bracketed_paste() {
            bracketed(data)
        } else {
            data.to_vec()
        };
        let mut info = session.info.lock().unwrap();
        if info.input_gen != expect || info.state == SessionState::Done {
            return Ok(false);
        }
        info.input_gen += 1;
        let mut items = vec![Input::Bytes(data)];
        if enter {
            items.extend([Input::Pause(ENTER_DELAY), Input::Bytes(b"\r".to_vec())]);
        }
        session.send(id, items)?;
        Ok(true)
    }

    /// Writes `data` as a paste: inside bracketed paste when the program
    /// asked for it, else as typed text.
    pub fn paste(&self, id: &str, data: &[u8]) -> Result<()> {
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        if !session.screen.lock().unwrap().screen.bracketed_paste() {
            return self.write(id, data);
        }
        self.write(id, &bracketed(data))
    }

}

/// One item for the writer thread. A pause holds the input behind it, so a
/// submit's Enter comes after its text with nothing typed between.
pub(super) enum Input {
    Bytes(Vec<u8>),
    Pause(Duration),
    /// An answer to a query, not typed: no latency trace.
    Reply(Vec<u8>),
}

/// The thread that writes input to the PTY, its count of writes queued, and
/// when it first wrote since the last output. A program that stops reading its input blocks this
/// thread alone. Input after the PTY is gone is dropped and still counted
/// down, so a reload never waits on a dead session.
pub(super) fn start_writer(id: &str, pty: &Pty) -> Result<(std::sync::mpsc::Sender<Input>, Arc<AtomicUsize>, Arc<AtomicU64>)> {
    let mut writer = pty.writer().context("clone writer")?;
    let queued = Arc::new(AtomicUsize::new(0));
    let wrote = Arc::new(AtomicU64::new(0));
    let wrote_at = wrote.clone();
    let (input, input_rx) = std::sync::mpsc::channel::<Input>();
    let count = queued.clone();
    std::thread::Builder::new()
        .name(format!("pty-in-{id}"))
        .spawn(move || {
            let mut open = true;
            // Ends when the session is dropped.
            for item in input_rx {
                match item {
                    Input::Bytes(data) => {
                        // Before the write: a terminal in canonical mode echoes
                        // inside it, so the reader can see the answer first.
                        // The first write since the last read: keys typed while
                        // the program is silent wait on the program, and the
                        // trace must not count that wait as skiffd's.
                        let _ = wrote_at.compare_exchange(0, now_us(), Ordering::Relaxed, Ordering::Relaxed);
                        if open && writer.write_all(&data).and_then(|_| writer.flush()).is_err() {
                            open = false;
                        }
                    }
                    Input::Pause(d) => std::thread::sleep(d),
                    Input::Reply(data) => {
                        if open
                            && writer
                                .write_all(&data)
                                .and_then(|_| writer.flush())
                                .is_err()
                        {
                            open = false;
                        }
                    }
                }
                count.fetch_sub(1, Ordering::AcqRel);
            }
        })
        .context("spawn writer thread")?;
    Ok((input, queued, wrote))
}


/// How many replies to terminal queries `data` holds, when it holds nothing
/// else: DA1 and DA2 (`CSI ? … c`, `CSI > … c`), status and cursor reports
/// (`CSI … n`, `CSI row ; col R`), mode reports (`CSI … $ y`), kitty
/// keyboard flags (`CSI ? … u`), window reports (`CSI … t`), and OSC or DCS
/// strings (colors, DECRQSS). No key sends any of these but the cursor
/// report, which Ctrl+F3 also sends; so a reply counts only while a query is
/// open, see [`Session::take_replies`].
fn replies(data: &[u8]) -> Option<u32> {
    let mut i = 0;
    let mut n = 0u32;
    while i < data.len() {
        if data[i] != 0x1b {
            return None;
        }
        match *data.get(i + 1)? {
            b'[' => {
                let start = i + 2;
                let mut j = start;
                while j < data.len() && (0x30..=0x3f).contains(&data[j]) {
                    j += 1;
                }
                let params = &data[start..j];
                let inter = j;
                while j < data.len() && (0x20..=0x2f).contains(&data[j]) {
                    j += 1;
                }
                let inter = &data[inter..j];
                let digits = params.iter().all(|b| b.is_ascii_digit() || *b == b';');
                let ok = match (*data.get(j)?, inter) {
                    (b'c', []) => matches!(params.first(), Some(b'?' | b'>')),
                    (b'u', []) => params.first() == Some(&b'?'),
                    (b'n' | b't', []) => digits && !params.is_empty(),
                    (b'R', []) => digits && params.contains(&b';'),
                    (b'y', b"$") => !params.is_empty(),
                    _ => false,
                };
                if !ok {
                    return None;
                }
                i = j + 1;
            }
            b']' | b'P' => {
                let osc = data[i + 1] == b']';
                let mut j = i + 2;
                loop {
                    match *data.get(j)? {
                        0x07 if osc => break j += 1,
                        0x1b if data.get(j + 1) == Some(&b'\\') => break j += 2,
                        _ => j += 1,
                    }
                }
                i = j;
            }
            _ => return None,
        }
        n += 1;
    }
    (n > 0).then_some(n)
}

/// `data` between bracketed paste marks. An end mark inside the data would
/// end the paste early and let the rest run as keys, so it is taken out.
fn bracketed(data: &[u8]) -> Vec<u8> {
    const END: &[u8] = b"\x1b[201~";
    let mut out = b"\x1b[200~".to_vec();
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(END) {
            i += END.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out.extend_from_slice(END);
    out
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_told_from_keys() {
        assert_eq!(replies(b"\x1b[?1;2c"), Some(1));
        assert_eq!(replies(b"\x1b[>0;276;0c\x1b[0n\x1b[12;40R"), Some(3));
        assert_eq!(
            replies(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\\x1b]10;rgb:d4d4/d4d4/d4d4\x07"),
            Some(2)
        );
        assert_eq!(
            replies(b"\x1bP1$r0m\x1b\\\x1b[?2004;1$y\x1b[?5u\x1b[8;24;80t"),
            Some(4)
        );
        // XTVERSION's answer, as a terminal gives it.
        assert_eq!(replies(b"\x1bP>|xterm.js(5.5.0)\x1b\\"), Some(1));
        // Keys, a paste, Alt+[, and a reply with a key after it are not replies.
        for keys in [
            &b"y"[..],
            b"\r",
            b"\x1b",
            b"\x1b[A",
            b"\x1b[1;5A",
            b"\x1b[13;2u",
            b"\x1b[15~",
            b"\x1b[",
            b"\x1b[200~\x1b[?1;2c\x1b[201~",
            b"\x1b[?1;2cy",
            b"\x1b]11;rgb:0/0/0",
            b"",
        ] {
            assert_eq!(replies(keys), None, "{keys:?}");
        }
    }

    /// A client's answer to the program's query is not the user's input:
    /// it neither answers a bell nor counts as typing. Without an open
    /// query the same bytes count as keys.
    #[test]
    fn a_reply_to_a_query_is_no_input() {
        use skiff_core::session::{SessionSpec, SessionState};
        let pool = super::SessionPool::new();
        let info = pool
            .create(SessionSpec {
                command: Some("/bin/sleep".into()),
                args: vec!["30".into()],
                ..Default::default()
            })
            .unwrap();
        let session = pool.get(&info.id).unwrap();
        // A program rings, then asks for DA1 and the cursor.
        pool.on_output(&session, &info.id, b"\x07");
        assert_eq!(session.info().state, SessionState::Waiting);
        pool.on_output(&session, &info.id, b"\x1b[c\x1b[6n");
        pool.write(&info.id, b"\x1b[?1;2c").unwrap();
        pool.write(&info.id, b"\x1b[3;1R").unwrap();
        let now = session.info();
        assert_eq!(
            (now.state, now.input_gen),
            (SessionState::Waiting, 0),
            "a reply counted as input"
        );
        assert!(!session.echoing(), "a reply opened the echo window");
        // Both are answered: the same bytes now are keys.
        pool.write(&info.id, b"\x1b[3;1R").unwrap();
        let now = session.info();
        assert_eq!((now.state, now.input_gen), (SessionState::Working, 1));
        pool.kill(&info.id).unwrap();
    }

}
