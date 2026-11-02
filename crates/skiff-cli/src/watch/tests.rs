use super::*;

#[test]
fn idle_counts_after_work_or_while_unread_and_is_told_once() {
    let mut p = Pane::default();
    // Quiet and seen: nothing to tell.
    assert_eq!(p.observe(Now::Idle { unread: false }), None);
    // A start-up screen: work with no turn marked.
    assert_eq!(p.observe(Now::Working), None);
    assert_eq!(p.observe(Now::Idle { unread: false }), None);
    assert_eq!(p.observe(Now::Waiting), Some(Fact::Waiting));
    assert_eq!(p.observe(Now::Waiting), None);
    assert_eq!(p.observe(Now::Working), None);
    assert_eq!(p.observe(Now::Idle { unread: true }), Some(Fact::Idle));
    assert_eq!(p.observe(Now::Idle { unread: true }), None);
    // Marked seen: nothing new until it works again.
    assert_eq!(p.observe(Now::Idle { unread: false }), None);
    // A new run: unread output still needs attention.
    assert_eq!(Pane::default().observe(Now::Idle { unread: true }), Some(Fact::Idle));
    assert_eq!(p.observe(Now::Exit), Some(Fact::Exit));
    assert_eq!(p.observe(Now::Gone), Some(Fact::Gone));
    assert_eq!(p.observe(Now::Gone), None);
}

#[test]
fn held_wakes_make_one_line() {
    let w = |fact, name: &str, id: &str| Wake { fact, pane: id.into(), name: name.into(), id: id.into(), task: None, failed: false };
    assert_eq!(wake_line(&[w(Fact::Idle, "api", "abc12345xyz")]), "[watch] pane api abc12345 is idle. Read it, then mark it seen.");
    assert_eq!(
        wake_line(&[w(Fact::Idle, "api", "abc12345"), w(Fact::Gone, "", "def67890")]),
        "[watch] pane api abc12345 is idle; pane def67890 is gone. Read them, then mark each seen."
    );
    // A gone pane with open tasks: they are reassigned or marked done, not marked seen.
    let gone = |id: &str, t: &str| Wake { fact: Fact::Gone, pane: id.into(), name: String::new(), id: id.into(), task: Some(t.into()), failed: false };
    assert_eq!(
        wake_line(&[gone("4ac9e7a1", "d0e7b2f4")]),
        "[watch] pane 4ac9e7a1 is gone: task d0e7b2f4 is open. Reassign it or mark it done."
    );
    assert_eq!(
        wake_line(&[gone("4ac9e7a1", "d0e7b2f4, 5b1c9e2a")]),
        "[watch] pane 4ac9e7a1 is gone: tasks d0e7b2f4, 5b1c9e2a are open. Reassign each task or mark it done."
    );
    assert_eq!(
        wake_line(&[w(Fact::Idle, "api", "abc12345"), gone("4ac9e7a1", "d0e7b2f4")]),
        "[watch] pane api abc12345 is idle; pane 4ac9e7a1 is gone: task d0e7b2f4 is open. Read the others, then mark each seen. Reassign each task of a gone pane or mark it done."
    );
    let mut w = Watcher::default();
    let s = info("abc12345xyz", "idle", true);
    assert!(w.interrupted("3f9a01bc", &s).is_some());
    // Once per run, however long the row stays open.
    assert!(w.interrupted("3f9a01bc", &s).is_none());
    assert_eq!(wake_line(&w.held), "[watch] pane abc12345xyz abc12345 restarted: task 3f9a01bc is interrupted. Read it, then mark it seen.");
}

fn info(id: &str, state: &str, unread: bool) -> SessionInfo {
    serde_json::from_value(serde_json::json!({
        "id": id, "label": id, "role": "agent", "cwd": "/", "command": "claude", "args": [],
        "state": state, "cols": 80, "rows": 24, "pid": null, "exit_code": null, "unread": unread,
    }))
    .unwrap()
}

#[test]
fn deliveries_and_ledger_reads_set_and_clear_the_failure() {
    let mut w = Watcher::default();
    w.after_delivery(Delivery::Refused, "held");
    assert!(w.failure.is_none());
    w.after_delivery(Delivery::NoTurn, "undelivered: x");
    assert_eq!(w.failure.as_ref().map(|f| (f.kind.as_str(), f.message.as_str())), Some(("undelivered", "undelivered: x")));
    w.after_delivery(Delivery::Refused, "held");
    assert_eq!(w.failure.as_ref().map(|f| f.kind.as_str()), Some("undelivered"));
    w.after_delivery(Delivery::Taken, "woke");
    assert!(w.failure.is_none());

    w.after_ledger(Err("busy"));
    w.after_ledger(Err("busy"));
    assert!(w.failure.is_none());
    w.after_ledger(Err("busy"));
    assert_eq!(w.failure.as_ref().map(|f| (f.kind.as_str(), f.message.as_str())), Some(("ledger", "busy")));
    w.after_ledger(Ok(()));
    assert!(w.failure.is_none());
    // A good read starts the count again.
    w.after_ledger(Err("busy"));
    w.after_ledger(Err("busy"));
    assert!(w.failure.is_none());
}

