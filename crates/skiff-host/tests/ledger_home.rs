//! M6: a result outside the ledger folder shows only on This Mac, and only
//! under the person's home. A service reached over SSH keeps the ledger
//! folder. Its own test binary: HOME and SKIFF_LEDGER are process-wide.
#![cfg(unix)]

use std::path::PathBuf;

use skiff_client::host::{data, local::HostSelection};
use skiff_host_api::Error;

mod common;
use common::{add, private_home, start, Guard, SCHEMA};

#[test]
fn a_result_under_home_shows_on_this_mac_only() {
    let dir = PathBuf::from("/tmp").join(format!("shlh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let _guard = Guard(dir.clone());
    let home = private_home(&dir);
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(run(dir, home));
}

async fn run(dir: PathBuf, home: PathBuf) {
    let socket = dir.join("skiffd.sock");
    skiff_core::state::set_socket(socket.clone());
    let agent = dir.join("agent");
    std::fs::create_dir_all(agent.join("results")).unwrap();
    std::fs::create_dir_all(home.join("repo")).unwrap();
    std::fs::write(agent.join("results/in.md"), "# Ledger\n").unwrap();
    std::fs::write(home.join("repo/out.md"), "# Repo\n").unwrap();
    std::fs::write(dir.join("elsewhere.md"), "# Elsewhere\n").unwrap();
    std::os::unix::fs::symlink(dir.join("elsewhere.md"), home.join("repo/link.md")).unwrap();
    let ledger = agent.join("ledger.db");
    let db = rusqlite::Connection::open(&ledger).unwrap();
    db.execute_batch(SCHEMA).unwrap();
    let at = |p: PathBuf| p.to_string_lossy().into_owned();
    for (uid, p) in [
        ("1111aaaa", agent.join("results/in.md")),
        ("2222bbbb", home.join("repo/out.md")),
        ("3333cccc", dir.join("elsewhere.md")),
        ("4444dddd", home.join("repo/link.md")),
        ("5555eeee", home.join("repo/../../elsewhere.md")),
    ] {
        add(&db, uid, "task", None, "done", Some(&at(p)), None, None);
    }
    drop(db);
    std::env::set_var("SKIFF_LEDGER", &ledger);

    let hello = skiff_host::hello().unwrap();
    let host = hello.host.host_id.clone();
    let _service = start(&skiff_core::socket::data_socket(&socket), hello);
    let selection = HostSelection::new(socket.clone(), dir.join("bootstrap-id"));
    let plane = selection.connect_data(&host).await.unwrap();
    let c = plane.connection();

    for this_mac in [false, true] {
        skiff_host::set_this_mac(this_mac);
        let rows = data::ledger_rows(&c).await.unwrap();
        let exists = |uid: &str| rows.iter().find(|r| r.uid == uid).unwrap().result_exists;
        assert!(exists("1111aaaa"), "{this_mac}: the ledger folder");
        assert_eq!(exists("2222bbbb"), this_mac, "{this_mac}: under home");
        assert_eq!(data::ledger_result(&c, "1111aaaa").await.unwrap().text.as_deref(), Some("# Ledger\n"));
        let got = data::ledger_result(&c, "2222bbbb").await;
        if this_mac {
            assert_eq!(got.unwrap().text.as_deref(), Some("# Repo\n"));
        } else {
            assert!(matches!(got, Err(Error::NotFound { .. })), "{got:?}");
        }
        // Never outside home, through a link or by `..`, on either.
        for uid in ["3333cccc", "4444dddd", "5555eeee"] {
            assert!(!exists(uid), "{this_mac}: {uid}");
            assert!(matches!(data::ledger_result(&c, uid).await, Err(Error::NotFound { .. })), "{this_mac}: {uid}");
        }
    }
    skiff_host::set_this_mac(false);
}
