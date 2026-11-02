//! PTY lifecycle: spawn, the reader, reaping, resize and kill.

use super::*;

impl SessionPool {
    pub fn create(self: &Arc<Self>, spec: SessionSpec) -> Result<SessionInfo> {
        let _change = self.change()?;
        self.spawn(spec, None)
    }

    /// Starts a session. `id` is the old one when a restore brings it back.
    pub(super) fn spawn(self: &Arc<Self>, spec: SessionSpec, id: Option<SessionId>) -> Result<SessionInfo> {
        let command = spec
            .command
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(default_shell);
        let cwd = spec
            .cwd
            .clone()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        // Match the realpaths git reports for worktrees.
        let cwd = cwd.canonicalize().unwrap_or(cwd);
        let label = if spec.label.is_empty() {
            PathBuf::from(&command)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| command.clone())
        } else {
            spec.label.clone()
        };

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: spec.rows,
                cols: spec.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("openpty: {e}"))?;

        let id: SessionId =
            id.unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..12].to_string());
        let (cmd, is_shell) = launch::launch_command(&spec, &command, &cwd, &id);
        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawn {command} in {}", cwd.display()))?;
        drop(pair.slave);
        // From here on the session is its master fd and the child's pid. The
        // reader reaps the pid; dropping `child` neither waits nor kills.
        let pid = child.process_id();
        drop(child);
        let pty = Pty::from_master(&*pair.master)?;
        drop(pair.master);

        let info = SessionInfo {
            id: id.clone(),
            label,
            role: spec.role,
            cwd,
            command,
            foreground_program: None,
            args: spec.args,
            state: SessionState::Working,
            cols: spec.cols,
            rows: spec.rows,
            pid,
            exit_code: None,
            theme: None,
            name: spec.name.filter(|name| !name.trim().is_empty()),
            title: None,
            started_at: now_ms(),
            last_output_at: now_ms(),
            output_bytes: 0,
            away: Vec::new(),
            was: None,
            resume: None,
            unread: false,
            agent_exit: None,
            peek: spec.peek,
            interactive: false,
            input_gen: 0,
            coordinator: spec.coordinator,
            skipper: spec.skipper.filter(|p| !p.trim().is_empty()),
            retiring: false,
            watch: None,
            program_status: None,
        };

        let (input, queued, wrote) = start_writer(&id, &pty)?;
        let (output, _) = broadcast::channel(4096);
        let session = Arc::new(Session {
            info: Mutex::new(info.clone()),
            pty,
            input,
            queued,
            wrote,
            waker: Poller::new().context("poller")?,
            park: Park::default(),
            output,
            last_output: AtomicU64::new(info.last_output_at),
            last_input: AtomicU64::new(0),
            key_pending: AtomicBool::new(false),
            last_work: AtomicU64::new(info.last_output_at),
            work_started: AtomicU64::new(info.last_output_at),
            fresh: AtomicBool::new(true),
            busy_seen: AtomicBool::new(false),
            title_state: Mutex::new(None),
            shell: AtomicBool::new(is_shell),
            // The foreground watcher names the agent within a second.
            front: Mutex::new(if is_shell { Front::Shell } else { Front::Program }),
            group: AtomicU32::new(0),
            belled: AtomicBool::new(false),
            records: Mutex::new(Records::default()),
            queries: AtomicU32::new(0),
            queried_at: AtomicU64::new(0),
            approval_since: AtomicU64::new(0),
            raised: AtomicBool::new(false),
            stalled: AtomicBool::new(false),
            status_sent_at: AtomicU64::new(0),
            status_dirty: AtomicBool::new(false),
            screen: Mutex::new(ScreenState {
                screen: Screen::new(spec.cols, spec.rows),
                pending: Vec::new(),
                held_since: Instant::now(),
                last_push: Instant::now(),
                last_scan: Instant::now() - SCAN,
                unscanned: false,
                printed: false,
                printed_work: false,
                echo: None,
            }),
        });
        self.sessions
            .write()
            .unwrap()
            .insert(id.clone(), session.clone());
        let _ = self.events.send(Event::SessionCreated {
            session: info.clone(),
        });
        self.dirty.notify_one();
        if info.peek {
            self.watch_peek(&id);
        }
        self.start_reader(session)?;
        Ok(info)
    }

    /// The thread that reads the PTY until the child is gone, feeds the
    /// screen, and reaps the child by its pid. It waits with a deadline only
    /// while output is held or unscanned, so an idle session never wakes.
    /// A reload can pause it between reads; see [`Session::park_point`].
    pub(super) fn start_reader(self: &Arc<Self>, session: Arc<Session>) -> Result<()> {
        let mut reader = session.pty.reader().context("clone reader")?;
        // SAFETY: the reader thread owns `reader` and deletes it from the
        // poller before it closes.
        unsafe { session.waker.add(&reader, polling::Event::readable(0)) }.context("watch PTY")?;
        let pool = self.clone();
        let sid = session.info.lock().unwrap().id.clone();
        std::thread::Builder::new()
            .name(format!("pty-{sid}"))
            .spawn(move || {
                let mut buf = vec![0u8; 16 * 1024];
                let mut events = polling::Events::new();
                loop {
                    session.park_point();
                    let timeout = session
                        .next_due()
                        .map(|due| due.saturating_duration_since(Instant::now()));
                    events.clear();
                    match session.waker.wait(&mut events, timeout) {
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                    // The deadline, or a wake from `pause_reader` or `set_front`.
                    if events.is_empty() {
                        let (signals, scan) = session.on_due();
                        pool.apply_read(&session, &sid, signals, scan, false);
                        continue;
                    }
                    let n = match reader.read(&mut buf) {
                        // EIO: the child closed the terminal.
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    pool.on_output(&session, &sid, &buf[..n]);
                    if session.waker.modify(&reader, polling::Event::readable(0)).is_err() {
                        break;
                    }
                }
                let _ = session.waker.delete(&reader);
                drop(reader);
                session.flush_output();
                pool.reap(&session, &sid);
            })
            .context("spawn reader thread")?;
        Ok(())
    }

    /// One read's worth of output: the screen, the bell, the state, the title.
    pub(super) fn on_output(&self, session: &Arc<Session>, sid: &str, chunk: &[u8]) {
        let now = now_ms();
        session.last_output.store(now, Ordering::Relaxed);
        {
            let mut info = session.info.lock().unwrap();
            info.output_bytes = info.output_bytes.saturating_add(chunk.len() as u64);
            // So a copy of the info, as the updates send, has it too.
            info.last_output_at = now;
        }
        let (signals, scan, echo) = session.push_output(chunk);
        self.apply_read(session, sid, signals, scan, echo);
    }

    /// What one read brought, or the end of a synchronized update that held
    /// it: the reports, open queries, the bell, the screen rules, the title.
    pub(super) fn apply_read(
        &self,
        session: &Arc<Session>,
        sid: &str,
        mut signals: Signals,
        scan: Scan,
        echo: bool,
    ) {
        self.apply_status(session, std::mem::take(&mut signals.status));
        // Output again: a stalled program is not.
        if session.stalled.swap(false, Ordering::Relaxed) {
            let cleared = {
                let mut info = session.info.lock().unwrap();
                info.program_status
                    .as_mut()
                    .is_some_and(|p| std::mem::take(&mut p.stalled))
            };
            if cleared {
                let _ = self.events.send(Event::SessionUpdated {
                    session: session.info(),
                });
            }
        }
        if signals.queries > 0 {
            let open = session
                .queries
                .load(Ordering::Relaxed)
                .saturating_add(signals.queries);
            session
                .queries
                .store(open.min(MAX_QUERIES), Ordering::Relaxed);
            session.queried_at.store(now_ms(), Ordering::Relaxed);
        }
        // A bell that answers a key is no call: the user is at the
        // keys. The BEL that ends a title sequence is not a bell.
        if signals.bell && !echo {
            self.bell(session);
        }
        // A program that starts or ends prints. A new group in front wakes
        // the foreground watcher, which reads /proc on its own thread.
        if scan.scanned {
            if let Some(group) = session.foreground_group() {
                if group != session.group.load(Ordering::Relaxed) {
                    foreground::wake();
                }
            }
        }
        self.apply_scan(session, scan);
        if let Some(title) = signals.title {
            let title = title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
            let handoff = title.as_deref().and_then(|t| t.strip_prefix(SHELL_HANDOFF_TITLE));
            if let Some(code) = handoff {
                // The agent that owned this pane just exited and the
                // wrapper is about to exec the fallback shell in its
                // place. Forget the agent's label and command so the
                // icon falls back to a plain shell instead of the
                // agent that is no longer running. Its flags go
                // too: a restore must not pass them to the shell.
                let shell_name = PathBuf::from(default_shell())
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(default_shell);
                session.shell.store(true, Ordering::Relaxed);
                let mut records = session.records.lock().unwrap();
                // The group stays. Forget it, so the watcher reads the
                // shell in front now instead of at its next recheck.
                session.group.store(0, Ordering::Relaxed);
                foreground::wake();
                let info = {
                    let mut info = session.info.lock().unwrap();
                    info.title = None;
                    info.label = shell_name.clone();
                    info.command = shell_name;
                    info.args.clear();
                    info.agent_exit = code.strip_prefix(':').and_then(|c| c.parse().ok());
                    end_reports(&mut records, &mut info);
                    info.clone()
                };
                drop(records);
                let _ = self.events.send(Event::SessionUpdated { session: info });
                self.dirty.notify_one();
            } else {
                self.apply_title(session, title.as_deref());
                // An agent's spinner changes the glyph, not the topic. A
                // frame of it is no new title. The raw title is kept, so
                // the state rules above and the restore offer read it as
                // before.
                let agent = matches!(session.front(), Front::Agent(_));
                let changed = {
                    let mut info = session.info.lock().unwrap();
                    let changed = if agent {
                        info.title.as_deref().map(topic) != title.as_deref().map(topic)
                    } else {
                        info.title != title
                    };
                    info.title = title.clone();
                    changed
                };
                if changed {
                    let _ = self.events.send(Event::Title {
                        session: sid.to_string(),
                        title,
                    });
                }
            }
        }
    }

    /// Waits for the child to exit, then records its exit. Only this pid is
    /// reaped, never another child of the daemon. A reload can pause the
    /// wait; the exit is reaped and recorded in one step, before or after.
    pub(super) fn reap(&self, session: &Arc<Session>, sid: &str) {
        let pid = session.info.lock().unwrap().pid;
        let code = loop {
            session.park_point();
            let Some(pid) = pid else { break None };
            match pty::try_reap(pid) {
                pty::Reap::Exited(code) => break Some(code),
                pty::Reap::Gone => break None,
                pty::Reap::Running => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let reported = {
            let mut records = session.records.lock().unwrap();
            let mut info = session.info.lock().unwrap();
            info.exit_code = code;
            info.state = SessionState::Done;
            end_reports(&mut records, &mut info)
        };
        session.set_reader_done();
        self.dying.lock().unwrap().retain(|s| !Arc::ptr_eq(s, session));
        let _ = self.events.send(Event::State {
            session: sid.to_string(),
            state: SessionState::Done,
        });
        let _ = self.events.send(Event::Exit { session: sid.to_string(), code });
        if reported {
            let _ = self.events.send(Event::SessionUpdated {
                session: session.info(),
            });
        }
    }

    /// The emulator reflows 2000 lines of scrollback, so the whole resize
    /// runs on a blocking thread and the executor serves other sessions
    /// meanwhile. The change guard holds a reload until the pty, the screen
    /// and the info agree. The screen lock covers the ioctl and the reflow,
    /// so the redraw the program sends for the new size lands on a screen
    /// of that size. It also covers the info and the event: two clients
    /// resizing at once apply and announce in the same order, so the last
    /// event every client hears is the size the pty has.
    pub async fn resize(self: &Arc<Self>, id: &str, cols: u16, rows: u16) -> Result<()> {
        let pool = self.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            let _change = pool.change()?;
            let session = pool.get(&id).ok_or_else(|| anyhow!("no such session: {id}"))?;
            // The program redraws for the new size. That is not work either.
            // Stored before the ioctl, as a write does.
            session.last_input.store(now_ms(), Ordering::Relaxed);
            let mut st = session.screen.lock().unwrap();
            session.pty.resize(cols, rows)?;
            st.screen.resize(cols, rows);
            {
                let mut info = session.info.lock().unwrap();
                info.cols = cols;
                info.rows = rows;
            }
            let _ = pool.events.send(Event::SessionUpdated { session: session.info() });
            drop(st);
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("resize failed: {e}"))??;
        Ok(())
    }

    /// Terminates the process and forgets the session. Drops it from every
    /// group; a group left with no session keeps an empty slot.
    pub fn kill(&self, id: &str) -> Result<()> {
        let _change = self.change()?;
        let mut groups = self.groups.lock().unwrap();
        let session = self
            .sessions
            .write()
            .unwrap()
            .remove(id)
            .ok_or_else(|| anyhow!("no such session: {id}"))?;
        // A reaped pid can belong to another process by now.
        let (pid, cwd) = {
            let info = session.info.lock().unwrap();
            (info.pid.filter(|_| info.state != SessionState::Done), info.cwd.clone())
        };
        if let Some(pid) = pid {
            pty::hang_up(pid);
        }
        if !session.reader_done() {
            self.dying.lock().unwrap().push(session.clone());
        }
        let pruned = prune(&mut groups, id, &cwd);
        drop(groups);
        let _ = self.events.send(Event::SessionRemoved {
            session: id.to_string(),
        });
        if pruned {
            let _ = self.events.send(Event::GroupsChanged {});
        }
        self.dirty.notify_one();
        Ok(())
    }

    /// A peek tool that takes the terminal is a terminal tool: the app shows
    /// its peek then. One that never does is an app with a window of its own,
    /// or a command that prints and exits.
    pub(super) fn watch_peek(self: &Arc<Self>, id: &str) {
        let pool = Arc::downgrade(self);
        let sid = id.to_string();
        let _ = std::thread::Builder::new().name(format!("peek-{sid}")).spawn(move || loop {
            std::thread::sleep(Duration::from_millis(40));
            let Some(pool) = pool.upgrade() else { break };
            let Some(s) = pool.get(&sid) else { break };
            if s.info.lock().unwrap().state == SessionState::Done {
                break;
            }
            if s.takes_terminal() {
                let session = {
                    let mut info = s.info.lock().unwrap();
                    info.interactive = true;
                    info.clone()
                };
                let _ = pool.events.send(Event::SessionUpdated { session });
                break;
            }
        });
    }
}

pub(super) fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}
