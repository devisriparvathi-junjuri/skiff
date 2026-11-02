//! `skiff wait`: block until sessions reach a state.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{bail, Result};
use skiff_client::Client;
use skiff_core::{
    protocol::Event,
    session::{SessionInfo, SessionState},
};
use tokio::{
    sync::broadcast::error::{RecvError, TryRecvError},
    time::Instant,
};

use crate::{print, resolve, sessions, watch};

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Target {
    Idle,
    Waiting,
    Done,
    /// Idle, waiting or done.
    AnyNotWorking,
    /// The turn ended, a result to read: the program reported done or error
    /// (OSC 7501), or the pane's turn ended and is unread.
    Turn,
}

impl Target {
    fn hit(self, s: SessionState) -> bool {
        match self {
            Target::Idle => s == SessionState::Idle,
            Target::Waiting => s == SessionState::Waiting,
            Target::Done => s == SessionState::Done,
            Target::AnyNotWorking => s != SessionState::Working,
            // A state alone is no turn's end; see [`Now::Turned`].
            Target::Turn => false,
        }
    }

    fn hits(self, n: &Now) -> bool {
        match *n {
            Now::State(s) => self.hit(s),
            Now::Turned { .. } => matches!(self, Target::Turn),
            Now::Gone => false,
        }
    }
}

/// The session's turn already ended and is not read: it is idle and unread,
/// or its program reported done or error. `Some(failed)`.
fn turned(s: &SessionInfo) -> Option<bool> {
    let reported = s.program_status.as_ref().map(|p| p.state.as_str());
    let ended = matches!(reported, Some("done" | "error")) || s.unread;
    (s.state == SessionState::Idle && ended).then_some(reported == Some("error"))
}

/// The timeout of a wait an agent runs when it names none, and the most it
/// can name: the agent's shell tool stops a call after about a minute.
const AGENT_DEFAULT: f64 = 45.0;
const AGENT_CAP: f64 = 60.0;
/// How often an idle agent that shows its busy line is read again.
const RECHECK: Duration = Duration::from_secs(2);

/// The timeout to use. `agent`: the caller is an agent, or a script with no
/// terminal. Then the wait has the agent default and cap.
pub fn timeout_for(asked: Option<f64>, agent: bool) -> Option<f64> {
    match (asked, agent) {
        (_, false) => asked,
        (None, true) => Some(AGENT_DEFAULT),
        (Some(t), true) => Some(t.min(AGENT_CAP)),
    }
}

/// The caller runs in a Skiff pane, or has no terminal on stdout.
pub fn agent_caller() -> bool {
    std::env::var("SKIFF_SESSION").is_ok_and(|s| !s.is_empty()) || unsafe { libc::isatty(1) } != 1
}

/// Session `id` is idle, a known agent is in front, and its busy line
/// shows: it works through a quiet stretch.
async fn busy(c: &Client, id: &str) -> Result<bool> {
    let all = c.list_sessions().await?;
    let Some(s) = all.iter().find(|s| s.id == id) else { return Ok(false) };
    if s.state != SessionState::Idle || !sessions::is_agent(s) {
        return Ok(false);
    }
    Ok(watch::read_front(c, s).await? == watch::Front::Busy)
}

/// Where one session stands.
#[derive(Clone, Copy, PartialEq)]
enum Now {
    State(SessionState),
    /// Its turn ended. `failed`: the program reported an error.
    Turned { failed: bool },
    /// Closed, or killed.
    Gone,
}