#[test]
fn the_first_step_names_the_unreported_count_once() {
    let mut w = Watcher::default();
    w.count_unreported(4);
    assert_eq!(w.unreported.as_deref(), Some("[watch] 4 done tasks not reported: skiff ledger ls --unreported"));
    // Once per run: a later count, or a new connection, adds nothing.
    w.unreported = None;
    w.count_unreported(5);
    assert_eq!(w.unreported, None);
    let mut w = Watcher::default();
    w.count_unreported(0);
    assert!(w.counted && w.unreported.is_none());
    let mut w = Watcher::default();
    w.count_unreported(1);
    assert_eq!(w.unreported.as_deref(), Some("[watch] 1 done task not reported: skiff ledger ls --unreported"));
}

#[test]
fn a_fleet_change_holds_one_line_after_the_first_read() {
    let mut w = Watcher::default();
    assert!(!w.fleet_is("Harbor", vec!["api".into(), "web".into()]));
    assert!(!w.fleet_is("Harbor", vec!["api".into(), "web".into()]));
    assert!(w.lines.is_empty());
    assert!(w.fleet_is("Harbor", vec!["web".into(), "cli".into()]));
    assert_eq!(w.lines, ["Harbor changed: cli joined, api left. Read SKIPPER.md again."]);
}

#[test]
fn a_watch_line_goes_to_the_coordinator_that_owns_the_task() {
    let mut grand = info("aaaa1111", "idle", false);
    grand.coordinator = true;
    let mut gesher = info("bbbb2222", "working", false);
    gesher.skipper = Some("gesher".into());
    gesher.name = Some("gesher Skipper".into());
    let all = [grand, gesher, info("cccc3333", "idle", true), info("dddd4444", "idle", true)];
    let targets = targets(&all);
    assert_eq!(targets.iter().map(|(o, s)| (o.as_str(), s.id.as_str())).collect::<Vec<_>>(), [("grand", "aaaa1111"), ("gesher", "bbbb2222")]);
    // A grand task on one pane, gesher's task on another, and the
    // hand-off at work on the gesher Skipper's own pane.
    let mine = row(1, "u1", "cccc3333", "sent", None);
    let sub = ledger::Row { owner: "gesher".into(), ..row(2, "u2", "dddd4444", "sent", None) };
    let hand = ledger::Row { kind: "handoff".into(), project: "gesher".into(), ..row(3, "u3", "bbbb2222", "sent", None) };
    let mut ws = Watchers::default();
    let told = route(&mut ws, &targets, &[mine, sub, hand.clone()], &[], &[], &[], &all);
    let told: Vec<(&str, &str)> = told.iter().map(|(o, w)| (o.as_str(), w.id.as_str())).collect();
    assert_eq!(told, [("grand", "cccc3333"), ("gesher", "dddd4444")]);
    // Each watcher follows its own coordinator's task panes, and registers
    // them on its own coordinator. The Skipper's pane is no task pane.
    assert_eq!(ws["grand"].follows.as_deref(), Some(&["cccc3333".to_string()][..]));
    assert_eq!(ws["gesher"].follows.as_deref(), Some(&["dddd4444".to_string()][..]));
    assert_eq!(wake_line(&ws["gesher"].held), "[watch] pane dddd4444 dddd4444 is idle. Read it, then mark it seen.");

    // gesher reports the hand-off: the grand Skipper alone hears it, once.
    let done = ledger::Row { state: "done".into(), ..hand };
    assert!(ws.get_mut("grand").unwrap().handed(&[], &[]).is_empty());
    let told = route(&mut ws, &targets, &[], &[], std::slice::from_ref(&done), &[], &all);
    assert_eq!(told.iter().map(|(o, w)| (o.as_str(), w.fact)).collect::<Vec<_>>(), [("grand", Fact::Reported)]);
    let grand_held: Vec<Wake> = ws["grand"].held.iter().filter(|h| h.fact == Fact::Reported).cloned().collect();
    assert_eq!(
        wake_line(&grand_held),
        "[watch] the gesher Skipper reported hand-off u3. Read its result. Tell the person, then run skiff ledger report u3."
    );
    assert!(route(&mut ws, &targets, &[], &[], std::slice::from_ref(&done), &[], &all).is_empty());
}

