//! `skiff-host [--session-socket <skiffd socket>] [--this-mac]`
//!
//! Serves the Host's data plane at `<skiffd socket stem>-host.sock`, beside
//! the skiffd it pairs with, and uses that socket's state: a private test
//! socket gets its own `projects.toml` and Host id, exactly as skiffd does.
//! One skiff-host runs per computer, beside the one skiffd; the newer app
//! replaces an older one. It starts nothing and stops nothing else; SIGTERM
//! or SIGINT end only this process. At start it makes the agent folder.
use std::{path::PathBuf, sync::Arc};

use anyhow::{bail, Context, Result};
use tokio::signal::unix::{signal, SignalKind};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut session = None;
    let mut this_mac = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session-socket" => session = Some(PathBuf::from(args.next().context("--session-socket needs a path")?)),
            "--this-mac" => this_mac = true,
            "--version" => {
                println!("skiff-host {}", skiff_core::VERSION);
                return Ok(());
            }
            other => bail!("unknown argument {other}; usage: skiff-host [--session-socket <skiffd socket>] [--this-mac]"),
        }
    }
    // The app sets it for This Mac's own service: results under home show.
    skiff_host::set_this_mac(this_mac);
    let session = session.unwrap_or_else(skiff_core::socket::socket_path);
    // Before anything reads config or identity: the state follows skiffd's socket.
    skiff_core::state::set_socket(session.clone());
    let hello = Arc::new(skiff_host::hello().context("read the Host identity")?);
    // The agent folder and its cache, Skipper on or off: drops from the app
    // and from `skiff attach --host` go there.
    if let Err(e) = skiff_core::config::load().and_then(|cfg| skiff_client::skipper::make_folder(&cfg)) {
        eprintln!("skiff-host: {e:#}");
    }
    let path = skiff_core::socket::data_socket(&session);
    tokio::runtime::Runtime::new()?.block_on(async move {
        let (listener, socket) = skiff_host::bind(&path).await?;
        eprintln!(
            "skiff-host {} listening on {} for Host {} (instance {})",
            skiff_core::VERSION,
            path.display(),
            hello.host.host_id.as_str(),
            hello.instance_id.as_str()
        );
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        let result = tokio::select! {
            r = skiff_host::serve(listener, hello) => r.context("accept"),
            _ = term.recv() => Ok(()),
            _ = int.recv() => Ok(()),
        };
        // Only its own socket, if the path still names it.
        socket.release();
        result
    })
}
