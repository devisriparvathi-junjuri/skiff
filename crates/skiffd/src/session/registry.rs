//! The pool's registry: construction, the change gate, list and metrics, and watch publication.

use super::*;

impl SessionPool {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Permission to change sessions or groups, or to start a process.
    /// Refused while a reload runs: the change would miss the handover, and
    /// a new process could inherit fds the reload passes on.
    pub(crate) fn change(&self) -> Result<RwLockReadGuard<'_, ()>> {
        let guard = self.changes.read().unwrap();
        if self.reloading.load(Ordering::Acquire) {
            bail!("skiffd is reloading; try again");
        }
        Ok(guard)
    }

    /// For the watchers: `None` while a reload runs.
    pub(super) fn quiet(&self) -> Option<RwLockReadGuard<'_, ()>> {
        self.change().ok()
    }

    /// The listening socket, for a reload to hand over.
    pub fn set_listener(&self, fd: i32) {
        self.listener.store(fd, Ordering::Relaxed);
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut v: Vec<SessionInfo> = self
            .sessions
            .read()
            .unwrap()
            .values()
            .map(|s| s.info())
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn metrics(&self) -> skiff_core::protocol::DaemonMetrics {
        let pid = std::process::id();
        let memory_bytes = std::fs::read_to_string(format!("/proc/{pid}/statm"))
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|pages| pages * 4096)
            .unwrap_or(0);
        skiff_core::protocol::DaemonMetrics {
            version: skiff_core::VERSION.to_string(), pid, started_at: self.started_at.load(Ordering::Relaxed),
            memory_bytes, session_count: self.sessions.read().unwrap().len() as u64,
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.read().unwrap().get(id).cloned()
    }

    /// Registers connection `conn` as a watcher of `session`, or replaces its
    /// registration for that session. A replacement keeps `since`. One
    /// connection may watch several sessions: a Skipper's watcher registers
    /// on the grand Skipper and on each project Skipper.
    pub fn watching(
        &self,
        conn: u64,
        pid: Option<u32>,
        session: &str,
        failure: Option<WatchFailure>,
        follows: Option<Vec<SessionId>>,
    ) -> Result<()> {
        if self.get(session).is_none() {
            bail!("no such session: {session}");
        }
        {
            let mut w = self.watchers.lock().unwrap();
            let key = (conn, session.to_string());
            let since = w.get(&key).map_or_else(now_ms, |o| o.since);
            w.insert(key, Watcher { session: session.to_string(), pid, since, failure, follows });
        }
        self.publish_watch(session);
        Ok(())
    }

    /// Connection `conn` closed: its registrations, if any, end.
    pub fn unwatch(&self, conn: u64) {
        let gone: Vec<Watcher> = {
            let mut w = self.watchers.lock().unwrap();
            let keys: Vec<(u64, SessionId)> = w.keys().filter(|k| k.0 == conn).cloned().collect();
            keys.into_iter().filter_map(|k| w.remove(&k)).collect()
        };
        for old in gone {
            self.publish_watch(&old.session);
        }
    }

    /// Writes the registry's view of `id` into its info, and sends
    /// `SessionUpdated` when it changed.
    pub(super) fn publish_watch(&self, id: &str) {
        let Some(session) = self.get(id) else { return };
        let watch = {
            let w = self.watchers.lock().unwrap();
            let mine: Vec<&Watcher> = w.values().filter(|x| x.session == id).collect();
            (!mine.is_empty()).then(|| WatchInfo {
                since: mine.iter().map(|x| x.since).min().unwrap_or(0),
                pids: {
                    let mut p: Vec<u32> = mine.iter().filter_map(|x| x.pid).collect();
                    p.sort_unstable();
                    p
                },
                failure: mine.iter().filter_map(|x| x.failure.clone()).max_by_key(|f| f.at),
                // The union of what the watchers report. Unknown when none does.
                follows: mine.iter().any(|x| x.follows.is_some()).then(|| {
                    let mut f: Vec<SessionId> = mine.iter().filter_map(|x| x.follows.as_ref()).flatten().cloned().collect();
                    f.sort_unstable();
                    f.dedup();
                    f
                }),
            })
        };
        let changed = {
            let mut info = session.info.lock().unwrap();
            let changed = info.watch != watch;
            info.watch = watch;
            changed
        };
        if changed {
            let _ = self.events.send(Event::SessionUpdated { session: session.info() });
        }
    }
}
