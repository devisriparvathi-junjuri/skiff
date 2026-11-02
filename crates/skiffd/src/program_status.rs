//! OSC 7501, the Program Status Protocol (rev 0.4): a program reports its
//! own state to the terminal, `ESC ] 7501 ; state=working:app=codex ST`.
//! The emulator hands each body here (see `screen::Signals`); this module
//! reads it and keeps the records. It needs no PTY, so tests drive it alone.
//!
//! Where this reader differs from the spec, on purpose:
//! - `msg` and `title` with a control character are cleaned, not discarded:
//!   a report that a program is blocked must not be lost to its message.
//! - A `msg` or `title` that is not base64 is dropped; the report stays.
//! - A decoded `msg` or `title` over its limit is cut, not discarded.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use skiff_core::session::{ProgramChild, ProgramStatus, SessionState};

/// The reply to the probe `OSC 7501 ; ?`. With no lists it means every state
/// and kind of rev 0.4, which is what skiffd reads. ST, whatever ended the
/// probe: the spec's form.
pub const PROBE_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

/// A whole sequence over this is discarded. The body is the sequence less
/// `ESC ] 7501 ;` and the two bytes of ST.
const MAX_REPORT: usize = 4096;
const MAX_BODY: usize = MAX_REPORT - 8;
const MAX_KEY: usize = 16;
/// Limits on the decoded text; the encoded forms are cut at 4/3 of these.
const MAX_MSG: usize = 2048;
const MAX_TITLE: usize = 192;
const MAX_APP: usize = 32;
const MAX_ID: usize = 128;
const MAX_ID_SEGMENT: usize = 32;
const MAX_ID_DEPTH: usize = 8;
/// Records kept per session. A new one past this drops the least recently
/// updated. The spec asks terminals for at least 64.
pub const MAX_RECORDS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Idle,
    Working,
    Blocked,
    Done,
    Error,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Blocked => "blocked",
            State::Done => "done",
            State::Error => "error",
        }
    }
}

/// What a blocked program waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
            Kind::Auth => "auth",
        }
    }
}

/// One record: what the last report for its id said. A report replaces its
/// record whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub state: State,
    pub kind: Option<Kind>,
    pub msg: Option<String>,
    pub title: Option<String>,
    pub progress: Option<u8>,
    pub app: Option<String>,
}

impl Record {
    pub fn view(&self) -> ProgramStatus {
        ProgramStatus {
            state: self.state.name().into(),
            kind: self.kind.map(|k| k.name().into()),
            msg: self.msg.clone(),
            title: self.title.clone(),
            progress: self.progress,
            app: self.app.clone(),
            stalled: false,
            ended: false,
            children: Vec::new(),
        }
    }
}

/// One OSC 7501 body, read.
#[derive(Debug, PartialEq, Eq)]
pub enum Report {
    /// `?`: the program asks whether the terminal reads reports.
    Probe,
    /// `id` is `None` for the root record.
    Set { id: Option<String>, record: Record },
    /// `state=clear`: the record and those under it, or every record with no id.
    Clear { id: Option<String> },
}

/// Reads one body. `None` when the spec says to ignore the report: no state
/// or an unknown one, a bad id, or a report over its size.
pub fn parse(body: &[u8]) -> Option<Report> {
    if is_probe(body) {
        return Some(Report::Probe);
    }
    if body.len() > MAX_BODY {
        return None;
    }
    let (mut state, mut kind, mut msg, mut title, mut progress, mut app, mut id) =
        (None, None, None, None, None, None, None);
    for pair in body.split(|&b| b == b':') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue;
        };
        let (key, value) = (&pair[..eq], &pair[eq + 1..]);
        // A bad id must not turn a child's report into the root's.
        if key == b"id" && !value.iter().all(|&b| value_byte(b)) {
            return None;
        }
        // A pair with a bad key or value is skipped; the rest still counts.
        if key.is_empty()
            || key.len() > MAX_KEY
            || !key.iter().all(u8::is_ascii_lowercase)
            || !value.iter().all(|&b| value_byte(b))
        {
            continue;
        }
        // Checked above: ASCII only.
        let value = std::str::from_utf8(value).unwrap_or_default();
        // A key that repeats: the last value wins.
        match key {
            b"state" => state = Some(value),
            b"kind" => kind = Some(value),
            b"msg" => msg = Some(value),
            b"title" => title = Some(value),
            b"progress" => progress = Some(value),
            b"app" => app = Some(value),
            b"id" => id = Some(value),
            _ => {}
        }
    }
    let id = match id {
        Some(id) => Some(valid_id(id)?.to_string()),
        None => None,
    };
    let state = match state? {
        "clear" => return Some(Report::Clear { id }),
        "idle" => State::Idle,
        "working" => State::Working,
        "blocked" => State::Blocked,
        "done" => State::Done,
        "error" => State::Error,
        _ => return None,
    };
    let kind = kind
        .filter(|_| state == State::Blocked)
        .and_then(|k| match k {
            "permission" => Some(Kind::Permission),
            "question" => Some(Kind::Question),
            "auth" => Some(Kind::Auth),
            _ => None,
        });
    let progress = progress
        .filter(|p| {
            matches!(state, State::Working | State::Blocked)
                && !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
        })
        .and_then(|p| p.parse::<u8>().ok())
        .filter(|p| *p <= 100);
    let record = Record {
        state,
        kind,
        msg: msg.and_then(|m| text(m, MAX_MSG)),
        title: title.and_then(|t| text(t, MAX_TITLE)),
        progress,
        app: app
            .filter(|a| !a.is_empty() && a.len() <= MAX_APP)
            .map(str::to_string),
    };
    Some(Report::Set { id, record })
}

