//! What `ping` reports as this daemon's identity: the Host's durable id and
//! this run's instance id. A reload execs a new image, which calls [`init`]
//! again, so the instance id changes with every start and every reload while
//! the pid stays.

use std::sync::OnceLock;

use skiff_core::protocol::Identity;

static IDENTITY: OnceLock<Option<Identity>> = OnceLock::new();

/// Reads or makes the Host's id. Call it once the socket is known: private
/// state keeps its own id. A failure is logged, and `ping` reports no
/// identity, as an older daemon does; the sessions run on.
pub fn init() {
    let path = skiff_core::host_identity::path();
    let identity = match skiff_core::host_identity::load(&path) {
        Ok(host_id) => Some(Identity {
            host_id,
            instance_id: uuid::Uuid::new_v4().to_string(),
            os: std::env::consts::OS.into(),
            account: skiff_core::host_identity::account(),
        }),
        Err(e) => {
            tracing::warn!("no Host identity in pong: {e:#}");
            None
        }
    };
    let _ = IDENTITY.set(identity);
}

pub fn get() -> Option<Identity> {
    IDENTITY.get().cloned().flatten()
}
