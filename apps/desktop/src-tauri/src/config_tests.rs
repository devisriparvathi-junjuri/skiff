//! The project change path against an in-process `skiff-host` on private state.
use std::{path::PathBuf, sync::Arc};

use skiff_client::host::{data, local::HostSelection};
use skiff_host_api::ProjectEdit;

use crate::connection::{project_write, App, DATA_UNREAD};

/// A change is made from the last read of this Host's projects, sent
/// once, and after a conflict not again until the projects are read.
#[test]
fn project_changes_are_made_from_the_last_read_and_never_resent() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = PathBuf::from("/tmp").join(format!("sw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    let old = std::env::var_os("SKIFF_CONFIG");
    let config = dir.join("projects.toml");
    std::env::set_var("SKIFF_CONFIG", &config);
    std::fs::write(&config, format!("[[project]]\nname = \"app\"\npath = \"{}\"\n", dir.display())).unwrap();
    let file = || std::fs::read_to_string(&config).unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let session = dir.join("s.sock");
        let listener = tokio::net::UnixListener::bind(&session).unwrap();
        // A skiffd that reports Host A.
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    while stream.read_line(&mut line).await.unwrap_or(0) > 0 {
                        let id = serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"].clone();
                        let reply = serde_json::json!({"id":id,"type":"pong","version":skiff_core::VERSION,"protocol":skiff_core::PROTOCOL,
                            "identity":{"host_id":"A","instance_id":"run","os":"test","account":"uid:1"}});
                        let mut bytes = serde_json::to_vec(&reply).unwrap();
                        bytes.push(b'\n');
                        stream.get_mut().write_all(&bytes).await.unwrap();
                        line.clear();
                    }
                });
            }
        });
        let mut hello = skiff_host::hello().unwrap();
        hello.host.host_id = skiff_host_api::HostId::try_from("A".to_string()).unwrap();
        let listener = tokio::net::UnixListener::bind(skiff_core::socket::data_socket(&session)).unwrap();
        let service = tokio::spawn(skiff_host::serve(listener, Arc::new(hello)));
        let app = App::default();
        app.set_selection(HostSelection::new(session.clone(), dir.join("bootstrap-id")));
        let app = &app;
        let color = |hex: &'static str| {
            project_write(app, move |c, revision| async move {
                data::project_edit(&c, &revision, "app", ProjectEdit::Color { color: hex.into() }).await.map(|r| ((), r))
            })
        };

        // Nothing read yet: nothing sent.
        assert_eq!(color("#111111").await, Err(DATA_UNREAD.to_string()));
        assert!(!file().contains("color"));

        // From a read: written, and its answer is the next change's revision.
        super::read_projects(app).await.unwrap();
        color("#222222").await.unwrap();
        color("#333333").await.unwrap();
        assert!(file().contains("#333333"));

        // Another writer on the Host: refused, and the revision forgotten,
        // so the next change waits for a read instead of being re-sent.
        skiff_core::config::set_project_short("app", "ap").unwrap();
        let refused = color("#444444").await.unwrap_err();
        assert!(refused.contains("changed on this Host"), "{refused}");
        assert!(app.revision.lock().unwrap().is_none());
        assert_eq!(color("#444444").await, Err(DATA_UNREAD.to_string()));
        assert!(!file().contains("#444444") && file().contains("short = \"ap\""));
        super::read_projects(app).await.unwrap();
        color("#444444").await.unwrap();
        assert!(file().contains("#444444"));
        service.abort();
    });
    match old {
        Some(v) => std::env::set_var("SKIFF_CONFIG", v),
        None => std::env::remove_var("SKIFF_CONFIG"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