/// The body of the probe, `OSC 7501 ; ? ST`.
pub fn is_probe(body: &[u8]) -> bool {
    body == b"?" || body.starts_with(b"?:")
}

/// The bytes a value may hold: `[A-Za-z0-9_.,+/=-]`.
fn value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b)
}

/// A hierarchical id, `a/b/c`: at most 8 segments of `[A-Za-z0-9_.+-]{1,32}`
/// and 128 bytes in all.
fn valid_id(id: &str) -> Option<&str> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID
        && id.split('/').count() <= MAX_ID_DEPTH
        && id.split('/').all(|s| {
            (1..=MAX_ID_SEGMENT).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
        });
    ok.then_some(id)
}

/// Decodes a base64 text field into plain text for display: control
/// characters go (a line break or tab becomes a space), and so do the
/// characters that reorder or hide text. Cut to `max` bytes. `None` when it
/// is not base64 or nothing is left.
fn text(encoded: &str, max: usize) -> Option<String> {
    // Enough to decode `max` bytes; more is cut before decoding.
    let room = (max / 3 + 1) * 4;
    let encoded = if encoded.len() > room {
        &encoded[..room]
    } else {
        encoded
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD_NO_PAD.decode(encoded.trim_end_matches('='))
        })
        .ok()?;
    let mut out = String::with_capacity(bytes.len().min(max));
    for c in String::from_utf8_lossy(&bytes).chars() {
        let c = match c {
            '\n' | '\r' | '\t' => ' ',
            c if c.is_control() || hidden(c) => continue,
            c => c,
        };
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    let trimmed = out.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Bidi controls and zero-width characters: they can make text read as
/// something other than what it is.
fn hidden(c: char) -> bool {
    matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

/// The session state while the root record is `claim`, from `now`. A live
/// root record decides it: `working` is working, `blocked` is waiting, and
/// `idle`, `done` and `error` are idle. With none, the screen rules and the
/// idle timer decide again from here; a prompt the reports kept up is gone
/// with them, so waiting becomes working until those rules say otherwise.
pub fn session_state(claim: Option<State>, now: SessionState) -> SessionState {
    match claim {
        Some(State::Working) => SessionState::Working,
        Some(State::Blocked) => SessionState::Waiting,
        Some(State::Idle | State::Done | State::Error) => SessionState::Idle,
        None if now == SessionState::Waiting => SessionState::Working,
        None => now,
    }
}

/// Whether the change from `old` to `new` ends a turn, a result to read:
/// `done` or `error` that was not already said. `idle` ends none; the
/// program is ready, or the user turned its prompt down.
pub fn ends_turn(old: Option<State>, new: Option<State>) -> bool {
    matches!(new, Some(State::Done | State::Error)) && old != new
}

/// A session's records, least recently updated first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Records {
    /// By id; `""` is the root record.
    list: Vec<(String, Record)>,
    /// The process group in front of the PTY when they were reported. 0:
    /// not known. Reports from another group are another program's.
    #[serde(default)]
    pub group: u32,
}

impl Records {
    /// Applies a report. A probe changes nothing.
    pub fn apply(&mut self, report: Report) {
        match report {
            Report::Probe => {}
            Report::Set { id, record } => {
                let id = id.unwrap_or_default();
                self.list.retain(|(k, _)| *k != id);
                self.list.push((id, record));
                if self.list.len() > MAX_RECORDS {
                    self.list.remove(0);
                }
            }
            Report::Clear { id: None } => self.list.clear(),
            Report::Clear { id: Some(id) } => {
                let under = format!("{id}/");
                self.list
                    .retain(|(k, _)| *k != id && !k.starts_with(&under));
            }
        }
    }

    /// The root record: the report with no id. It speaks for the program.
    pub fn root(&self) -> Option<&Record> {
        self.get("")
    }

    pub fn get(&self, id: &str) -> Option<&Record> {
        self.list.iter().find(|(k, _)| k == id).map(|(_, r)| r)
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// The root record with the others under it, by id. `None` with no root:
    /// only the root speaks for the program.
    pub fn view(&self) -> Option<ProgramStatus> {
        let mut view = self.root()?.view();
        let mut children: Vec<_> = self
            .list
            .iter()
            .filter(|(id, _)| !id.is_empty())
            .map(|(id, r)| ProgramChild {
                id: id.clone(),
                state: r.state.name().into(),
                kind: r.kind.map(|k| k.name().into()),
                msg: r.msg.clone(),
                title: r.title.clone(),
                progress: r.progress,
            })
            .collect();
        children.sort_by(|a, b| a.id.cmp(&b.id));
        view.children = children;
        Some(view)
    }

    /// What is left to show once the program has left: a root `done` or
    /// `error`, marked ended, without its children. The records are cleared.
    pub fn end(&mut self) -> Option<ProgramStatus> {
        let kept = self
            .root()
            .filter(|r| matches!(r.state, State::Done | State::Error))
            .map(|r| ProgramStatus {
                ended: true,
                ..r.view()
            });
        self.clear();
        kept
    }

    /// Only the result of the root changes from `old`: its msg, title or
    /// progress, or a child's. Such a change may wait a moment to be sent.
    pub fn minor_change(old: &ProgramStatus, new: &ProgramStatus) -> bool {
        let key = |p: &ProgramStatus| {
            (
                p.state.clone(),
                p.kind.clone(),
                p.app.clone(),
                p.ended,
                p.stalled,
                p.children
                    .iter()
                    .map(|c| (c.id.clone(), c.state.clone(), c.kind.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        key(old) == key(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(body: &str) -> (Option<String>, Record) {
        match parse(body.as_bytes()) {
            Some(Report::Set { id, record }) => (id, record),
            other => panic!("{body}: {other:?}"),
        }
    }

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn reads_the_agents_reports() {
        let (id, r) = set("state=working:app=codex");
        assert_eq!(
            (id, r.state, r.app.as_deref()),
            (None, State::Working, Some("codex"))
        );
        let msg = "YXBwcm92ZSBCYXNoOiBta2RpciAvdG1wL29zYzc1MDEtZGVueS10ZXN0";
        let (_, r) = set(&format!(
            "state=blocked:app=claude-code:kind=permission:msg={msg}"
        ));
        assert_eq!(r.state, State::Blocked);
        assert_eq!(r.kind, Some(Kind::Permission));
        assert_eq!(
            r.msg.as_deref(),
            Some("approve Bash: mkdir /tmp/osc7501-deny-test")
        );
        assert_eq!(parse(b"state=clear"), Some(Report::Clear { id: None }));
        assert_eq!(parse(b"?"), Some(Report::Probe));
    }

    #[test]
    fn ignores_what_the_spec_ignores() {
        // No state, an unknown state, a bad id, a report over its size.
        assert_eq!(parse(b"app=codex"), None);
        assert_eq!(parse(b"state=sleeping"), None);
        assert_eq!(parse(b"state=working:id=a//b"), None);
        assert_eq!(parse(b"state=working:id=a b"), None);
        assert_eq!(
            parse(&[b"state=working:x=".as_slice(), &[b'a'; MAX_BODY]].concat()),
            None
        );
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"\xff\xfe"), None);
        // A bad pair is skipped and the rest still counts; the last of a repeated key wins.
        let (_, r) = set("bogus:=x:state=idle:Kind=x:app=a;b:state=working:future=1");
        assert_eq!((r.state, r.app), (State::Working, None));
        // kind only with blocked, progress only with working or blocked, both in range.
        let (_, r) = set("state=done:kind=permission:progress=50");
        assert_eq!((r.kind, r.progress), (None, None));
        assert_eq!(set("state=working:progress=101").1.progress, None);
        assert_eq!(set("state=working:progress=+5").1.progress, None);
        assert_eq!(set("state=working:progress=42").1.progress, Some(42));
        assert_eq!(set("state=blocked:kind=telepathy").1.kind, None);
        assert_eq!(
            set(&format!("state=idle:app={}", "a".repeat(33))).1.app,
            None
        );
    }

    #[test]
    fn cleans_and_caps_the_message() {
        let (_, r) = set(&format!(
            "state=blocked:msg={}",
            b64("rm -rf\n\x1b[31mbuild\u{202e}txt.exe\x07")
        ));
        assert_eq!(r.msg.as_deref(), Some("rm -rf [31mbuildtxt.exe"));
        // Not base64: the field goes, the report stays.
        let (_, r) = set("state=blocked:kind=question:msg=@@@");
        assert_eq!(
            (r.state, r.kind, r.msg),
            (State::Blocked, Some(Kind::Question), None)
        );
        // Unpadded base64 reads too.
        assert_eq!(set("state=idle:title=aGk").1.title.as_deref(), Some("hi"));
        let long = "é".repeat(1500);
        let msg = set(&format!("state=blocked:msg={}", b64(&long)))
            .1
            .msg
            .unwrap();
        assert!(
            msg.len() <= MAX_MSG && msg.chars().all(|c| c == 'é'),
            "{}",
            msg.len()
        );
        assert_eq!(set(&format!("state=idle:msg={}", b64(" \n "))).1.msg, None);
    }

    #[test]
    fn records_by_id_and_clear() {
        let mut r = Records::default();
        for body in [
            "state=working",
            "state=working:id=build",
            "state=done:id=build/test",
            "state=error:id=lint",
            "state=idle:id=buildx",
        ] {
            r.apply(parse(body.as_bytes()).unwrap());
        }
        assert_eq!(r.len(), 5);
        // A report replaces its record whole.
        r.apply(parse(b"state=blocked:id=lint").unwrap());
        assert_eq!(r.get("lint").unwrap().state, State::Blocked);
        // Clear takes the record and those under it, not a sibling that shares a prefix.
        r.apply(parse(b"state=clear:id=build").unwrap());
        assert!(r.get("build").is_none() && r.get("build/test").is_none());
        assert!(r.get("buildx").is_some() && r.root().is_some());
        r.apply(parse(b"state=clear:id=").unwrap_or(Report::Probe));
        assert_eq!(r.len(), 3, "an empty id is a bad id: the report is ignored");
        r.apply(parse(b"state=clear").unwrap());
        assert!(r.is_empty());
    }

    #[test]
    fn a_live_report_decides_the_state() {
        use SessionState::*;
        for now in [Working, Waiting, Idle] {
            assert_eq!(session_state(Some(State::Working), now), Working);
            assert_eq!(session_state(Some(State::Blocked), now), Waiting);
            for s in [State::Idle, State::Done, State::Error] {
                assert_eq!(session_state(Some(s), now), Idle);
            }
        }
        // With no root record, the other rules decide from where it stands.
        assert_eq!(session_state(None, Waiting), Working);
        assert_eq!(session_state(None, Idle), Idle);
        assert_eq!(session_state(None, Working), Working);
        // done and error end a turn once; idle ends none.
        assert!(ends_turn(Some(State::Working), Some(State::Done)));
        assert!(ends_turn(None, Some(State::Error)));
        assert!(ends_turn(Some(State::Done), Some(State::Error)));
        assert!(!ends_turn(Some(State::Done), Some(State::Done)));
        assert!(!ends_turn(Some(State::Blocked), Some(State::Idle)));
        assert!(!ends_turn(Some(State::Done), None));
    }

    #[test]
    fn drops_the_least_recently_updated_record() {
        let mut r = Records::default();
        r.apply(parse(b"state=working").unwrap());
        for i in 0..MAX_RECORDS {
            r.apply(parse(format!("state=working:id=t{i}").as_bytes()).unwrap());
        }
        assert_eq!(r.len(), MAX_RECORDS);
        assert!(r.root().is_none() && r.get("t0").is_some());
    }

    #[test]
    fn the_view_lists_children_and_only_a_result_outlives_its_program() {
        let mut r = Records::default();
        for body in [
            "state=working:app=grok",
            "state=blocked:id=s2:kind=question",
            "state=working:id=s1",
        ] {
            r.apply(parse(body.as_bytes()).unwrap());
        }
        let view = r.view().unwrap();
        let ids: Vec<_> = view
            .children
            .iter()
            .map(|c| (c.id.as_str(), c.state.as_str()))
            .collect();
        assert_eq!(ids, [("s1", "working"), ("s2", "blocked")]);
        assert_eq!(view.children[1].kind.as_deref(), Some("question"));
        assert_eq!(
            r.clone().end(),
            None,
            "a working program left nothing to read"
        );
        r.apply(parse(b"state=error:msg=Ym9vbQ==").unwrap());
        let kept = r.end().unwrap();
        assert_eq!(
            (kept.state.as_str(), kept.ended, kept.msg.as_deref()),
            ("error", true, Some("boom"))
        );
        assert!(kept.children.is_empty() && r.is_empty());
        let mut a = view.clone();
        a.msg = Some("Thinking".into());
        assert!(Records::minor_change(&view, &a));
        a.state = "idle".into();
        assert!(!Records::minor_change(&view, &a));
    }
}
