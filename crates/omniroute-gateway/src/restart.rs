//! Supervised restart of the gateway.
//!
//! Connection activity only takes effect after a restart (backends are built
//! once at startup), so the dashboard offers a restart button. This module
//! keeps the sharp edges in one place: restart works only when the gateway
//! runs supervised under launchd, and it re-execs the *same* binary —
//! in-flight requests die, but no new code is deployed by the restart itself.

use std::process::Stdio;

/// launchd label of this service. Overridable for tests and custom installs.
pub fn service_label() -> String {
    std::env::var("OMNIROUTE_SERVICE_LABEL")
        .ok()
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| "com.omniroute.rust".to_string())
}

/// Numeric uid of the current user, via `id -u`.
pub fn current_uid() -> Option<String> {
    let output = std::process::Command::new("id").arg("-u").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let uid = String::from_utf8(output.stdout).ok()?;
    let uid = uid.trim().to_string();
    (!uid.is_empty()).then_some(uid)
}

/// Shell that restarts the service after a grace delay.
///
/// The delay lets the HTTP 202 flush before `kickstart -k` kills us. The
/// script runs detached (reparented), so it survives our own death.
pub fn kickstart_script(label: &str, uid: &str) -> String {
    format!("sleep 2; exec launchctl kickstart -k gui/{uid}/{label}")
}

/// Whether launchd currently supervises `label` in this user's domain.
pub fn is_supervised(label: &str, uid: &str) -> bool {
    std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{label}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Fire the detached restart script. Returns once the supervisor child is
/// spawned — the actual restart lands ~2s later.
pub fn request_restart(label: &str, uid: &str) -> std::io::Result<()> {
    std::process::Command::new("sh")
        .args(["-c", &kickstart_script(label, uid)])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_kicks_the_right_job_after_a_delay() {
        assert_eq!(
            kickstart_script("com.example.svc", "501"),
            "sleep 2; exec launchctl kickstart -k gui/501/com.example.svc"
        );
    }

    #[test]
    fn bogus_label_is_not_supervised() {
        assert!(!is_supervised("com.omniroute.definitely-not-a-job", "0"));
    }

    #[test]
    fn uid_looks_numeric() {
        let uid = current_uid().expect("id -u works on unix");
        assert!(uid.chars().all(|ch| ch.is_ascii_digit()));
    }
}