#[test]
fn a_plan_gives_one_line_when_every_task_ended() {
    let mut grand = info("aaaa1111", "idle", false);
    grand.coordinator = true;
    let in_plan = |id, uid: &str, pane: &str| ledger::Row { parent: Some("p1".into()), ..row(id, uid, pane, "sent", None) };
    let tasks = [in_plan(1, "t1", "cccc3333"), in_plan(2, "t2", "dddd4444"), in_plan(3, "t3", "eeee5555")];
    let alone = row(4, "t4", "ffff6666", "sent", None);
    let open: Vec<ledger::Row> = tasks.iter().cloned().chain([alone]).collect();
    let mut ws = Watchers::default();
    // Two plan panes end their turns and the third works: their lines wait.
    // A task with no plan, and a waiting plan pane, are told at once.
    let all = [grand.clone(), info("cccc3333", "idle", true), info("dddd4444", "idle", true), info("eeee5555", "working", false), info("ffff6666", "idle", true)];
    route(&mut ws, &targets(&all), &open, &[], &[], &tasks, &all);
    assert_eq!(ws["grand"].ready_line().as_deref(), Some("[watch] pane ffff6666 ffff6666 is idle. Read it, then mark it seen."));
    ws.get_mut("grand").unwrap().delivered(Delivery::Taken, "");
    assert_eq!(ws["grand"].held.len(), 2);
    assert_eq!(ws["grand"].ready_line(), None);
    let all = [grand.clone(), info("cccc3333", "idle", true), info("dddd4444", "idle", true), info("eeee5555", "waiting", false)];
    let told = route(&mut ws, &targets(&all), &tasks, &[], &[], &tasks, &all);
    assert_eq!(told.iter().map(|(_, w)| w.fact).collect::<Vec<_>>(), [Fact::Waiting]);
    assert_eq!(ws["grand"].ready_line(), None);
    // The last one ends its turn: one line for the plan.
    let all = [grand, info("cccc3333", "idle", true), info("dddd4444", "idle", true), info("eeee5555", "idle", true)];
    route(&mut ws, &targets(&all), &tasks, &[], &[], &tasks, &all);
    assert_eq!(
        ws["grand"].ready_line().as_deref(),
        Some(
            "[watch] plan p1: 3 of 3 tasks ended: pane cccc3333 cccc3333, pane dddd4444 dddd4444, pane eeee5555 eeee5555. \
             Read the results, mark each pane seen and set each task done, then run skiff ledger report p1."
        )
    );
    ws.get_mut("grand").unwrap().delivered(Delivery::Taken, "");
    assert!(ws["grand"].held.is_empty());
}

#[test]
fn the_grand_skipper_never_hears_a_project_row() {
    let mut grand = info("aaaa1111", "idle", false);
    grand.coordinator = true;
    let mut gesher = info("7508753f0000", "working", false);
    gesher.skipper = Some("gesher".into());
    // gesher's task 9e0caa67 on pane 7902ac72, idle after a turn.
    let sub = ledger::Row { owner: "gesher".into(), ..row(357, "9e0caa67", "7902ac72405d", "sent", None) };
    let all = [grand.clone(), gesher, info("7902ac72405d", "idle", true)];
    let mut ws = Watchers::default();
    let told = route(&mut ws, &targets(&all), std::slice::from_ref(&sub), &[], &[], &[], &all);
    assert_eq!(told.iter().map(|(o, w)| (o.as_str(), w.id.as_str())).collect::<Vec<_>>(), [("gesher", "7902ac72405d")]);
    assert!(ws["grand"].held.is_empty());
    assert_eq!(ws["grand"].follows.as_deref(), Some(&[][..]));
    // With no gesher Skipper running, the row goes to no one.
    let all = [grand, info("7902ac72405d", "idle", true)];
    let mut ws = Watchers::default();
    assert!(route(&mut ws, &targets(&all), &[sub], &[], &[], &[], &all).is_empty());
    assert!(ws["grand"].held.is_empty());
}

