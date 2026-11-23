//! Cargo-built recording external boundary. Never invokes adb or device tools.
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::path::PathBuf;

fn main() -> io::Result<()> {
    if let Some(root) = std::env::var_os("ECHO_SETUP_TEST_BOOTSTRAP") {
        return bootstrap_child(Path::new(&root));
    }
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
    // Prompt observation and account dispatch intentionally run concurrently.
    // Append each small record in one write rather than interleaving formatting
    // fragments from independently spawned fake-adb processes.
    let record = serde_json::to_string(&args.iter().map(|v| v.to_string_lossy()).collect::<Vec<_>>())?;
    ledger.write_all(format!("{record}\n").as_bytes())?;
    let _ = writeln!(io::stderr(), "SYNTHETIC-TOKEN SYNTHETIC-ACCOUNT SYNTHETIC-PRIVATE-PATH");
    if args.first().is_some_and(|v| v == "-s") {
        args.drain(..2);
    }
    if args.first().is_some_and(|v| v == "devices") {
        let offline = root.join("offline-source");
        if offline.exists() {
            let source = PathBuf::from(fs::read_to_string(offline)?);
            for name in ["scripts/echo-install.sh", "MANIFEST"] {
                fs::write(source.join(name), b"changed after verification")?;
            }
        }
        let devices = fs::read_to_string(root.join("devices"))?;
        write!(io::stdout(), "List of devices attached\r\n{devices}\r\n")?;
    } else if args.first().is_some_and(|v| v == "shell") && args.get(1).is_some_and(|v| v == "-T") {
        wifi(&root, &args[2..])?;
    } else if args.first().is_some_and(|v| v == "shell") {
        let command = args
            .get(1)
            .ok_or_else(|| io::Error::other("missing shell"))?
            .to_string_lossy();
        shell(&root, &command)?;
    } else if args.first().is_some_and(|v| v == "forward") {
        forward(&root, &args[1..])?;
    } else if args.first().is_some_and(|v| v == "push") {
        if root.join("offline-source").is_file() && args[2].to_string_lossy().ends_with("/bundle") {
            for (member, captured) in [
                ("scripts/echo-install.sh", "script"),
                ("MANIFEST", "manifest"),
                ("licenses/dependencies/example/NOTICE", "attribution"),
            ] {
                fs::copy(Path::new(&args[1]).join(member), root.join(captured))?;
            }
        }
    } else {
        std::process::exit(1);
    }
    Ok(())
}