/// Exit codes: 0 when every session got there, 1 on timeout, 2 when a
/// session went away or exited before it got there, 3 when a session is
/// waiting, 4 with `--for turn` when a turn ended in an error the program
/// reported. Only a person answers an approval, so unless the target is
/// `waiting`, a waiting session ends the wait at once.
///
/// With `any`, the first session to get there ends the wait with 0, and
/// only that session prints. 2 then means every session went away or
/// exited first.
///
/// With `--for idle`, an idle known agent that shows its busy line does not
/// count until the line goes.
pub async fn wait(c: &Client, args: &[String], target: Target, any: bool, timeout: Option<f64>) -> Result<i32> {
    // Listen before the list, so no change falls between the two.
    let mut events = c.events();
    let all = sessions::sorted(c).await?;
    let mut ids: Vec<String> = Vec::new();
    let mut names = HashMap::new();
    for a in args {
        let s = match resolve::session(&all, a) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("skiff: {e:#}");
                return Ok(2);
            }
        };
        if !ids.contains(&s.id) {
            ids.push(s.id.clone());
            names.insert(s.id.clone(), resolve::display_name(s).to_string());
        }
    }
    let for_turn = matches!(target, Target::Turn);
    let mut now: HashMap<String, Now> = all
        .iter()
        .filter(|s| ids.contains(&s.id))
        .map(|s| {
            let n = match turned(s) {
                Some(failed) if for_turn => Now::Turned { failed },
                _ => Now::State(s.state),
            };
            (s.id.clone(), n)
        })
        .collect();
    let stop_on_waiting = !matches!(target, Target::Waiting);
    let needs_person = |id: &String| -> i32 {
        eprintln!("skiff: {}", sessions::waiting_msg(&names[id], id));
        3
    };
    if stop_on_waiting {
        if let Some(id) = ids.iter().find(|id| now[*id] == Now::State(SessionState::Waiting)) {
            return Ok(needs_person(id));
        }
    }

    // Idle by state, but the busy line shows: counted as working.
    let check_busy = matches!(target, Target::Idle);
    let mut held: HashSet<String> = HashSet::new();
    if check_busy {
        for id in &ids {
            if now[id] == Now::State(SessionState::Idle) && busy(c, id).await? {
                now.insert(id.clone(), Now::State(SessionState::Working));
                held.insert(id.clone());
            }
        }
    }

    // A timeout too large to count is no timeout.
    let deadline = timeout
        .and_then(|t| Duration::try_from_secs_f64(t.max(0.0)).ok())
        .and_then(|d| Instant::now().checked_add(d));
    // Events queued before the list reply are older than the list.
    while !matches!(events.try_recv(), Err(TryRecvError::Empty | TryRecvError::Closed)) {}
    // A session settles when it reaches the target, or can no longer reach it.
    let settled = |n: &Now| match *n {
        Now::State(s) => target.hit(s) || s == SessionState::Done,
        Now::Turned { .. } | Now::Gone => true,
    };
    let hit = |n: &Now| target.hits(n);
    let finished = |now: &HashMap<String, Now>| {
        (any && ids.iter().any(|id| hit(&now[id]))) || ids.iter().all(|id| settled(&now[id]))
    };
    let mut timed_out = false;
    while !finished(&now) {
        let next = async {
            match deadline {
                Some(d) => tokio::time::timeout_at(d, events.recv()).await.ok(),
                None => Some(events.recv().await),
            }
        };
        let e = if held.is_empty() {
            next.await
        } else {
            tokio::select! {
                e = next => e,
                _ = tokio::time::sleep(RECHECK) => {
                    for id in held.clone() {
                        let all = c.list_sessions().await?;
                        match all.iter().find(|s| s.id == id) {
                            // Not idle now: the state events tell the rest.
                            Some(s) if s.state != SessionState::Idle => {
                                held.remove(&id);
                            }
                            None => {
                                held.remove(&id);
                            }
                            Some(_) if !busy(c, &id).await? => {
                                held.remove(&id);
                                now.insert(id, Now::State(SessionState::Idle));
                            }
                            Some(_) => {}
                        }
                    }
                    continue;
                }
            }
        };
        let Some(e) = e else {
            timed_out = true;
            break;
        };
        let (id, n) = match e {
            Ok(Event::State { session, state }) => (session, Now::State(state)),
            Ok(Event::Exit { session, .. }) => (session, Now::State(SessionState::Done)),
            Ok(Event::SessionRemoved { session }) => (session, Now::Gone),
            // The report that ended the turn comes in the update after it:
            // read it from the session.
            Ok(Event::TurnEnded { session }) if for_turn && ids.contains(&session) => {
                let all = c.list_sessions().await?;
                let failed = all
                    .iter()
                    .find(|s| s.id == session)
                    .and_then(|s| s.program_status.as_ref())
                    .is_some_and(|p| p.state == "error");
                (session, Now::Turned { failed })
            }
            Ok(_) => continue,
            Err(RecvError::Lagged(_)) => {
                // Missed events: read the states again.
                let all = c.list_sessions().await?;
                if stop_on_waiting {
                    let waiting = |id: &&String| all.iter().any(|s| &s.id == *id && s.state == SessionState::Waiting);
                    if let Some(id) = ids.iter().find(waiting) {
                        return Ok(needs_person(id));
                    }
                }
                for id in &ids {
                    let cur = now.get_mut(id).expect("listed");
                    if !settled(cur) {
                        *cur = all.iter().find(|s| &s.id == id).map_or(Now::Gone, |s| match turned(s) {
                            Some(failed) if for_turn => Now::Turned { failed },
                            _ => Now::State(s.state),
                        });
                        held.remove(id);
                        if check_busy && *cur == Now::State(SessionState::Idle) && busy(c, id).await? {
                            *cur = Now::State(SessionState::Working);
                            held.insert(id.clone());
                        }
                    }
                }
                continue;
            }
            Err(RecvError::Closed) => bail!("skiffd closed the connection"),
        };
        if stop_on_waiting && n == Now::State(SessionState::Waiting) && ids.contains(&id) {
            return Ok(needs_person(&id));
        }
        // Once settled, a session keeps the state it settled in.
        if let Some(cur) = now.get_mut(&id) {
            if !settled(cur) {
                *cur = n;
                held.remove(&id);
                if check_busy && n == Now::State(SessionState::Idle) && busy(c, &id).await? {
                    *cur = Now::State(SessionState::Working);
                    held.insert(id);
                }
            }
        }
    }

    if any {
        let first = ids.iter().find(|id| hit(&now[*id]));
        if let Some(id) = first {
            // The full id, so a script can pass it on as it is.
            let row = vec![id.clone(), names[id].clone(), word(now[id]).to_string()];
            print::table(&["", "", ""], &[row]);
            return Ok(if now[id] == (Now::Turned { failed: true }) { 4 } else { 0 });
        }
    }
    let mut code = 0;
    let rows: Vec<Vec<String>> = ids
        .iter()
        .map(|id| {
            let n = now[id];
            let c = match n {
                Now::Turned { failed: true } => 4,
                _ if hit(&n) => 0,
                _ if !settled(&n) => 1,
                _ => 2,
            };
            code = code.max(c);
            vec![print::short_id(id).to_string(), names[id].clone(), word(n).to_string()]
        })
        .collect();
    // With `any`, a session that went away first does not count while
    // another can still get there: that is a timeout, not 2.
    if any && timed_out {
        code = 1;
    }
    print::table(&["", "", ""], &rows);
    if timed_out && code == 1 {
        eprintln!("skiff: timed out");
    }
    Ok(code)
}