#[cfg(unix)]
#[test]
fn a_new_file_at_the_path_is_a_new_build() {
    let dir = std::env::temp_dir().join(format!("skiff-watch-build-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("skiff");
    std::fs::write(&path, "old").unwrap();
    let b = Build { stamp: Build::stamp(&path).unwrap(), path: path.clone() };
    assert!(!b.replaced());
    // A build writes a new file, then links it in place of the old one.
    let new = dir.join("skiff.new");
    std::fs::write(&new, "new").unwrap();
    std::fs::rename(&new, &path).unwrap();
    // Too fresh: the build may still write it.
    assert!(!b.replaced());
    let old = std::time::SystemTime::now() - SETTLE * 2;
    File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
    assert!(b.replaced());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_the_agents_session_is_watched() {
    let mut agent = info("abc12345", "idle", false);
    agent.coordinator = true;
    let all = [agent, info("def67890", "idle", false)];
    assert_eq!(not_the_agent(&all, "abc12345", true), None);
    assert_eq!(not_the_agent(&all, "def67890", true).as_deref(), Some("skiff watch: def67890 is not a Skipper's session"));
    // An older daemon cannot tell, and an unknown session ends the watch as before.
    assert_eq!(not_the_agent(&all, "def67890", false), None);
    assert_eq!(not_the_agent(&all, "zzz", true), None);
}

#[test]
fn only_a_turn_end_counts_as_a_turn() {
    let mut w = Watcher::default();
    // A rename, a theme or a resize on an unread pane.
    w.note(&Event::SessionUpdated { session: info("a", "idle", true) });
    assert!(w.turned.is_empty());
    w.note(&Event::TurnEnded { session: "a".into() });
    assert!(w.turned.contains("a"));

    // Told idle once; metadata events after it wake nothing again.
    let mut w = Watcher::default();
    let s = info("a", "idle", true);
    assert_eq!(w.track("a", Some(&s), &[]).map(|k| k.fact), Some(Fact::Idle));
    w.held.clear();
    for _ in 0..3 {
        w.note(&Event::SessionUpdated { session: s.clone() });
        assert_eq!(w.track("a", Some(&s), &[]), None);
        w.turned.clear();
    }
    assert!(w.held.is_empty());
    // A real second turn is told.
    w.note(&Event::TurnEnded { session: "a".into() });
    assert_eq!(w.track("a", Some(&s), &[]).map(|k| k.fact), Some(Fact::Idle));
}

/// A turn the program said ended in an error is told as failed.
#[test]
fn a_turn_that_ended_in_an_error_is_told_failed() {
    let mut w = Watcher::default();
    let mut s = info("a", "idle", true);
    s.program_status = serde_json::from_value(serde_json::json!({"state": "error", "msg": "boom"})).unwrap();
    let wake = w.track("a", Some(&s), &[]).unwrap();
    assert_eq!((wake.fact, wake.failed), (Fact::Idle, true));
    assert_eq!(wake_line(&[wake]), "[watch] pane a a failed. Read it, then mark it seen.");
    let mut w = Watcher::default();
    s.program_status = serde_json::from_value(serde_json::json!({"state": "done"})).unwrap();
    assert!(!w.track("a", Some(&s), &[]).unwrap().failed);
}

#[test]
fn a_held_idle_line_goes_when_the_pane_works_or_waits() {
    for state in ["working", "waiting"] {
        let mut w = Watcher::default();
        w.track("a", Some(&info("a", "idle", true)), &[]);
        w.track("b", Some(&info("b", "idle", true)), &[]);
        assert_eq!(w.held.len(), 2);
        w.track("a", Some(&info("a", state, false)), &[]);
        assert_eq!(w.held.iter().map(|h| h.pane.as_str()).collect::<Vec<_>>(), ["b"], "{state}");
    }
    // An exit stays held while the pane is gone.
    let mut w = Watcher::default();
    w.track("a", None, &[]);
    w.track("a", None, &[]);
    assert_eq!(w.held.len(), 1);
}

#[test]
fn a_refused_line_is_held_and_one_with_no_turn_is_undelivered() {
    let mut w = Watcher::default();
    w.track("a", Some(&info("a", "idle", true)), &[]);
    let line = wake_line(&w.held);
    assert!(w.delivered(Delivery::Refused, &line).starts_with("held:"));
    assert_eq!(w.held.len(), 1);
    assert!(w.delivered(Delivery::NoTurn, &line).starts_with("undelivered:"));
    assert!(w.held.is_empty());
    w.track("b", Some(&info("b", "idle", true)), &[]);
    assert!(w.delivered(Delivery::Taken, &line).starts_with("woke the agent"));
    assert!(w.held.is_empty());
}

#[test]
fn a_task_skiff_closed_for_a_gone_pane_is_told_once_after_the_start() {
    let mut w = Watcher::default();
    let done = |uid: &str, pane: &str| row(1, uid, pane, "done", None);
    // The first read only notes them: the unreported count names them.
    assert!(w.closed(&[done("aaaa1111", "4ac9e7a1")]).is_empty());
    let wakes = w.closed(&[done("aaaa1111", "4ac9e7a1"), done("d0e7b2f4", "5b1c9e2a"), done("e5f6a7b8", "5b1c9e2a")]);
    assert_eq!(wakes.len(), 1);
    assert_eq!(wake_line(&w.held), "[watch] pane 5b1c9e2a is gone: tasks d0e7b2f4, e5f6a7b8 are done with it. Tell the person, then run skiff ledger report d0e7b2f4 e5f6a7b8.");
    // Told once.
    assert!(w.closed(&[done("d0e7b2f4", "5b1c9e2a"), done("e5f6a7b8", "5b1c9e2a")]).is_empty());
    // The line survives a step with no open task left on that pane.
    w.take(&[], &[]);
    assert_eq!(w.held.len(), 1);
    // Reported before the line went: it is dropped.
    w.closed(&[done("d0e7b2f4", "5b1c9e2a")]);
    assert!(w.held.is_empty());
    // With other lines, the others come first.
    let idle = Wake { fact: Fact::Idle, pane: "abc12345".into(), name: "api".into(), id: "abc12345".into(), task: None, failed: false };
    let closed = Wake { fact: Fact::Closed, pane: "gone:4ac9e7a1".into(), name: String::new(), id: "4ac9e7a1".into(), task: Some("d0e7b2f4".into()), failed: false };
    assert_eq!(
        wake_line(&[closed, idle]),
        "[watch] pane api abc12345 is idle; pane 4ac9e7a1 is gone: task d0e7b2f4 is done with it. Read it, then mark it seen. Tell the person, then run skiff ledger report d0e7b2f4."
    );
}

pub(crate) fn row(id: i64, uid: &str, pane: &str, state: &str, gen: Option<i64>) -> ledger::Row {
    ledger::Row {
        id,
        uid: uid.into(),
        kind: "task".into(),
        parent: None,
        pane_id: Some(pane.into()),
        pane_gen: gen,
        project: String::new(),
        task: "t".into(),
        sent_at: String::new(),
        state: state.into(),
        result_path: None,
        reference: None,
        updated_at: String::new(),
        task_type: None,
        socket: None,
        reported_at: None,
        title: None,
        closed_at: None,
        gone_at: None,
        owner: ledger::GRAND.into(),
        work_dir: None,
        flushed_at: None,
        bucket: None,
        agent_on: false,
        elsewhere: false,
        live: None,
        interrupted: None,
    }
}

fn born(mut s: SessionInfo, started_at: u64) -> SessionInfo {
    s.started_at = started_at;
    s
}

/// One step with these rows and sessions, as `step` runs it.
fn run(w: &mut Watcher, rows: &[ledger::Row], all: &[SessionInfo]) -> Vec<Fact> {
    let (groups, interrupted) = group(rows, all, &["skipper"]);
    w.take(&groups, &interrupted).iter().map(|k| k.fact).collect()
}

fn held(w: &Watcher) -> Vec<(Fact, Option<&str>)> {
    w.held.iter().map(|h| (h.fact, h.task.as_deref())).collect()
}

fn state(w: &mut Watcher, id: &str, states: &[SessionState]) {
    for &state in states {
        w.note(&Event::State { session: id.into(), state });
    }
}

#[test]
fn a_prompt_then_idle_is_told_once() {
    let seq = |nows: &[Now]| {
        let mut p = Pane::default();
        nows.iter().filter_map(|&n| p.observe(n)).collect::<Vec<_>>()
    };
    let seen = Now::Idle { unread: false };
    let unread = Now::Idle { unread: true };
    assert_eq!(seq(&[Now::Waiting, Now::Working, seen, seen]), [Fact::Waiting, Fact::Idle]);
    // The working step not seen.
    assert_eq!(seq(&[Now::Waiting, seen]), [Fact::Waiting, Fact::Idle]);
    // Each prompt notifies the person; one idle after the last.
    assert_eq!(
        seq(&[Now::Waiting, Now::Working, Now::Waiting, Now::Working, seen]),
        [Fact::Waiting, Fact::Waiting, Fact::Idle]
    );
    // A turn after the prompt: one idle, not one more for the prompt.
    assert_eq!(seq(&[Now::Waiting, Now::Working, unread, seen]), [Fact::Waiting, Fact::Idle]);
    assert_eq!(seq(&[Now::Waiting, Now::Exit, seen]), [Fact::Waiting, Fact::Exit]);
    assert_eq!(seq(&[Now::Waiting, Now::Gone]), [Fact::Waiting, Fact::Gone]);
    // A start-up screen is still no turn.
    assert_eq!(seq(&[Now::Working, seen, seen]), []);
}

#[test]
fn an_idle_pane_with_a_pending_task_wakes_once_per_idle_stretch() {
    let mut w = Watcher::default();
    let rows = [row(1, "7c2e91ab", "a", "pending", None)];
    assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), [Fact::Pending]);
    assert_eq!(wake_line(&w.held), "[watch] pane a a is idle: task 7c2e91ab is pending: skiff ledger send 7c2e91ab --enter. Read it, then mark it seen.");
    assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), []);
    assert_eq!(w.held.len(), 1);
    // It works: the held line goes. The next idle tells it again.
    assert_eq!(run(&mut w, &rows, &[info("a", "working", false)]), []);
    assert!(w.held.is_empty());
    assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), [Fact::Pending]);
    // Waiting drops it too.
    assert_eq!(run(&mut w, &rows, &[info("a", "waiting", false)]), [Fact::Waiting]);
    assert!(w.held.is_empty());
    // Joined with other parts as today.
    let b = info("b", "idle", true);
    assert_eq!(run(&mut w, &rows, &[info("a", "idle", false), b]), [Fact::Pending]);
    let all = [info("a", "idle", false), info("b", "idle", true)];
    let rows = [row(1, "7c2e91ab", "a", "pending", None), row(2, "9d01ffe2", "b", "sent", None)];
    assert_eq!(run(&mut w, &rows, &all), [Fact::Idle]);
    assert_eq!(
        wake_line(&w.held),
        "[watch] pane a a is idle: task 7c2e91ab is pending: skiff ledger send 7c2e91ab --enter; pane b b is idle. Read them, then mark each seen."
    );
}

