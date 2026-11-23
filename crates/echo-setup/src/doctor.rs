//! Read-only diagnostic checks; external observations never become diagnostics.
use crate::{
    adb::Adb,
    cli::{InstallPath, Options},
};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CHECKS: [(&str, &str); 7] = [
    ("adb", "Install Android platform-tools or select --adb PATH."),
    (
        "device",
        "Connect and authorize one device or select a ready device with --serial.",
    ),
    ("hassmic", "Install hassmic separately and check its runas helper."),
    (
        "egress",
        "Check device DNS, TCP port 443 and egress as the bridge identity (nobody 3990,inet,input); nc is required.",
    ),
    ("clock", "Set the device clock within five minutes of the host clock."),
    ("disk_free", "Free at least 256 MiB on the device data partition."),
    (
        "process_user",
        "Start the installed bridge as nobody; stop and reinstall any unsafe process.",
    ),
];

/// Observe seven checks with a bounded transport; never repair device state.
pub async fn execute(options: &Options) -> Value {
    let mut passed = [false; 7];
    let mut auth_mode = "unavailable";
    let mut owner = InstallPath::Legacy;
    let ceiling = Duration::from_secs(options.timeout_seconds.min(15));
    let _ = tokio::time::timeout(ceiling, observe(options, &mut passed, &mut auth_mode, &mut owner)).await;
    let checks: Vec<_> = CHECKS
        .iter()
        .zip(passed)
        .map(|((name, fix), ok)| {
            let fix = if *name == "hassmic" && owner == InstallPath::A2 {
                "Check the A2 device role and its local Wyoming socket."
            } else {
                fix
            };
            json!({"name":name, "ok":ok, "fix":(!ok).then_some(fix)})
        })
        .collect();
    json!({"type":"result", "command":"doctor", "ok":passed.iter().all(|ok| *ok), "checks":checks, "auth_mode":auth_mode})
}

async fn observe(
    options: &Options,
    passed: &mut [bool; 7],
    auth_mode: &mut &'static str,
    owner: &mut InstallPath,
) -> crate::Result<()> {
    let mut adb = Adb::discover(options.adb.as_deref())?;
    passed[0] = true;
    adb.select_ready(options.serial.as_deref()).await?;
    passed[1] = true;
    let installed = adb.installed_owner().await;
    *owner = adb.owner();
    installed?;
    let (probes, mode) = selected(&adb).await?;
    passed[2..].copy_from_slice(&probes);
    *auth_mode = mode;
    Ok(())
}

#[allow(clippy::disallowed_methods, reason = "Existing host time")]
pub(crate) async fn selected(adb: &Adb) -> crate::Result<([bool; 5], &'static str)> {
    let ceiling = Duration::from_secs(15);
    tokio::time::timeout(ceiling, async {
        let (success, output) = adb.script("doctor").await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs());
        let checks = if success {
            probes(&output, now).unwrap_or_default()
        } else {
            [false; 5]
        };
        Ok((checks, crate::setup::installed_auth_mode(adb).await))
    })
    .await
    .map_err(|_| crate::Failure("doctor_timeout"))?
}

fn probes(output: &str, now: Option<u64>) -> Option<[bool; 5]> {
    let mut values = std::collections::BTreeMap::new();
    for line in output.lines() {
        let (key, value) = line.split_once('=')?;
        if !["hassmic", "egress", "clock", "disk_free", "process_user"].contains(&key)
            || values.insert(key, value).is_some()
        {
            return None;
        }
    }
    if values.len() != 5 {
        return None;
    }
    Some([
        values["hassmic"] == "ok",
        values["egress"] == "ok",
        values["clock"]
            .parse::<u64>()
            .ok()
            .zip(now)
            .is_some_and(|(device, host)| device.abs_diff(host) <= 300),
        values["disk_free"].parse::<u64>().is_ok_and(|kb| kb >= 256 * 1024),
        matches!(values["process_user"], "nobody" | "9999"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_and_untrusted_probe_frames_fail_closed() {
        let healthy = "hassmic=ok\negress=ok\nclock=1000\ndisk_free=262144\nprocess_user=9999\n";
        assert_eq!(probes(healthy, Some(1300)), Some([true; 5]));
        assert_eq!(probes(healthy, Some(1301)), Some([true, true, false, true, true]));
        assert_eq!(probes(healthy, None), Some([true, true, false, true, true]));
        assert_eq!(
            probes(&healthy.replace("262144", "262143"), Some(1000)),
            Some([true, true, true, false, true])
        );
        for invalid in ["", &format!("{healthy}egress=ok\n"), &format!("{healthy}unknown=ok\n")] {
            assert_eq!(probes(invalid, Some(1000)), None);
        }
    }
}
