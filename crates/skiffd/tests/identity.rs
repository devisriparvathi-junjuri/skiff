//! A private daemon reports the Host's identity in `pong`: the durable id from
//! its private state, and an instance id new on every start and every reload.
//! The selected local adapter adopts it and validates it on reconnect.
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use skiff_client::{
    host::{local::{IdentitySource, HostSelection}, Continuity},
    Client,
};
use skiff_core::protocol::Identity;

mod common;

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let stopped = common::stop(&mut self.child, &self.dir);
        if std::thread::panicking() {
            eprintln!("{}", std::fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default());
        }
        if stopped {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn spawn(dir: &Path) -> Child {
    let log = std::fs::File::options().create(true).append(true).open(dir.join("daemon.log")).unwrap();
    Command::new(env!("CARGO_BIN_EXE_skiffd"))
        .env("SKIFF_SOCKET", dir.join("skiffd.sock"))
        .env("SKIFF_STATE", dir.join("workspace.json"))
        .env("SKIFF_CONFIG", dir.join("config/projects.toml"))
        .env("SKIFF_LEDGER", dir.join("ledger.db"))
        .env("SKIFF_SHELL_ENV", "0")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap()
}

async fn identity(socket: &Path) -> (Identity, Option<u32>) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(c) = Client::connect(socket).await {
            if let Ok(info) = c.daemon_info().await {
                return (info.identity.expect("skiffd reports its identity"), info.pid);
            }
        }
        assert!(Instant::now() < until, "skiffd did not answer");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn identity_is_durable_and_each_start_and_reload_is_a_new_instance() {
    let dir = std::env::temp_dir().join(format!("skiff-identity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("skiffd.sock");
    let file = dir.join("config/local-host-id");
    let mut daemon = Daemon { child: spawn(&dir), dir: dir.clone() };

    let (first, pid) = identity(&socket).await;
    assert_eq!(pid, Some(daemon.child.id()));
    // The id is the Host's own private store; never the pid or the socket.
    assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), first.host_id);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(first.account, format!("uid:{}", unsafe { libc::geteuid() }));
    assert_eq!(first.os, std::env::consts::OS);
    assert_ne!(first.instance_id, first.host_id);
    // The same run answers every connection alike.
    assert_eq!(identity(&socket).await.0, first);

    // The selected adapter adopts it, and a reconnect to the same run is SameInstance.
    let selection = HostSelection::new(socket.clone(), dir.join("app-bootstrap-id"));
    let activate = || async {
        let mut pending = selection.connect().await.unwrap();
        pending.daemon_info().await.unwrap();
        pending.activate().await.unwrap()
    };
    let host = activate().await;
    assert_eq!(host.identity_source(), IdentitySource::Host);
    assert_eq!(host.connection().hello().host.host_id.as_str(), first.host_id);
    assert_eq!(host.connection().hello().instance_id.as_str(), first.instance_id);
    drop(host);
    assert_eq!(activate().await.connection().continuity(), Continuity::SameInstance);
    assert!(!dir.join("app-bootstrap-id").exists(), "the app's bootstrap store is not consulted");

    // A reload keeps the pid and the Host, and is a new instance.
    Client::connect(&socket).await.unwrap().reload(None).await.unwrap();
    let (reloaded, pid) = identity(&socket).await;
    assert_eq!(pid, Some(daemon.child.id()));
    assert_eq!(reloaded.host_id, first.host_id);
    assert_ne!(reloaded.instance_id, first.instance_id);
    assert_eq!(activate().await.connection().continuity(), Continuity::NewInstance);

    // A restart: same Host, another instance.
    assert!(common::stop(&mut daemon.child, &dir));
    daemon.child = spawn(&dir);
    let (restarted, _) = identity(&socket).await;
    assert_eq!(restarted.host_id, first.host_id);
    assert!(restarted.instance_id != first.instance_id && restarted.instance_id != reloaded.instance_id);
    assert_eq!(activate().await.connection().continuity(), Continuity::NewInstance);

    // Another Host's store behind the same endpoint is refused on reconnect.
    assert!(common::stop(&mut daemon.child, &dir));
    std::fs::remove_file(&file).unwrap();
    daemon.child = spawn(&dir);
    let (other, _) = identity(&socket).await;
    assert_ne!(other.host_id, first.host_id);
    let mut pending = selection.connect().await.unwrap();
    pending.daemon_info().await.unwrap();
    let refused = pending.activate().await.err().unwrap();
    assert!(matches!(
        refused.downcast_ref::<skiff_host_api::Error>(),
        Some(skiff_host_api::Error::IdentityChanged { .. })
    ));
}
