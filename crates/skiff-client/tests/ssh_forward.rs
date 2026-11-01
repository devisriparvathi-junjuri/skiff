//! The M2 review's fixtures, kept as regressions: a fake `ssh` (a Python
//! Unix-socket server, never OpenSSH, never a network connection) that binds
//! only the `-L` path it is given and answers `ping` with the Host id in a
//! private file. Skips when this machine has no python3.

use std::path::{Path, PathBuf};

use serde_json::json;
use skiff_client::host::{
    local::{
        legacy_agents, legacy_create, legacy_delete_group, legacy_groups, legacy_kill,
        legacy_rename, legacy_save_group, HostSelection, SelectedHost,
    },
    ssh::{SshForward, SshOptions, SshTarget},
};
use skiff_host_api::{Capability, Error, HostId};

struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn new(name: &str) -> Option<Self> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            eprintln!("skipped: no /usr/bin/python3");
            return None;
        }
        // Short: the forward's socket path must fit in sun_path.
        let dir = PathBuf::from("/tmp").join(format!("sf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The fake skiffd: `ping` answers with the Host id and protocol in
        // private files. Every other request is logged first; the session
        // and group requests answer as skiffd does, and `create_session`
        // drops the connection unanswered while `drop-create` exists.
        let script = r#"#!/usr/bin/python3
import socket,sys,json,threading,os
D=sys.argv[0].rsplit('/',1)[0]
spec=sys.argv[sys.argv.index('-L')+1]
s=socket.socket(socket.AF_UNIX); s.bind(spec.split(':')[0]); s.listen()
lock=threading.Lock()
def row(i,cwd,name):
 return {'id':i,'label':'sh','role':'shell','cwd':cwd,'command':'/bin/sh','args':[],'state':'idle','cols':80,'rows':24,'pid':7,'exit_code':None,'theme':None,'name':name}
def serve(c):
 with c, c.makefile('rb') as f:
  for line in f:
   req=json.loads(line); cmd=req.get('cmd'); data=None
   if cmd=='ping':
    hid=open(D+'/remote-id').read()
    proto=int(open(D+'/protocol').read()) if os.path.exists(D+'/protocol') else 27
    data={'type':'pong','version':'1.6.6','protocol':proto,'identity':{'host_id':hid,'instance_id':'run','os':'fixture','account':'fixture'}}
   else:
    with lock: open(D+'/requests','a').write(line.decode())
    if cmd=='create_session':
     if os.path.exists(D+'/drop-create'): return
     data={'type':'session','session':row('s1',req['spec']['cwd'] or '/home/fixture',req['spec']['name'])}
    elif cmd=='rename_session': data={'type':'session','session':row(req['session'],'/srv',req['name'])}
    elif cmd in ('kill','delete_group','seen'): data={'type':'ok'}
    elif cmd=='list_agents': data={'type':'agents','agents':[{'id':'claude','command':'claude --host-side','default_command':'claude','custom':False,'installed':True,'enabled':True}]}
    elif cmd=='list_groups': data={'type':'groups','groups':[]}
    elif cmd=='save_group': g=req['group']; g['id']='g1'; data={'type':'group','group':g}
    else: data={'type':'error','message':'unknown'}
   data['id']=req['id']
   c.sendall((json.dumps(data)+'\n').encode())
while True:
 c,_=s.accept();threading.Thread(target=serve,args=(c,),daemon=True).start()
"#;
        let ssh = dir.join("fake-ssh");
        std::fs::write(&ssh, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Another test's fork can hold the new file open for a moment, and
        // exec then fails with ETXTBSY. Once one run of it starts, no fork
        // holds it any more: this one, with no `-L`, stops at once.
        for _ in 0..200 {
            match std::process::Command::new(&ssh).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status() {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => std::thread::sleep(std::time::Duration::from_millis(10)),
                _ => break,
            }
        }
        Some(Self { dir })
    }

    fn report(&self, host_id: &str) {
        std::fs::write(self.dir.join("remote-id"), host_id).unwrap();
    }

    /// The protocol the fake skiffd reports in its pong.
    fn protocol(&self, protocol: u32) {
        std::fs::write(self.dir.join("protocol"), protocol.to_string()).unwrap();
    }

    /// Every non-ping frame the fake skiffd received, in order, without ids.
    fn requests(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.join("requests"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
                v.as_object_mut().unwrap().remove("id");
                v
            })
            .collect()
    }

    fn options(&self) -> SshOptions {
        SshOptions {
            program: self.dir.join("fake-ssh"),
            config: None,
        }
    }

    fn target(&self) -> SshTarget {
        SshTarget::new("fixture", "/fixture.sock")
    }

    fn selection(&self, pin: Option<&str>) -> HostSelection {
        let pin = pin.map(|p| HostId::try_from(p.to_string()).unwrap());
        HostSelection::ssh(self.target(), self.options(), self.dir.join("fwd"), pin).unwrap()
    }
}

