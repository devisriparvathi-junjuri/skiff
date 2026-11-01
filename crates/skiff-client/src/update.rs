//! Whether a running skiffd can move onto an installed one with its
//! sessions, and what to tell the user when it cannot. The app's badge and
//! `skiff status` use the same words, so they give the same advice (#182).

use std::{path::Path, process::Stdio, time::Duration};

use crate::DaemonInfo;

/// The installed skiffd answers `--reload-info` within this.
const INFO_TIMEOUT: Duration = Duration::from_secs(5);

/// What `skiffd --reload-info` reports.
#[derive(Debug, Clone)]
pub struct Target {
    pub version: String,
    /// Handover versions it reads.
    pub handover: Vec<u32>,
}

/// Asks `binary` what it can adopt. Never starts a daemon: `--reload-info`
/// prints and exits.
pub async fn reload_info(binary: &Path) -> Result<Target, String> {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("--reload-info").stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(INFO_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("{} did not answer --reload-info", binary.display()))?
        .map_err(|e| format!("run {}: {e}", binary.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .ok()
        .filter(|_| out.status.success())
        .ok_or_else(|| format!("{} does not support reload", binary.display()))?;
    Ok(Target {
        version: v["version"].as_str().unwrap_or_default().to_string(),
        handover: v["handover"].as_array().map_or_else(Vec::new, |a| a.iter().filter_map(|h| h.as_u64().map(|h| h as u32)).collect()),
    })
}

/// Can the running daemon move onto `target` and keep its sessions?
#[derive(Debug, PartialEq, Eq)]
pub enum Handover {
    /// Yes: a reload keeps every session.
    Ready,
    /// `target` is no newer than the daemon. Nothing to do.
    NotNewer,
    /// No. The reason, as one clause for [`unfinished`].
    Blocked(String),
}

pub fn handover(info: &DaemonInfo, target: &Target) -> Handover {
    if !skiff_core::compare_versions(&target.version, &info.version).is_gt() {
        return Handover::NotNewer;
    }
    let (Some(state), Some(_)) = (info.reload_state, info.pid) else {
        return Handover::Blocked(format!("skiffd {} is too old to hand over its sessions", info.version));
    };
    if !target.handover.contains(&state) {
        return Handover::Blocked(format!(
            "skiffd {} cannot take over sessions from skiffd {} (handover {state}; it reads {:?})",
            target.version, info.version, target.handover
        ));
    }
    Handover::Ready
}

/// The message for an update that a handover cannot finish. `running`: the
/// daemon's version. `installed`: the version the update brings. `why`: the
/// reason from [`handover`] or from a reload that failed. `restart`: how
/// this front end restarts skiffd, such as "Restart skiffd" or a command.
pub fn unfinished(running: &str, installed: &str, why: &str, live: Option<usize>, restart: &str) -> String {
    let sessions = match live {
        Some(1) => "1 session keeps running".to_string(),
        Some(n) => format!("{n} sessions keep running"),
        None => "Its sessions keep running".to_string(),
    };
    format!(
        "skiffd {running} still runs. The update to {installed} cannot finish without a restart: {why}. \
         {sessions} on {running} until you restart. {restart} stops them, starts {installed}, \
         and brings each pane back as a shell in its folder."
    )
}

/// The message for an update that a handover finishes. `reload`: how this
/// front end starts it.
pub fn ready(running: &str, installed: &str, reload: &str) -> String {
    format!("skiffd {running} still runs. {reload} moves it to {installed} and keeps every session.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(version: &str, state: Option<u32>) -> DaemonInfo {
        DaemonInfo { version: version.into(), protocol: skiff_core::PROTOCOL, reload_state: state, pid: Some(7), debug: None, identity: None }
    }

    /// The case of #182: 0.6.32 read only handover 3, and 0.6.29 writes 2.
    #[test]
    fn says_why_a_handover_cannot_work() {
        let only3 = Target { version: "0.6.32".into(), handover: vec![3] };
        let both = Target { version: "0.6.33".into(), handover: vec![2, 3] };
        assert!(matches!(handover(&info("0.6.29", Some(2)), &only3), Handover::Blocked(w) if w.contains("handover 2")));
        assert_eq!(handover(&info("0.6.29", Some(2)), &both), Handover::Ready);
        assert!(matches!(handover(&info("0.5.0", None), &both), Handover::Blocked(w) if w.contains("too old")));
        assert_eq!(handover(&info("0.6.33", Some(3)), &both), Handover::NotNewer);
    }
}
