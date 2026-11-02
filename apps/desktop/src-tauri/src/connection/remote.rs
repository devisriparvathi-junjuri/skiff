//! Remote Host connection and identity pinning.

use super::*;

/// A remote Host: connect through its forward, check the identity it reports
/// against its pin, and never start, reload, stop or replace anything. A
/// version mismatch is reported, not acted on. Every failure leaves its cause
/// in the warning, so the status and the picker say why, in words that never
/// repeat ssh's own output.
pub(super) async fn connect_remote(app: &App, selection: &HostSelection, guard: &mut Option<Arc<SelectedHost>>) -> Result<(Arc<SelectedHost>, bool), String> {
    let destination = selection.ssh_target().map(|t| t.destination.clone()).unwrap_or_default();
    let arrival = match selection.ssh_target() {
        Some(t) => {
            let arrival = crate::hosts::update_on_connect(t).await;
            // The probe that ran for it found the Host's one socket: the
            // forward goes there. Without it, the connect probes itself.
            if let Some(socket) = crate::hosts::found_socket(t) {
                selection.set_remote_socket(socket);
            }
            arrival
        }
        None => None,
    };
    match try_remote(app, selection, &destination, arrival.as_ref(), crate::hosts::path()).await {
        Ok(host) => {
            // The passive note: "skiffd 2.1 (updated from 2.0)".
            if let Some(from) = arrival.and_then(|a| a.updated_from) {
                *app.replaced.lock().await = Some(from);
            }
            if let Some(t) = selection.ssh_target().cloned() {
                tauri::async_runtime::spawn_blocking(move || crate::hosts::note_login(&t, true));
            }
            *app.changed.lock().unwrap() = None;
            *app.live.lock().unwrap() = Some(host.connection());
            *guard = Some(host.clone());
            Ok((host, false))
        }
        Err(e) => {
            let e = refine_refused(e, selection).await;
            let (warning, message) = remote_failure(&e, &destination);
            if let (Some(_), Some(t)) = (&warning, selection.ssh_target().cloned()) {
                tauri::async_runtime::spawn_blocking(move || crate::hosts::note_login(&t, false));
            }
            if let Some(skiff_host_api::Error::IdentityChanged { received, .. }) = e.downcast_ref() {
                *app.changed.lock().unwrap() = Some((destination.clone(), received.clone()));
            }
            // The wait between tries is not a cause: the last cause stays.
            if let Some(w) = warning {
                *app.warning.lock().await = Some(w);
            }
            Err(message)
        }
    }
}

/// A refused channel names two causes. The probe looks at the socket: when
/// one is there, the sshd blocks the forward. Any other error stays.
async fn refine_refused(e: anyhow::Error, selection: &HostSelection) -> anyhow::Error {
    use skiff_client::host::{check, ssh::SshError};
    let (Some(SshError::NoSkiffd { .. }), Some(t)) = (e.downcast_ref::<SshError>(), selection.ssh_target()) else { return e };
    let options = skiff_client::host::ssh::SshOptions::default();
    match check::run_probe(t, &options).await {
        Ok(check::ProbeRun::Probed(p, _)) => match e.downcast::<SshError>() {
            Ok(ssh) => ssh.refine(p.socket_live).into(),
            Err(e) => e,
        },
        _ => e,
    }
}

/// The warning and the message for a failed remote connection. Only known
/// causes are named; anything else gets one plain line with what to check.
/// `None` warning: a wait between tries, which keeps the last cause.
pub(super) fn remote_failure(e: &anyhow::Error, destination: &str) -> (Option<Warning>, String) {
    use skiff_client::host::ssh::SshError;
    let warn = |kind: &'static str, message: String| (Some(Warning { kind, message: message.clone(), sessions: None }), message);
    if let Some(e) = e.downcast_ref::<SshError>() {
        return match e {
            SshError::Waiting { .. } => (None, e.to_string()),
            _ => warn("remote", e.to_string()),
        };
    }
    match e.downcast_ref::<skiff_host_api::Error>() {
        Some(skiff_host_api::Error::IdentityChanged { .. }) => warn(
            "identity",
            format!(
                "{destination} answers as another Skiff Host than before. Skiff stopped and sends nothing to it. \
                 If you expect this, as after Skiff was reinstalled there, accept the new identity."
            ),
        ),
        Some(skiff_host_api::Error::MalformedContract { .. }) => warn(
            "remote",
            format!("Skiff cannot read the replies from the sessions service on {destination}. Settings › Hosts checks {destination}."),
        ),
        _ if e.downcast_ref::<RemoteSilent>().is_some() => warn(
            "remote",
            format!("skiffd on {destination} did not answer within {} s. Settings › Hosts checks {destination}.", REMOTE_HELLO_TIMEOUT.as_secs()),
        ),
        _ => warn(
            "remote",
            format!("Cannot reach skiffd on {destination} over ssh. Settings › Hosts checks {destination} and finds the cause."),
        ),
    }
}