async fn activate(selection: &HostSelection) -> anyhow::Result<()> {
    let mut c = selection.connect().await?;
    c.daemon_info().await?;
    c.activate().await.map(drop)
}

fn changed(r: anyhow::Result<()>) -> bool {
    matches!(
        r.err().as_ref().and_then(|e| e.downcast_ref()),
        Some(Error::IdentityChanged { .. })
    )
}

/// Review P1: accept A; the same endpoint then reports B. A reconnect refused
/// B, and so must a selection rebuilt with the stored pin, as after picking
/// another Host and back, or a restart.
#[tokio::test]
async fn a_changed_identity_stays_refused_after_reselection() {
    let Some(f) = Fixture::new("pin") else { return };
    let first = f.selection(None);
    f.report("A");
    activate(&first).await.unwrap();
    // Activation pins nothing: the desktop pins only what it stored in
    // hosts.toml, so a failed store leaves nothing to skip it on a retry.
    assert_eq!(first.pinned(), None, "activation pinned before the store");
    let pin = HostId::try_from("A".to_string()).unwrap();
    first.repin(pin.clone());
    f.report("B");
    assert!(changed(activate(&first).await), "same selection accepted B");
    drop(first);
    let reselected = f.selection(Some(pin.as_str()));
    assert!(
        changed(activate(&reselected).await),
        "reselected endpoint accepted B"
    );
    drop(reselected);
    // Only an accepted new identity (the person's step) connects to B.
    let accepted = f.selection(Some("B"));
    activate(&accepted).await.unwrap();
    // The store held another id than the one first accepted here (another
    // window won): after a repin this selection refuses what it accepted.
    accepted.repin(HostId::try_from("A".to_string()).unwrap());
    assert!(changed(activate(&accepted).await));
}

/// Review P2: two windows or app runs open the same destination. Each forward
/// has its own socket, and dropping one leaves the other's in place.
#[tokio::test]
async fn two_forwards_to_one_destination_never_share_a_socket() {
    let Some(f) = Fixture::new("own") else { return };
    let parent = f.dir.join("fwd");
    let first = SshForward::open(&f.target(), &f.options(), &parent)
        .await
        .unwrap();
    let second = SshForward::open(&f.target(), &f.options(), &parent)
        .await
        .unwrap();
    assert_ne!(first.socket(), second.socket());
    let kept = second.socket().to_path_buf();
    drop(first);
    assert!(
        kept.exists(),
        "dropping the first forward removed the second's socket"
    );
    drop(second);
    assert!(!kept.exists() && !kept.parent().unwrap().exists());
    // The shared parent stays for other owners.
    assert!(parent.is_dir());
}

async fn host(selection: &HostSelection) -> SelectedHost {
    let mut c = selection.connect().await.unwrap();
    c.daemon_info().await.unwrap();
    c.activate().await.unwrap()
}

fn spec(cwd: Option<&str>, peek: bool) -> skiff_core::session::SessionSpec {
    serde_json::from_value(json!({"label":"claude","name":"Fix it","role":"agent","cwd":cwd,
        "command":"claude","args":["--host-side"],"peek":peek,"cols":100,"rows":30}))
    .unwrap()
}