#[test]
fn a_pending_task_on_an_unread_pane_is_one_part() {
    let mut w = Watcher::default();
    let rows = [row(1, "u1", "a", "pending", None)];
    w.note(&Event::TurnEnded { session: "a".into() });
    assert_eq!(run(&mut w, &rows, &[info("a", "idle", true)]), [Fact::Pending]);
    assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
    // A prompt answered with a pending row: one part for both.
    let mut w = Watcher::default();
    assert_eq!(run(&mut w, &rows, &[info("a", "waiting", false)]), [Fact::Waiting]);
    assert_eq!(run(&mut w, &rows, &[info("a", "working", false)]), []);
    assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), [Fact::Pending]);
    assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
    assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), []);
}

#[test]
fn a_late_turn_end_does_not_repeat_a_pending_line() {
    let rows = [row(1, "u1", "a", "pending", None)];
    let a = info("a", "idle", true);
    // Delivered, then the turn's end comes in.
    let mut w = Watcher::default();
    assert_eq!(run(&mut w, &rows, std::slice::from_ref(&a)), [Fact::Pending]);
    w.delivered(Delivery::Taken, "");
    w.note(&Event::TurnEnded { session: "a".into() });
    assert_eq!(run(&mut w, &rows, std::slice::from_ref(&a)), []);
    assert!(w.held.is_empty());
    // Still held: it stays one part.
    let mut w = Watcher::default();
    run(&mut w, &rows, std::slice::from_ref(&a));
    w.note(&Event::TurnEnded { session: "a".into() });
    assert_eq!(run(&mut w, &rows, std::slice::from_ref(&a)), []);
    assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
}

