//! Cargo-built recording external boundary. Never invokes adb or device tools.
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::path::PathBuf;

fn main() -> io::Result<()> {
    let root = std::env::var_os("ECHO_SETUP_TEST_ROOT").ok_or_else(|| io::Error::other("missing fixture"))?;
    let root = PathBuf::from(root);
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let mut ledger = OpenOptions::new().create(true).append(true).open(root.join("ledger"))?;
    writeln!(
        ledger,
        "{}",
        serde_json::to_string(&args.iter().map(|v| v.to_string_lossy()).collect::<Vec<_>>())?
    )?;
    let _ = writeln!(io::stderr(), "SYNTHETIC-TOKEN SYNTHETIC-ACCOUNT SYNTHETIC-PRIVATE-PATH");
    if args.first().is_some_and(|v| v == "-s") {
        args.drain(..2);
    }
    if args.first().is_some_and(|v| v == "devices") {
        let devices = fs::read_to_string(root.join("devices"))?;
        write!(io::stdout(), "List of devices attached\r\n{devices}\r\n")?;
    } else if args.first().is_some_and(|v| v == "shell") {
        let command = args
            .get(1)
            .ok_or_else(|| io::Error::other("missing shell"))?
            .to_string_lossy();
        shell(&root, &command)?;
    } else if args.first().is_none_or(|v| v != "push") {
        std::process::exit(1);
    }
    Ok(())
}

fn shell(root: &Path, command: &str) -> io::Result<()> {
    if (command == "test -x /data/local/hassmic/runas" && root.join("no-hassmic").exists())
        || (command.contains("prepare-archive") && root.join("unsafe-archive").exists())
    {
        std::process::exit(1);
    }
    if command.starts_with("sh -s -- --device ") {
        let mut script = Vec::new();
        io::stdin().read_to_end(&mut script)?;
        fs::write(root.join("adapter-input"), script)?;
        if root.join("operation-failed").exists() {
            std::process::exit(1);
        }
        if command == "sh -s -- --device status" {
            io::stdout().write_all(&fs::read(root.join("status"))?)?;
            if root.join("status-refused").exists() {
                std::process::exit(1);
            }
            return Ok(());
        }
        fs::write(root.join("last-operation"), command)?;
    }
    if command.contains("--device start") {
        fs::write(root.join("process-user"), "nobody")?;
    }
    writeln!(io::stdout(), "SYNTHETIC-TOKEN SYNTHETIC-ACCOUNT SYNTHETIC-PRIVATE-PATH")
}