/// M3: a remote Host whose skiffd speaks this app's protocol gets lifecycle,
/// groups and its own agent list, over the very same legacy frames. The cwd
/// is the Host's path, sent as typed; the agent command line is the Host's.
#[tokio::test]
async fn a_compatible_remote_host_serves_lifecycle_groups_and_its_own_agents() {
    let Some(f) = Fixture::new("life") else { return };
    f.report("A");
    f.protocol(skiff_core::PROTOCOL);
    let selection = f.selection(Some("A"));
    let host = host(&selection).await;
    let c = host.connection();
    for cap in [
        Capability::session_lifecycle(),
        Capability::session_groups(),
        Capability::session_agents(),
    ] {
        assert!(c.hello().capabilities.contains(&cap), "{cap:?}");
    }
    let agents = legacy_agents(&c).await.unwrap();
    assert_eq!(agents[0].command, "claude --host-side");
    let created = legacy_create(&c, spec(Some("/srv/λ repo"), false)).await.unwrap();
    assert_eq!(created.cwd, std::path::PathBuf::from("/srv/λ repo"));
    // No cwd: the Host account's home, chosen on the Host.
    let home = legacy_create(&c, spec(None, false)).await.unwrap();
    assert_eq!(home.cwd, std::path::PathBuf::from("/home/fixture"));
    assert_eq!(legacy_rename(&c, "s1", "renamed").await.unwrap().name.as_deref(), Some("renamed"));
    legacy_kill(&c, "s1").await.unwrap();
    assert!(legacy_groups(&c).await.unwrap().is_empty());
    let group: skiff_core::group::Group = serde_json::from_value(json!({"name":"Review",
        "layout":{"type":"pane","session":"s1"},"cwd":"/srv/λ repo"}))
    .unwrap();
    assert_eq!(legacy_save_group(&c, group).await.unwrap().id, "g1");
    legacy_delete_group(&c, "g1").await.unwrap();
    let sent = f.requests();
    let cmds: Vec<&str> = sent.iter().map(|v| v["cmd"].as_str().unwrap()).collect();
    assert_eq!(
        cmds,
        ["list_agents", "create_session", "create_session", "rename_session", "kill", "list_groups", "save_group", "delete_group"]
    );
    assert_eq!(
        sent[1],
        json!({"cmd":"create_session","spec":{"label":"claude","name":"Fix it","role":"agent",
            "cwd":"/srv/λ repo","command":"claude","args":["--host-side"],"peek":false,"cols":100,"rows":30}})
    );
    assert_eq!(sent[2]["spec"]["cwd"], json!(null));
    assert_eq!(sent[6]["group"]["cwd"], json!("/srv/λ repo"));
    // Nothing on a remote Host reaches this Mac's process control.
    assert!(host.process_control().is_none());
}

/// Capabilities say what the endpoint can do. A remote skiffd on another
/// protocol is never changed by this app, so its mutations and agent list
/// are not offered, though the local adapter implements them: each is
/// refused before any frame. Terminals and reads stay.
#[tokio::test]
async fn a_remote_host_on_another_protocol_is_offered_no_mutation() {
    let Some(f) = Fixture::new("proto") else { return };
    f.report("A");
    for protocol in [skiff_core::PROTOCOL - 1, skiff_core::PROTOCOL + 1] {
        f.protocol(protocol);
        let selection = f.selection(Some("A"));
        let host = host(&selection).await;
        let c = host.connection();
        let caps = &c.hello().capabilities;
        assert!(caps.contains(&Capability::session_terminal()));
        assert!(caps.contains(&Capability::session_inventory()));
        for cap in [
            Capability::session_lifecycle(),
            Capability::session_groups(),
            Capability::session_agents(),
        ] {
            assert!(!caps.contains(&cap), "protocol {protocol}: {cap:?}");
        }
        assert!(legacy_create(&c, spec(Some("/srv"), false)).await.is_err());
        assert!(legacy_kill(&c, "s1").await.is_err());
        assert!(legacy_groups(&c).await.is_err());
        assert!(legacy_agents(&c).await.is_err());
        assert!(f.requests().is_empty(), "protocol {protocol}: {:?}", f.requests());
    }
}

/// A peek tool shows a file of this Mac: the remote adapter refuses it as a
/// permission error, with no frame, though the local adapter starts it.
#[tokio::test]
async fn a_remote_host_never_starts_a_peek_tool() {
    let Some(f) = Fixture::new("peek") else { return };
    f.report("A");
    f.protocol(skiff_core::PROTOCOL);
    let selection = f.selection(Some("A"));
    let host = host(&selection).await;
    let e = legacy_create(&host.connection(), spec(Some("/srv"), true)).await.unwrap_err();
    assert!(format!("{e:#}").contains("Permission"), "{e:#}");
    assert!(f.requests().is_empty());
}

/// No replay: a create whose reply is lost is an error, written once. The
/// next connection, after the reconnect, sends nothing for it; a later
/// mutation is the person's new request and goes once.
#[tokio::test]
async fn a_mutation_whose_reply_is_lost_is_never_sent_again() {
    let Some(f) = Fixture::new("replay") else { return };
    f.report("A");
    f.protocol(skiff_core::PROTOCOL);
    std::fs::write(f.dir.join("drop-create"), "").unwrap();
    let selection = f.selection(Some("A"));
    let first = host(&selection).await;
    let e = legacy_create(&first.connection(), spec(Some("/srv"), false)).await.unwrap_err();
    assert!(!format!("{e:#}").is_empty());
    drop(first);
    std::fs::remove_file(f.dir.join("drop-create")).unwrap();
    let again = host(&selection).await;
    legacy_kill(&again.connection(), "s1").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let cmds: Vec<String> = f.requests().iter().map(|v| v["cmd"].as_str().unwrap().to_string()).collect();
    assert_eq!(cmds, ["create_session", "kill"]);
}
