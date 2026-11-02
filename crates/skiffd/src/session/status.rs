//! OSC 7501 session status policy.

use super::*;

impl SessionPool {
    /// Applies what the program reported with OSC 7501, in order. While a
    /// root record lives, it decides the state; see
    /// [`program_status::session_state`] and [`program_status::ends_turn`].
    pub(super) fn apply_status(&self, session: &Session, signals: Vec<StatusSignal>) {
        if signals.is_empty() {
            return;
        }
        // A shell at its prompt reports nothing. A report while it is in
        // front comes from a command that is gone, or from printed text, and
        // no return of the prompt would ever drop it.
        if session.front() == Front::Shell {
            return;
        }
        // Records belong to the process group in front when they came. A
        // report under another group is another program's: the old records
        // went with their program.
        let group = session.foreground_group().unwrap_or(0);
        let mut records = session.records.lock().unwrap();
        if group != 0 && records.group != 0 && records.group != group {
            *records = Records::default();
        }
        let old = records.root().map(|r| r.state);
        for signal in signals {
            match signal {
                StatusSignal::Reset => records.clear(),
                StatusSignal::Report(body) => {
                    if let Some(report) = program_status::parse(&body) {
                        records.apply(report);
                    }
                }
            }
        }
        if !records.is_empty() {
            records.group = group;
        }
        let claim = records.root().map(|r| r.state);
        let (state, ended, id, send) = {
            let mut info = session.info.lock().unwrap();
            // A result kept from a program that left stays while no new
            // records replace it.
            let view = records.view().or_else(|| {
                info.program_status
                    .clone()
                    .filter(|p| p.ended && old.is_none() && records.is_empty())
            });
            // A turn ends with done or error, or for a program that never
            // says them, with idle after a turn the user started.
            let ended = records
                .turns
                .step(old, claim, now_ms(), info.input_gen, info.output_bytes);
            drop(records);
            if info.state == SessionState::Done || info.program_status == view {
                return;
            }
            let minor = matches!(
                (&info.program_status, &view),
                (Some(a), Some(b)) if Records::minor_change(a, b)
            );
            info.program_status = view;
            session.stalled.store(false, Ordering::Relaxed);
            // The grace for an approval prompt starts again from this report.
            session.approval_since.store(0, Ordering::Relaxed);
            session.raised.store(false, Ordering::Relaxed);
            // The quiet time starts now, should the reports end.
            session.last_work.store(now_ms(), Ordering::Relaxed);
            if claim.is_some() {
                session.belled.store(false, Ordering::Relaxed);
            }
            if ended {
                info.unread = true;
                session.fresh.store(false, Ordering::Relaxed);
                session.busy_seen.store(false, Ordering::Relaxed);
            }
            let state = program_status::session_state(claim, info.state);
            let changed = info.state != state;
            if changed && state == SessionState::Working && info.state == SessionState::Idle {
                session.work_started.store(now_ms(), Ordering::Relaxed);
            }
            info.state = state;
            let now = now_ms();
            let send = !minor
                || now.saturating_sub(session.status_sent_at.load(Ordering::Relaxed))
                    >= STATUS_EVERY_MS;
            if send {
                session.status_sent_at.store(now, Ordering::Relaxed);
            }
            session.status_dirty.store(!send, Ordering::Relaxed);
            (changed.then_some(state), ended, info.id.clone(), send)
        };
        if let Some(state) = state {
            let _ = self.events.send(Event::State {
                session: id.clone(),
                state,
            });
        }
        if ended {
            let _ = self.events.send(Event::TurnEnded {
                session: id.clone(),
            });
        }
        // Read again out of the lock: `info()` adds the time of the last output.
        if send {
            let _ = self.events.send(Event::SessionUpdated {
                session: session.info(),
            });
        }
    }

    /// Another process group came in front of the PTY. Reports made under
    /// the one before are its program's: they stop deciding the state, and
    /// only a done or error stays to be read.
    pub(crate) fn status_group(&self, session: &Session, group: u32) {
        let (state, updated) = {
            let mut records = session.records.lock().unwrap();
            if records.is_empty() || records.group == 0 || records.group == group {
                return;
            }
            let mut info = session.info.lock().unwrap();
            if info.state == SessionState::Done {
                return;
            }
            let changed = end_reports(&mut records, &mut info);
            let state = program_status::session_state(None, info.state);
            let moved = (state != info.state).then(|| {
                info.state = state;
                (info.id.clone(), state)
            });
            (moved, changed.then(|| info.clone()))
        };
        if let Some((session, state)) = state {
            let _ = self.events.send(Event::State { session, state });
        }
        if let Some(session) = updated {
            let _ = self.events.send(Event::SessionUpdated { session });
        }
    }

    /// Once a tick, for a program that reports: its approval prompt that
    /// outstayed the grace, a long silence while it says it works, and an
    /// update held back by [`STATUS_EVERY_MS`].
    pub(super) fn status_tick(&self, s: &Session) {
        let now = now_ms();
        let (state, updated) = {
            let mut info = s.info.lock().unwrap();
            if info.state == SessionState::Done || !live(&info) {
                return;
            }
            let mut changed = s.status_dirty.swap(false, Ordering::Relaxed);
            let mut state = None;
            let working = info
                .program_status
                .as_ref()
                .is_some_and(|p| p.state == "working");
            let quiet = now.saturating_sub(s.last_output.load(Ordering::Relaxed)) >= STALL_MS;
            if let Some(p) = info
                .program_status
                .as_mut()
                .filter(|p| working && quiet && !p.stalled)
            {
                p.stalled = true;
                s.stalled.store(true, Ordering::Relaxed);
                changed = true;
            }
            let since = s.approval_since.load(Ordering::Relaxed);
            if working
                && since != 0
                && now.saturating_sub(since) >= GRACE_MS
                && info.state == SessionState::Working
                && !s.raised.swap(true, Ordering::Relaxed)
            {
                info.state = SessionState::Waiting;
                state = Some((info.id.clone(), SessionState::Waiting));
            }
            if changed {
                s.status_sent_at.store(now, Ordering::Relaxed);
            }
            (state, changed)
        };
        if let Some((session, state)) = state {
            let _ = self.events.send(Event::State { session, state });
        }
        if updated {
            let _ = self
                .events
                .send(Event::SessionUpdated { session: s.info() });
        }
    }
}
