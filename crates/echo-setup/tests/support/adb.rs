//! Cargo-built recording external boundary. Never invokes adb or device tools.
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::path::PathBuf;

fn main() -> io::Result<()> {
    let root = if let Some(root) = std::env::var_os("ECHO_SETUP_TEST_ROOT") {
        PathBuf::from(root)
    } else {
        // Preserve the invocation's per-case symlink directory on Unix. The
        // immutable Cargo-built image can then be shared without rewriting an
        // executable while other test threads are spawning children.
        PathBuf::from(
            std::env::args_os()
                .next()
                .ok_or_else(|| io::Error::other("missing argv"))?,
        )
        .parent()
        .ok_or_else(|| io::Error::other("missing fixture"))?
        .to_path_buf()
    };
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
    } else if args.first().is_some_and(|v| v == "forward") {
        forward(&root, &args[1..])?;
    } else if args.first().is_none_or(|v| v != "push") {
        std::process::exit(1);
    }
    Ok(())
}

fn forward(root: &Path, args: &[std::ffi::OsString]) -> io::Result<()> {
    let state = root.join("forwards.json");
    let mut active: Vec<String> = serde_json::from_slice(&fs::read(&state)?)?;
    let targets = root.join("forward-targets.json");
    let mut mapped: std::collections::BTreeMap<String, String> = serde_json::from_slice(&fs::read(&targets)?)?;
    if args.first().is_some_and(|arg| arg == "--list") {
        for port in active {
            let remote = mapped.get(&port).ok_or_else(|| io::Error::other("missing target"))?;
            writeln!(io::stdout(), "SYNTHETIC-DEVICE {port} {remote}\r")?;
        }
    } else if args.first().is_some_and(|arg| arg == "--remove") {
        let port = args
            .get(1)
            .ok_or_else(|| io::Error::other("missing port"))?
            .to_string_lossy();
        if root.join("remove-fails").exists() {
            std::process::exit(1);
        }
        active.retain(|value| value != &port);
        fs::write(state, serde_json::to_vec(&active)?)?;
        notify(root, "cleanup-notify", port.as_bytes())?;
    } else {
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "--no-rebind");
        assert_eq!(args[1].to_str().and_then(|value| value.strip_prefix("tcp:")), Some("0"));
        let replies_path = root.join("forward-replies.json");
        let mut replies: Vec<String> = serde_json::from_slice(&fs::read(&replies_path)?)?;
        let reply = replies.remove(0);
        fs::write(replies_path, serde_json::to_vec(&replies)?)?;
        if reply == "FAIL" {
            std::process::exit(1);
        }
        notify(root, "allocation-barrier", b"ready")?;
        let port = format!("tcp:{}", reply.trim());
        if reply.trim().parse::<u16>().is_ok_and(|port| port > 0) && !active.contains(&port) {
            active.push(port.clone());
            mapped.insert(port, args[2].to_string_lossy().into_owned());
            fs::write(targets, serde_json::to_vec(&mapped)?)?;
            fs::write(state, serde_json::to_vec(&active)?)?;
        }
        write!(io::stdout(), "{reply}")?;
    }
    Ok(())
}

fn notify(root: &Path, name: &str, message: &[u8]) -> io::Result<()> {
    let path = root.join(name);
    if path.exists() {
        let port: u16 = fs::read_to_string(path)?.parse().map_err(io::Error::other)?;
        let mut socket = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))?;
        socket.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        socket.write_all(message)?;
        let mut ack = [0];
        socket.read_exact(&mut ack)?;
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
            let status = root.join("status");
            if status.exists() {
                io::stdout().write_all(&fs::read(status)?)?;
            } else if root.join("process-user").exists() {
                write!(
                    io::stdout(),
                    "version_current=0.2.0\r\nversion_previous=null\r\nprocess=running\r\nprocess_user=nobody\r\nuptime_seconds=1\r\nhassmic_link=unavailable\r\n"
                )?;
            } else {
                std::process::exit(1);
            }
            if root.join("status-refused").exists() {
                std::process::exit(1);
            }
            return Ok(());
        }
        if command == "sh -s -- --device doctor" {
            io::stdout().write_all(&fs::read(root.join("doctor"))?)?;
            return Ok(());
        }
        if command == "sh -s -- --device wake-cursor" || command.starts_with("sh -s -- --device wake-check ") {
            let name = if command.ends_with("wake-cursor") {
                "wake-cursor"
            } else {
                "wake-evidence"
            };
            notify(root, "wake-barrier", b"ready")?;
            io::stdout().write_all(&fs::read(root.join(name))?)?;
            return Ok(());
        }
        if command == "sh -s -- --device confirm-cursor" || command.starts_with("sh -s -- --device confirm-check ") {
            let name = if command.ends_with("confirm-cursor") {
                "confirm-cursor"
            } else {
                "confirm-evidence"
            };
            notify(root, "wake-barrier", b"ready")?;
            io::stdout().write_all(&fs::read(root.join(name))?)?;
            return Ok(());
        }
        fs::write(root.join("last-operation"), command)?;
    }
    if command.contains("--device start") {
        fs::write(root.join("process-user"), "nobody")?;
    }
    writeln!(io::stdout(), "SYNTHETIC-TOKEN SYNTHETIC-ACCOUNT SYNTHETIC-PRIVATE-PATH")
}