#[test]
fn a_new_pending_row_upgrades_a_held_idle_line() {
    let mut w = Watcher::default();
    let a = [info("a", "idle", true)];
    assert_eq!(run(&mut w, &[row(1, "s1", "a", "sent", None)], &a), [Fact::Idle]);
    // A row is added; the pane stays unread with no new state.
    let rows = [row(1, "s1", "a", "sent", None), row(2, "u1", "a", "pending", None)];
    assert_eq!(run(&mut w, &rows, &a), [Fact::Pending]);
    assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
    // The oldest uid changes: u1 removed, u2 added.
    let rows = [row(1, "s1", "a", "sent", None), row(3, "u2", "a", "pending", None)];
    assert_eq!(run(&mut w, &rows, &a), [Fact::Pending]);
    assert_eq!(held(&w), [(Fact::Pending, Some("u2"))]);
}

#[test]
fn the_next_pending_task_goes_once_the_told_one_leaves_pending() {
    let a = [info("a", "idle", false)];
    let both = [row(1, "u1", "a", "pending", None), row(2, "u2", "a", "pending", None)];
    // u1 removed while u2 stays pending: u2 at once.
    let mut w = Watcher::default();
    run(&mut w, &both, &a);
    w.delivered(Delivery::Taken, "");
    assert_eq!(run(&mut w, &both[1..], &a), [Fact::Pending]);
    assert_eq!(held(&w), [(Fact::Pending, Some("u2"))]);
    // u1 sent, then pending again with no state change: told again.
    let mut w = Watcher::default();
    let u1 = |state| [row(1, "u1", "a", state, None)];
    run(&mut w, &u1("pending"), &a);
    w.delivered(Delivery::Taken, "");
    assert_eq!(run(&mut w, &u1("sent"), &a), []);
    assert_eq!(run(&mut w, &u1("pending"), &a), [Fact::Pending]);
    // u1 cancelled (done or removed) while held: no line, no idle part.
    let mut w = Watcher::default();
    run(&mut w, &u1("pending"), &a);
    assert_eq!(run(&mut w, &[], &a), []);
    assert!(w.held.is_empty());
    // u1 sent before watch sees work: the held line goes.
    let mut w = Watcher::default();
    run(&mut w, &u1("pending"), &a);
    assert_eq!(run(&mut w, &u1("sent"), &a), []);
    assert!(w.held.is_empty());
}

