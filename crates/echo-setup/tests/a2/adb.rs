//! Controlled responses to the rooted A2 adb operations.
use super::{account_reply, notify, projection_read, status_reply};
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};

pub(super) fn shell(root: &Path, command: &str) -> io::Result<bool> {
    if command.starts_with("for p in /data/local/echo/bin/.install ") {
        let owner = if root.join("a2-installed").exists() {
            "a2"
        } else {
            "legacy"
        };
        writeln!(io::stdout(), "{owner}")?;
        return Ok(true);
    }
    if command == "id -ru && id -u" {
        writeln!(
            io::stdout(),
            "0\n{}",
            if root.join("not-root").exists() { 2000 } else { 0 }
        )?;
        return Ok(true);
    }
    if command == "cat /proc/sys/kernel/random/boot_id" {
        let boot = u8::from(root.join("rebooted").exists());
        writeln!(io::stdout(), "00000000-0000-0000-0000-00000000000{boot}")?;
        return Ok(true);
    }
    if command == "reboot" {
        fs::write(root.join("rebooted"), b"")?;
        return Ok(true);
    }
    if command == "getprop sys.boot_completed" {
        writeln!(io::stdout(), "1")?;
        return Ok(true);
    }
    if command.contains(" install --magiskpolicy ") {
        if root.join("a2-install-failed").exists() {
            std::process::exit(1);
        }
        fs::write(root.join("a2-installed"), "0.2.0\n")?;
        fs::write(root.join("process-user"), "nobody")?;
        writeln!(io::stdout(), "echo-bridge install: installed")?;
        return Ok(true);
    }
    let Some((_, operation)) = command.split_once("/data/local/echo/bin/boot install --host ") else {
        return Ok(false);
    };
    installed(root, operation)?;
    Ok(true)
}

fn installed(root: &Path, operation: &str) -> io::Result<()> {
    match operation {
        "owner" => io::stdout().write_all(&fs::read(root.join("a2-installed"))?),
        "status" => status_reply(root),
        "start" | "request-park" => Ok(()),
        "ensure-signin" | "signin" | "link" | "api-key-status" | "stop" => {
            account_reply(root, &format!("sh -s -- --device {operation}"))?;
            Ok(())
        }
        "setup-config" | "setup-pairing" | "doctor" => {
            projection_read(root, &format!("sh -s -- --device {operation}"))?;
            Ok(())
        }
        "confirm-cursor" => {
            let cursor = fs::read_to_string(root.join("confirm-cursor"))?;
            writeln!(
                io::stdout(),
                "{}",
                cursor
                    .lines()
                    .next()
                    .ok_or_else(|| io::Error::other("missing cursor"))?
            )
        }
        value if value.starts_with("confirm-check ") => {
            notify(root, "wake-barrier", b"ready")?;
            io::stdout().write_all(&fs::read(root.join("confirm-evidence"))?)
        }
        value if value.starts_with("api-key-write ") || value == "config-write" => {
            let mut input = Vec::new();
            io::stdin().read_to_end(&mut input)?;
            fs::write(root.join("last-private-input"), input)
        }
        _ => Err(io::Error::other("unknown A2 operation")),
    }
}