/// What a session's line says.
fn word(n: Now) -> &'static str {
    match n {
        Now::State(s) => sessions::state(s),
        Now::Turned { failed: false } => "turn ended",
        Now::Turned { failed: true } => "failed",
        Now::Gone => "gone",
    }
}

#[cfg(test)]
mod tests {
    use super::{timeout_for, turned};
    use skiff_core::session::SessionInfo;

    #[test]
    fn a_turn_ends_with_a_result_or_an_unread_idle() {
        let s = |state: &str, unread: bool, status: Option<&str>| -> SessionInfo {
            let mut v = serde_json::json!({
                "id": "a", "label": "a", "role": "agent", "cwd": "/", "command": "claude", "args": [],
                "state": state, "cols": 80, "rows": 24, "pid": null, "exit_code": null, "unread": unread,
            });
            if let Some(st) = status {
                v["program_status"] = serde_json::json!({ "state": st });
            }
            serde_json::from_value(v).unwrap()
        };
        assert_eq!(turned(&s("idle", false, None)), None, "quiet is no turn's end");
        assert_eq!(turned(&s("idle", true, None)), Some(false));
        assert_eq!(turned(&s("idle", false, Some("done"))), Some(false));
        assert_eq!(turned(&s("idle", false, Some("error"))), Some(true));
        assert_eq!(turned(&s("idle", false, Some("idle"))), None);
        assert_eq!(turned(&s("working", true, Some("working"))), None);
    }

    #[test]
    fn an_agent_gets_a_default_and_a_cap_and_a_person_gets_what_they_ask() {
        assert_eq!(timeout_for(None, true), Some(45.0));
        assert_eq!(timeout_for(Some(600.0), true), Some(60.0));
        assert_eq!(timeout_for(Some(10.0), true), Some(10.0));
        assert_eq!(timeout_for(None, false), None);
        assert_eq!(timeout_for(Some(600.0), false), Some(600.0));
    }
}