#[test]
fn state_events_between_two_lists_count() {
    use SessionState::{Idle, Waiting, Working};
    let seen = [info("a", "idle", false)];
    // A prompt shown and answered between two seen-idle lists, no turn.
    let mut w = Watcher::default();
    let sent = [row(1, "s1", "a", "sent", None)];
    assert_eq!(run(&mut w, &sent, &seen), []);
    state(&mut w, "a", &[Waiting, Working, Idle]);
    assert_eq!(run(&mut w, &sent, &seen), [Fact::Idle]);
    assert_eq!(run(&mut w, &sent, &seen), []);
    // A told pending task is told again after unseen work.
    let mut w = Watcher::default();
    let rows = [row(1, "u1", "a", "pending", None)];
    assert_eq!(run(&mut w, &rows, &seen), [Fact::Pending]);
    w.delivered(Delivery::Taken, "");
    state(&mut w, "a", &[Working, Idle]);
    assert_eq!(run(&mut w, &rows, &seen), [Fact::Pending]);
    // A held one goes with the work, and comes back as one part.
    state(&mut w, "a", &[Waiting, Working, Idle]);
    assert_eq!(run(&mut w, &rows, &seen), [Fact::Pending]);
    assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
    // An idle event alone is no work.
    state(&mut w, "a", &[Idle]);
    assert_eq!(run(&mut w, &rows, &seen), []);
}

#[test]
fn a_row_changed_after_the_wake_is_dropped_before_the_line() {
    let mut w = Watcher::default();
    let all = [info("a", "idle", false), info("b", "idle", true)];
    let rows = [row(1, "u1", "a", "pending", None), row(2, "s1", "b", "sent", None)];
    run(&mut w, &rows, &all);
    assert_eq!(w.held.len(), 2);
    // The read after `ready`: u1 was sent meanwhile.
    let rows = [row(1, "u1", "a", "sent", None), row(2, "s1", "b", "sent", None)];
    w.drop_stale(&group(&rows, &all, &["skipper"]).0);
    assert_eq!(held(&w), [(Fact::Idle, None)]);
}

#[test]
fn a_new_generation_forgets_the_pane() {
    let rows = [row(1, "u1", "a", "pending", None)];
    let mut w = Watcher::default();
    assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 1)]), [Fact::Pending]);
    w.delivered(Delivery::Taken, "");
    // A reload keeps the generation: nothing new.
    assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 1)]), []);
    // A restore, seen only after a reconnect, already seen-idle.
    assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 2)]), [Fact::Pending]);
    // A sent row from the old generation is interrupted beside it.
    let mut w = Watcher::default();
    let rows = [row(1, "s1", "a", "sent", Some(1)), row(2, "u1", "a", "pending", None)];
    assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 2)]), [Fact::Interrupted, Fact::Pending]);
    assert_eq!(
        wake_line(&w.held),
        "[watch] pane a a restarted: task s1 is interrupted; pane a a is idle: task u1 is pending: skiff ledger send u1 --enter. Read them, then mark each seen."
    );
}

#[test]
fn rows_that_name_one_pane_make_one_group() {
    let mut w = Watcher::default();
    let all = [info("abc12345xyz", "idle", false)];
    // An imported prefix row and a full-id row, the newer one first in id order.
    let rows = [row(4, "u1", "abc12345", "pending", None), row(7, "u2", "abc12345xyz", "pending", None)];
    let (groups, _) = group(&rows, &all, &["skipper"]);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].pending, ["u1", "u2"]);
    assert_eq!(run(&mut w, &rows, &all), [Fact::Pending]);
    assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
    // The agent's own pane is skipped.
    let all = [info("skipper", "idle", false)];
    assert!(group(&[row(1, "u1", "skipper", "pending", None)], &all, &["skipper"]).0.is_empty());
}

#[test]
fn follows_names_live_panes_with_an_open_task() {
    let all = [info("b", "working", false), info("a", "idle", false), info("skipper", "idle", false)];
    // A gone pane and the agent's own pane are not followed. Each pane once.
    let rows = [
        row(1, "u1", "b", "sent", None),
        row(2, "u2", "a", "pending", None),
        row(3, "u3", "b", "blocked", None),
        row(4, "u4", "gone", "sent", None),
        row(5, "u5", "skipper", "pending", None),
    ];
    assert_eq!(follows(&group(&rows, &all, &["skipper"]).0), ["a", "b"]);
    assert!(follows(&group(&[], &all, &["skipper"]).0).is_empty());
}