fn wifi(root: &Path, args: &[std::ffi::OsString]) -> io::Result<()> {
    let (op, remote) = match args {
        [no_input, remote] if no_input == "-n" => ("status", remote),
        [remote] => ("join", remote),
        _ => return Err(io::Error::other("invalid Wi-Fi shell flags")),
    };
    let expected = format!(
        "if [ ! -e /data/local/echo/bin/current ]; then printf '%s\\n' wifi-unsupported; \
         exit 1; fi; exec /data/local/echo/bin/current wifi {op}"
    );
    assert_eq!(remote, expected.as_str());
    if let Some(bridge) = std::env::var_os("ECHO_SETUP_TEST_WIFI_BRIDGE") {
        let status = std::process::Command::new(bridge)
            .env("ECHO_LAB_WIFI_SOCKET", root.join("wifi.sock"))
            .args(["wifi", op])
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    if op == "status" {
        assert!(input.is_empty());
    } else if root.join("wifi-reflect").is_file() {
        io::stdout().write_all(&input)?;
        io::stderr().write_all(&input)?;
        return Ok(());
    }
    io::stdout().write_all(&fs::read(root.join("wifi-output"))?)?;
    if root.join("wifi-fail").exists() {
        std::process::exit(1);
    }
    Ok(())
}

fn bootstrap_child(root: &Path) -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    fs::write(root.join("installer-args.json"), serde_json::to_vec(&args)?)?;
    writeln!(io::stdout(), "synthetic installer completed")?;
    let code: i32 = fs::read_to_string(root.join("child-exit"))?
        .parse()
        .map_err(io::Error::other)?;
    if code != 0 {
        writeln!(io::stderr(), "synthetic installer needs resume")?;
    }
    std::process::exit(code);
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
        || (command.ends_with("--device screen-setup") && root.join("screen-failed").exists())
    {
        std::process::exit(1);
    }
    if command.starts_with("umask 077 && cat > '/data/local/tmp/echo-setup-") {
        // Private bytes arrive on stdin only; argv is recorded in the ledger.
        let mut key = Vec::new();
        io::stdin().read_to_end(&mut key)?;
        fs::write(root.join("api-key-input"), key)?;
        return Ok(());
    }
    if command.contains("/bundle/scripts/echo-install.sh' --device api-key --source '") {
        fs::write(root.join("api-key-target"), command)?;
        return writeln!(io::stdout(), "echo-install: api-key completed");
    }
    if command.starts_with("sh -s -- --device ") {
        let mut script = Vec::new();
        io::stdin().read_to_end(&mut script)?;
        fs::write(root.join("adapter-input"), script)?;
        if root.join("operation-failed").exists() {
            std::process::exit(1);
        }
        if projection_read(root, command)? || account_reply(root, command)? {
            return Ok(());
        }
        if command == "sh -s -- --device status" {
            return status_reply(root);
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
        if command == "sh -s -- --device rollback" && root.join("previous").exists() {
            fs::remove_dir_all(root.join("active"))?;
            fs::rename(root.join("previous"), root.join("active"))?;
        }
        fs::write(root.join("last-operation"), command)?;
    }
    if command.contains("--device start") {
        fs::write(root.join("process-user"), "nobody")?;
    }
    writeln!(io::stdout(), "SYNTHETIC-TOKEN SYNTHETIC-ACCOUNT SYNTHETIC-PRIVATE-PATH")
}

fn status_reply(root: &Path) -> io::Result<()> {
    let status = root.join("status");
    if status.exists() {
        io::stdout().write_all(&fs::read(status)?)?;
    } else if root.join("active/version").exists() {
        let current = fs::read_to_string(root.join("active/version"))?;
        let previous = root.join("previous/version");
        let previous = if previous.exists() {
            fs::read_to_string(previous)?
        } else {
            "null".to_owned()
        };
        let user = fs::read_to_string(root.join("process-user"))?;
        write!(
            io::stdout(),
            "version_current={current}\r\nversion_previous={previous}\r\nprocess=running\r\nprocess_user={user}\r\nuptime_seconds=1\r\nhassmic_link=unavailable\r\n"
        )?;
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
    Ok(())
}

fn account_reply(root: &Path, command: &str) -> io::Result<bool> {
    if command == "sh -s -- --device stop" && root.join("stop-reply").exists() {
        notify(root, "stop-barrier", b"ready")?;
        io::stdout().write_all(&fs::read(root.join("stop-reply"))?)?;
        return Ok(true);
    }
    if !["signin", "ensure-signin", "link", "api-key-status"]
        .iter()
        .any(|verb| command == format!("sh -s -- --device {verb}"))
    {
        return Ok(false);
    }
    notify(root, "account-barrier", b"ready")?;
    let verb = command
        .strip_prefix("sh -s -- --device ")
        .ok_or_else(|| io::Error::other("missing verb"))?;
    let specific = root.join(format!("{verb}-reply"));
    let reply = if specific.exists() {
        specific
    } else {
        root.join("account-reply")
    };
    io::stdout().write_all(&fs::read(reply)?)?;
    if root.join("account-failed").exists() {
        std::process::exit(1);
    }
    Ok(true)
}

fn projection_read(root: &Path, command: &str) -> io::Result<bool> {
    let Some(name) = command.strip_prefix("sh -s -- --device ") else {
        return Ok(false);
    };
    if !["setup-config", "setup-pairing", "doctor"].contains(&name) {
        return Ok(false);
    }
    if name != "doctor" {
        notify(root, "private-barrier", b"ready")?;
    }
    io::stdout().write_all(&fs::read(root.join(name))?)?;
    Ok(true)
}
