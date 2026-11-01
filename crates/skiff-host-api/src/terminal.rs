//! M1e session terminal: attach (subscribe, snapshot first), raw output,
//! ordered input and resize, gated by `session_terminal@1`. These are the
//! existing session-plane semantics; no new terminal implementation.
use serde::{Deserialize, Serialize};

/// When the session plane wrote input to the PTY and read the first output
/// after it, Unix microseconds. Diagnostic metadata only, never terminal data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EchoTimes {
    pub wrote: u64,
    pub read: u64,
}

/// One frame on a session's terminal stream, in the Host's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalFrame {
    /// The whole screen at attach. Output before it predates the screen.
    Snapshot { data: Vec<u8>, cols: u16, rows: u16 },
    /// Raw PTY bytes after the last snapshot.
    Output {
        data: Vec<u8>,
        echo: Option<EchoTimes>,
    },
}