#[test]
fn a_task_on_another_socket_is_never_gone_here() {
    let here = Path::new("/tmp/here.sock");
    let mut theirs = row(1, "u1", "a", "sent", None);
    theirs.socket = Some("/tmp/other.sock".into());
    let mut mine = row(2, "u2", "b", "sent", None);
    mine.socket = Some(here.display().to_string());
    let (kept, line) = on_socket(vec![theirs, mine], here);
    assert_eq!(kept.iter().map(|r| r.uid.as_str()).collect::<Vec<_>>(), ["u2"]);
    assert_eq!(line, "tasks u1 belong to /tmp/other.sock; not followed here");
    // Neither pane is on this daemon: only the task of this socket is told gone.
    let mut w = Watcher::default();
    assert_eq!(run(&mut w, &kept, &[]), [Fact::Gone]);
    assert!(!wake_line(&w.held).contains("u1"));
    // With only the other socket's task, nothing is told, and no reassign.
    let mut w = Watcher::default();
    assert!(run(&mut w, &on_socket(vec![row(1, "u1", "a", "sent", None)].into_iter().map(|mut r| { r.socket = Some("/tmp/other.sock".into()); r }).collect(), here).0, &[]).is_empty());
    assert!(w.held.is_empty());
}

#[test]
fn exit_and_gone_win_over_a_pending_task() {
    let rows = [row(1, "u1", "a", "pending", None)];
    let mut w = Watcher::default();
    assert_eq!(run(&mut w, &rows, &[info("a", "done", false)]), [Fact::Exit]);
    assert_eq!(wake_line(&w.held), "[watch] pane a a exited. Read it, then mark it seen.");
    let mut w = Watcher::default();
    run(&mut w, &rows, &[info("a", "idle", false)]);
    // Gone after a held pending line: the gone line replaces it.
    assert_eq!(run(&mut w, &rows, &[]), [Fact::Gone]);
    // It names the open task, which is reassigned or marked done, not marked seen.
    assert_eq!(held(&w), [(Fact::Gone, Some("u1"))]);
    assert_eq!(wake_line(&w.held), "[watch] pane a a is gone: task u1 is open. Reassign it or mark it done.");
}

#[test]
fn a_pending_line_is_held_when_refused_and_dropped_with_no_turn() {
    let rows = [row(1, "u1", "a", "pending", None)];
    let a = [info("a", "idle", false)];
    let mut w = Watcher::default();
    run(&mut w, &rows, &a);
    let line = wake_line(&w.held);
    w.delivered(Delivery::Refused, &line);
    assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
    // A failed ledger read skips the step: the next one keeps the line.
    assert_eq!(run(&mut w, &rows, &a), []);
    assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
    w.delivered(Delivery::NoTurn, &line);
    assert!(w.held.is_empty());
    // Not told again in this idle stretch.
    assert_eq!(run(&mut w, &rows, &a), []);
}

fn rows(lines: &[&str]) -> Vec<(String, String)> {
    lines.iter().map(|l| (l.to_string(), l.to_string())).collect()
}

#[test]
fn finds_an_empty_box_and_a_draft() {
    let rule = "─".repeat(40);
    assert_eq!(input_box(&rows(&["done.", &rule, "❯", &rule, "  ? for shortcuts"]), "claude"), Some(false));
    let mut tall = rows(&[&rule, "❯ x", &rule]);
    tall.extend(rows(&[""; 30]));
    assert_eq!(input_box(&tall, "claude"), Some(true));
    assert_eq!(input_box(&rows(&["done.", &rule, "❯ fix the", "  tests", &rule]), "claude"), Some(true));
    // The dim hint is not a draft.
    let hint = vec![(rule.clone(), rule.clone()), ("❯ Try \"fix lint\"".into(), "❯".into()), (rule.clone(), rule.clone())];
    assert_eq!(input_box(&hint, "claude"), Some(false));
    assert_eq!(input_box(&rows(&["$ ls", "a b"]), "claude"), None);
    assert!(busy_line(&rows(&["* Working (2s · esc to interrupt)", &rule, "❯", &rule])));
    assert!(!busy_line(&rows(&["done.", &rule, "❯", &rule])));
    assert_eq!(input_box(&rows(&["text", "›", "", "  ? for shortcuts"]), "codex"), Some(false));
    assert_eq!(input_box(&rows(&["text", "› hello", ""]), "codex"), Some(true));
    assert_eq!(front(&rows(&["done.", &rule, "❯", &rule]), "claude"), Front::Ready);
    assert_eq!(front(&rows(&["done.", &rule, "❯ fix the", &rule]), "claude"), Front::Draft);
    // The busy line wins over an empty box.
    assert_eq!(front(&rows(&["* Working (2s · esc to interrupt)", &rule, "❯", &rule]), "claude"), Front::Busy);
    assert_eq!(front(&rows(&["$ ls", "a b"]), "claude"), Front::NoBox);
}
