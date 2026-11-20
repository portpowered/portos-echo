//! Project authored shell observations into a fixed, private-data-free result.
use crate::adb::Adb;
use crate::cli::Options;
use crate::{Failure, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// # Errors
/// Returns fixed prerequisite, transport or malformed-observation categories.
pub async fn execute(options: &Options) -> Result<Value> {
    let mut adb = Adb::discover(options.adb.as_deref())?;
    adb.select(options.serial.as_deref()).await?;
    let operation = match options.command.as_str() {
        "status" => "status",
        "uninstall" if options.purge => "uninstall --purge",
        "uninstall" => "uninstall",
        _ => return Err(Failure("command_not_implemented")),
    };
    let (success, output) = adb.script(operation).await?;
    if options.command == "status" {
        let result = status(&output)?;
        if !success && result["ok"] == true {
            return Err(Failure("status_failed"));
        }
        return Ok(result);
    }
    if !success {
        return Err(Failure("uninstall_failed"));
    }
    Ok(json!({"type":"result", "command":"uninstall", "ok":true, "purged":options.purge}))
}

fn fields(output: &str) -> Result<BTreeMap<&str, &str>> {
    let mut fields = BTreeMap::new();
    for line in output.lines().take_while(|line| *line != "bridge.log (last 5):") {
        let (key, value) = line.split_once('=').ok_or(Failure("status_failed"))?;
        if ![
            "process",
            "process_user",
            "uptime_seconds",
            "hassmic_link",
            "version_current",
            "version_previous",
        ]
        .contains(&key)
            || fields.insert(key, value).is_some()
        {
            return Err(Failure("status_failed"));
        }
    }
    if !["process", "process_user", "uptime_seconds", "hassmic_link"]
        .iter()
        .all(|key| fields.contains_key(key))
        || fields.contains_key("version_current") != fields.contains_key("version_previous")
    {
        return Err(Failure("status_failed"));
    }
    Ok(fields)
}

fn release_version(value: Option<&&str>) -> Result<Option<String>> {
    match value.copied() {
        None | Some("null") => Ok(None),
        Some(value) => crate::version::normalized(value)
            .filter(|normalized| normalized == value)
            .map(Some)
            .ok_or(Failure("status_failed")),
    }
}

fn status(output: &str) -> Result<Value> {
    let fields = fields(output)?;
    let running = match fields["process"] {
        "running" => true,
        "stopped" => false,
        _ => return Err(Failure("status_failed")),
    };
    let user = match (running, fields["process_user"]) {
        (true, "nobody" | "9999") => "nobody",
        (true, "root" | "0") => "root",
        _ => "unavailable",
    };
    let error = (running && user != "nobody").then_some("unsafe_process_user");
    let uptime = running.then(|| fields["uptime_seconds"].parse::<u64>().ok()).flatten();
    let link = match fields["hassmic_link"] {
        line if line.contains("client connected") => Some("connected"),
        line if line.contains("no Home Assistant link") => Some("disconnected"),
        _ => None,
    };
    Ok(json!({
        "type":"result", "command":"status", "ok":error.is_none(),
        "versions":{
            "current":release_version(fields.get("version_current"))?,
            "previous":release_version(fields.get("version_previous"))?
        },
        "process":if running {"running"} else {"stopped"},
        "process_user":user, "uptime_seconds":uptime,
        "hassmic_link":link, "last_error":error
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_fail_closed_and_never_promote_log_text_to_fields() -> Result<()> {
        let output = "process=running\nprocess_user=9999\nuptime_seconds=42\nhassmic_link=client connected\nbridge.log (last 5):\nprocess_user=root\n";
        assert_eq!(status(output)?["process_user"], "nobody");
        assert_eq!(status(output)?["uptime_seconds"], 42);
        for invalid in [
            "",
            "process=running\n",
            &output.replace("process=running", "process=unknown"),
            &output.replace("process_user=9999", "process_user=9999\nprocess_user=root"),
        ] {
            assert_eq!(status(invalid), Err(Failure("status_failed")));
        }
        Ok(())
    }
}
