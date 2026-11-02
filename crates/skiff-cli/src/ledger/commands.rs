//! One `skiff ledger` command, matched and sequenced over the read and write sides.

use super::*;

pub(super) async fn run_one(socket: &Path, cmd: LedgerCmd, person: bool) -> Result<i32> {
    match cmd {
        LedgerCmd::Add { pane: p, task, title, plan: is_plan, parent, project, state: st, result, reference, task_type, send, enter, shell } => {
            let task_type = task_type.as_deref().map(skiff_core::tasks::check_name).transpose()?.map(str::to_string);
            let st = add_state(st.as_deref(), is_plan, send)?.to_string();
            let target = match p {
                Some(p) if !is_plan => Some(pane(socket, &p).await?),
                _ => None,
            };
            let (pane_id, pane_gen, project) = match &target {
                Some(s) => (Some(s.id.clone()), gen(s), project.unwrap_or_else(|| project_of(s))),
                None => (None, None, project.unwrap_or_default()),
            };
            let file = path()?;
            let here = pane_id.as_ref().map(|_| socket.display().to_string());
            let mut db = open_at(&file)?;
            let caller = check_writer(socket, &db, person).await?;
            if let Some(s) = &target {
                check_scope(&caller, s)?;
            }
            let owner = match parent.as_deref() {
                Some(p) => caller.check_parent(&container(&db, p)?)?,
                None => caller.owner().to_string(),
            };
            let new = New { plan: is_plan, parent, pane_id, pane_gen, project, task, title, state: st, result, reference, task_type, socket: here, owner, handoff: false };
            let (id, uid, result, plan) = insert(&mut db, &file, new)?;
            let dir = one(&db, &uid)?.work_dir.unwrap_or_default();
            println!("id {id}\nuid {uid}\nfolder {dir}\nresult {result}");
            if let Some(p) = plan {
                // A task under a hand-off names the hand-off.
                println!("{} {p}", one(&db, &p).map(|r| r.kind).unwrap_or_else(|_| "plan".into()));
            }
            if send {
                let row = one(&db, &uid)?;
                return send_row(socket, &db, &row, SendOpts { text: Vec::new(), enter, shell }).await;
            }
        }
        LedgerCmd::Handoff { project, task, title, parent } => {
            let file = path()?;
            let mut db = open_at(&file)?;
            let caller = check_writer(socket, &db, person).await?;
            if matches!(caller, Caller::Project(_)) {
                bail!("a project or fleet Skipper does not hand off. Add your own tasks with skiff ledger add. Nothing changed");
            }
            let cfg = skiff_core::config::load()?;
            let target = scope(&cfg, &project)?;
            if let Some(p) = parent.as_deref() {
                let c = container(&db, p)?;
                if c.is_handoff() {
                    bail!("{p} is in hand-off {}: a hand-off goes under a plan of Admiral's. Nothing changed", c.uid);
                }
                caller.check_parent(&c)?;
            }
            let c = crate::connect(socket).await?;
            let (ps, key, name) = match target {
                Scope::Project(p) => {
                    // One Skipper per project: its fleet's Skipper takes the work.
                    if let Some(f) = skiff_client::skipper::commanding_fleet(&cfg, &c.list_sessions().await?, &p) {
                        bail!("{p} is in {f}, which has a fleet Skipper. Use: skiff ledger handoff {f}. Nothing changed");
                    }
                    (crate::skipper::start_project(socket, &c, &p).await?, p.clone(), p)
                }
                Scope::Fleet(f) => {
                    let fs = crate::skipper::start_fleet(socket, &c, &f, crate::skipper::Over::Refuse).await?;
                    (fs.skipper, skiff_client::skipper::fleet_scope(&f), f)
                }
            };
            let s = &ps.session;
            let new = New {
                plan: false,
                parent,
                pane_id: Some(s.id.clone()),
                pane_gen: gen(s),
                project: key,
                task,
                title,
                state: "pending".into(),
                result: None,
                reference: None,
                task_type: None,
                socket: Some(socket.display().to_string()),
                owner: GRAND.into(),
                handoff: true,
            };
            let (id, uid, result, plan) = insert(&mut db, &file, new)?;
            let dir = one(&db, &uid)?.work_dir.unwrap_or_default();
            println!("id {id}\nuid {uid}\nfolder {dir}\nresult {result}");
            if let Some(p) = plan {
                // A task under a hand-off names the hand-off.
                println!("{} {p}", one(&db, &p).map(|r| r.kind).unwrap_or_else(|_| "plan".into()));
            }
            let row = one(&db, &uid)?;
            let mut text = message(&row);
            if ps.created {
                // A new agent takes its instructions first, then the task, in one turn.
                text = format!("{} Then take this hand-off. {text}", skiff_client::skipper::first_message(&skiff_core::config::load()?, &ps.dir)?);
                crate::skipper::ready(&c, &s.id).await?;
                println!("started the {name} Skipper");
            }
            return send_row(socket, &db, &row, SendOpts { text: vec![text], enter: true, shell: false }).await;
        }
        LedgerCmd::Send { id, text, enter, shell } => {
            let db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let row = one(&db, &id)?;
            caller.check(&row)?;
            return send_row(socket, &db, &row, SendOpts { text, enter, shell }).await;
        }
        LedgerCmd::Update { id, state: st, result, task, title, pane: p, project, parent, reference, task_type } => {
            let st = st.as_deref().map(state).transpose()?;
            let task_type = task_type.as_deref().map(skiff_core::tasks::check_name).transpose()?;
            let file = path()?;
            let mut db = open_at(&file)?;
            let caller = check_writer(socket, &db, person).await?;
            let row = one(&db, &id)?;
            // A project Skipper reports the hand-off it was given: its state and result.
            let report_only = task.is_none() && title.is_none() && p.is_none() && project.is_none() && parent.is_none() && reference.is_none() && task_type.is_none();
            if report_only {
                caller.check_report(&row)?;
            } else {
                caller.check(&row)?;
            }
            if let Some(p) = parent.as_deref() {
                if caller.check_parent(&container(&db, p)?)? != row.owner {
                    bail!("{} is {}'s {}, and {p} is not. Move it under a plan of {}", row.uid, who(&row.owner), kind_word(&row.kind), who(&row.owner));
                }
            }
            if row.is_plan() && (p.is_some() || parent.is_some()) {
                bail!("ledger row {id} is a plan: it has no pane and no plan above it");
            }
            // A new pane, or a new send to the same one, starts from the
            // pane's program as it is now.
            let (pane_id, gen) = match (&p, st) {
                (Some(p), _) => {
                    let s = pane(socket, p).await?;
                    check_scope(&caller, &s)?;
                    (Some(s.id.clone()), Some(gen(&s)))
                }
                (None, Some("sent")) if !row.is_plan() && row.socket_other_than(socket).is_none() => {
                    let all = live(socket).await.unwrap_or_default();
                    let s = row.pane_id.as_deref().and_then(|p| resolve::session(&all, p).ok());
                    (None, s.map(gen))
                }
                _ => (None, None),
            };
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if parent.as_deref().is_some_and(|p| one(&tx, p).is_ok_and(|p| p.id == row.id)) {
                bail!("task {} cannot join its own work. Nothing changed", row.uid);
            }
            let parent = parent.as_deref().map(|p| join(&tx, &file, p)).transpose()?.map(|p| p.uid);
            // A new pane is on this socket.
            let here = pane_id.as_ref().map(|_| socket.display().to_string());
            tx.execute(
                &format!(
                    "UPDATE tasks SET state = coalesce(?2, state), result_path = coalesce(?3, result_path), task = coalesce(?4, task),
                     pane_id = coalesce(?5, pane_id), project = coalesce(?6, project), parent = coalesce(?7, parent), ref = coalesce(?8, ref),
                     pane_gen = CASE WHEN ?9 THEN ?10 ELSE pane_gen END, type = coalesce(?11, type), socket = coalesce(?12, socket), title = coalesce(?13, title),
                     closed_at = CASE WHEN ?7 IS NULL THEN closed_at END, gone_at = CASE WHEN ?2 IS NULL AND ?5 IS NULL THEN gone_at END,
                     updated_at = {NOW} WHERE id = ?1"
                ),
                params![row.id, st, result, task, pane_id, project, parent, reference, gen.is_some(), gen.flatten(), task_type, here, title],
            )?;
            restate(&tx, &one(&tx, &row.uid)?, st)?;
            tx.commit()?;
        }
        LedgerCmd::Ls { state: st, all, closed, unreported, json } => {
            if let Some(s) = st.as_deref().filter(|s| *s != INTERRUPTED && *s != GONE) {
                state(s)?;
            }
            let view = if all {
                View::All
            } else if closed {
                View::Closed
            } else if unreported {
                View::Unreported
            } else {
                View::Open
            };
            let mut db = open()?;
            let all = live(socket).await;
            sweep(&mut db, socket, all.as_deref())?;
            let mut list = rows(&db, "", None)?;
            mark_live(socket, all.as_deref(), &mut list);
            roll_up(&mut list);
            keep(&mut list, view);
            if let Some(s) = &st {
                list.retain(|r| r.shown_state() == s);
            }
            let mut list = tree(list);
            // Stable: each work keeps its tasks under it.
            list.sort_by_key(|(r, _)| rank(r.bucket));
            if json {
                print::json(&list.iter().map(|(r, _)| r).collect::<Vec<_>>())?;
            } else {
                print_rows(&list, matches!(view, View::Closed | View::All));
            }
        }
        LedgerCmd::Report { ids, all } => {
            let mut db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let n = report(&mut db, &caller, &ids, all)?;
            println!("reported {n} {}", rows_word(n));
        }
        LedgerCmd::Close { ids } => {
            let mut db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let n = close(&mut db, &caller, &ids)?;
            println!("closed {n} {}", work_word(n));
        }
        LedgerCmd::Reopen { ids } => {
            let mut db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let n = reopen(&mut db, &caller, &ids)?;
            println!("reopened {n} {}", work_word(n));
        }
        LedgerCmd::Prune { older_than, yes, flush } => {
            let secs = age(&older_than)?;
            let mut db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let sessions = live(socket).await;
            // The rows first, so the rule still sees them while it judges their folders.
            let mut gone = prune(&mut db, &caller, secs, false)?;
            if gone.is_empty() {
                println!("no work was closed more than {older_than} ago. Nothing changed");
                return Ok(0);
            }
            let dirs: Vec<PathBuf> = gone.iter().filter_map(|r| r.work_dir.as_deref().map(PathBuf::from)).collect();
            if yes && flush {
                flush_folders(socket, sessions.as_deref(), &dirs)?;
            }
            let gone_now = if yes { prune(&mut db, &caller, secs, true)? } else { Vec::new() };
            let n = gone.len();
            mark_live(socket, sessions.as_deref(), &mut gone);
            print_rows(&tree(gone), true);
            if yes {
                let n = gone_now.len();
                println!("removed {n} {}. {}", rows_word(n), if flush { "Files outside the work folders stay" } else { "The work folders and result files stay" });
            } else {
                let (files, bytes) = dirs.iter().map(|d| folder_size(d)).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
                println!(
                    "would remove {n} {}. Nothing changed. Their work folders hold {files} files ({}). Run again with --yes to remove the rows, and add --flush to flush their folders",
                    rows_word(n),
                    skiff_client::agent_folder::size(bytes)
                );
            }
        }
        LedgerCmd::Show { id, json } => {
            let mut db = open()?;
            let all = live(socket).await;
            sweep(&mut db, socket, all.as_deref())?;
            let r = one(&db, &id)?;
            // The whole work: a plan's state and the bucket come from its tasks.
            let h = top(&db, &r)?;
            let mut work = under(&db, &h)?;
            work.push(h.clone());
            mark_live(socket, all.as_deref(), &mut work);
            roll_up(&mut work);
            let closed = h.closed_at.clone();
            let r = work.into_iter().find(|w| w.uid == r.uid).context("the row left the ledger")?;
            if json {
                print::json(&r)?;
            } else {
                print_one(&r, closed.as_deref());
            }
        }
        LedgerCmd::Dir { id } => {
            let r = one(&open()?, &id)?;
            match (&r.work_dir, &r.result_path) {
                (Some(d), _) => println!("{d}"),
                (None, Some(p)) => {
                    eprintln!("skiff: row {} is from before work folders and has none. Its result: {p}", r.uid);
                    return Ok(1);
                }
                (None, None) => {
                    eprintln!("skiff: row {} is from before work folders and has none", r.uid);
                    return Ok(1);
                }
            }
        }
        LedgerCmd::Rm { id, flush } => {
            let db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let row = one(&db, &id)?;
            caller.check(&row)?;
            if row.is_plan() || row.is_handoff() {
                let n: i64 = db.query_row("SELECT count(*) FROM tasks WHERE parent = ?1", [&row.uid], |r| r.get(0))?;
                if n > 0 {
                    bail!("{} {id} still has {n} tasks. Remove them first", row.kind);
                }
            }
            if flush {
                if let Some(d) = &row.work_dir {
                    flush_folders(socket, live(socket).await.as_deref(), &[PathBuf::from(d)])?;
                }
            }
            // The row. Without --flush, its folder and result file stay where they are.
            db.execute("DELETE FROM tasks WHERE id = ?1", [row.id])?;
            match (&row.work_dir, flush) {
                (Some(d), false) if Path::new(d).exists() => println!("removed {} {}. Its work folder stays: {d}", kind_word(&row.kind), row.uid),
                _ => println!("removed {} {}", kind_word(&row.kind), row.uid),
            }
        }
    }
    Ok(0)
}
