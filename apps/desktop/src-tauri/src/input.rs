//! The input bridge: keystrokes from the page to the selected Host, in order.
//!
//! Input goes straight onto the live Host connection (`session_terminal@1`). It waits here instead while
//! a reload runs (the gate is closed) or while no connection is up yet. Input
//! that waits leaves in order, and new input queues behind it.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Mutex,
    time::{Duration, Instant},
};
use skiff_client::host::{HostConnection, local::legacy_input};

/// Input waits at most this long for a reload. Older input is dropped.
pub(crate) const HOLD_FOR: Duration = Duration::from_secs(15);
/// Bytes one session may hold. More, and its held input is dropped.
const HOLD_MAX: usize = 256 * 1024;

#[derive(Default)]
pub(crate) struct Input(Mutex<State>);

#[derive(Default)]
struct State {
    /// Set while a reload runs, to when it began.
    gate: Option<Instant>,
    /// Per session, the input that waits.
    held: HashMap<String, Held>,
    /// Sessions whose held input was dropped while the gate was closed. Their
    /// later input is dropped too, so nothing arrives with a gap before it.
    lost: BTreeSet<String>,
    /// A task connects to send what `held` has. Not set with the gate.
    connecting: bool,
}

#[derive(Default)]
struct Held {
    chunks: Vec<Vec<u8>>,
    bytes: usize,
}

/// What [`Input::write`] did with the input.
pub(crate) enum Sent {
    Done,
    Held,
    /// Held, and no task connects yet: the caller starts one.
    Connect,
}

impl Input {
    /// Sends `data` on `live`, or holds it. Under one lock with the gate, so
    /// a write is on the connection before the reload request, or it is held.
    pub(crate) fn write(&self, live: Option<&HostConnection>, session: &str, data: Vec<u8>) -> anyhow::Result<Sent> {
        let mut st = self.0.lock().unwrap();
        if st.gate.is_none() && !st.connecting {
            if let Some(c) = live {
                legacy_input(c, session, data)?;
                return Ok(Sent::Done);
            }
        }
        if st.lost.contains(session) {
            return Ok(Sent::Held);
        }
        if st.gate.is_some_and(|t| t.elapsed() > HOLD_FOR) {
            let ids: Vec<String> = st.held.drain().map(|(id, _)| id).collect();
            st.lost.extend(ids);
            st.lost.insert(session.to_string());
            return Ok(Sent::Held);
        }
        let held = st.held.entry(session.to_string()).or_default();
        held.bytes += data.len();
        held.chunks.push(data);
        if held.bytes > HOLD_MAX {
            st.held.remove(session);
            if st.gate.is_some() {
                st.lost.insert(session.to_string());
            }
            return Ok(Sent::Held);
        }
        if st.gate.is_none() && !st.connecting {
            st.connecting = true;
            return Ok(Sent::Connect);
        }
        Ok(Sent::Held)
    }

    /// The connect task got a connection, or none. Sends the held input,
    /// unless a reload closed the gate meanwhile: the reload sends it.
    pub(crate) fn connected(&self, c: Option<&HostConnection>) {
        let mut st = self.0.lock().unwrap();
        st.connecting = false;
        if st.gate.is_some() {
            return;
        }
        for (id, held) in st.held.drain() {
            let Some(c) = c else { continue };
            for chunk in held.chunks {
                if legacy_input(c, &id, chunk).is_err() {
                    break;
                }
            }
        }
    }

    /// Drops every held input and opens the gate: another Host is selected,
    /// so nothing typed for the old one may reach it.
    pub(crate) fn discard(&self) {
        *self.0.lock().unwrap() = State::default();
    }

    /// Holds all input from now on.
    pub(crate) fn close_gate(&self) {
        let mut st = self.0.lock().unwrap();
        st.gate = Some(Instant::now());
        st.lost.clear();
    }

    /// Sends the held input to the sessions `keep` accepts, on `c`, and lets
    /// input through again. Returns the sessions whose held input was dropped.
    pub(crate) fn open_gate(&self, c: Option<&HostConnection>, keep: impl Fn(&str) -> bool) -> Vec<String> {
        let mut st = self.0.lock().unwrap();
        let expired = st.gate.is_some_and(|t| t.elapsed() > HOLD_FOR);
        let mut lost: BTreeSet<String> = std::mem::take(&mut st.lost);
        for (id, held) in st.held.drain() {
            match c {
                Some(c) if !expired && keep(&id) => {
                    for chunk in held.chunks {
                        if legacy_input(c, &id, chunk).is_err() {
                            lost.insert(id.clone());
                            break;
                        }
                    }
                }
                _ => {
                    lost.insert(id);
                }
            }
        }
        st.gate = None;
        lost.into_iter().collect()
    }
}
