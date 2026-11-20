use echo_setup::{Failure, Result, cli};
use std::io::{self, Write};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    std::process::exit(run().await);
}

async fn run() -> i32 {
    let Ok(options) = cli::Options::parse(std::env::args_os().skip(1)) else {
        let _ = writeln!(io::stderr(), "echo-setup: invalid usage; see --help");
        return 2;
    };
    if options.command == "--help" {
        let _ = write!(io::stdout(), "{}", cli::HELP);
        return 0;
    }
    if options.command == "--version" {
        let _ = writeln!(io::stdout(), "echo-setup {}", cli::VERSION);
        return 0;
    }
    let result: Result<serde_json::Value> = match options.command.as_str() {
        "install" | "update" => echo_setup::install::install(&options).await.map(|version| {
            serde_json::json!({"type":"result", "command":options.command, "ok":true, "version":version, "setup":"pending"})
        }),
        "status" | "uninstall" | "rollback" => echo_setup::lifecycle::execute(&options).await,
        "doctor" => Ok(echo_setup::doctor::execute(&options).await),
        _ => Err(Failure("command_not_implemented")),
    };
    let event = match &result {
        Ok(event) => event.clone(),
        Err(error) => {
            serde_json::json!({"type":"result", "command":options.command, "ok":false, "error":error.0, "fix":fix(error.0)})
        }
    };
    render(&options, &event);
    i32::from(event["ok"] != true)
}

fn render(options: &cli::Options, event: &serde_json::Value) {
    if options.json {
        let _ = writeln!(io::stdout(), "{event}");
    } else {
        match options.command.as_str() {
            "install" | "update" if event["ok"] == true => {
                let version = event["version"].as_str().unwrap_or("unavailable");
                let _ = writeln!(
                    io::stdout(),
                    "Echo {version} installed and started; account setup is pending."
                );
            }
            "status" if event.get("process").is_some() => {
                let _ = writeln!(io::stdout(), "{event}");
            }
            "doctor" => {
                if let Some(checks) = event["checks"].as_array() {
                    for check in checks {
                        let name = check["name"].as_str().unwrap_or("unavailable");
                        let state = if check["ok"] == true { "OK" } else { "FAIL" };
                        let fix = check["fix"].as_str().unwrap_or("");
                        let _ = writeln!(io::stdout(), "{name}: {state} {fix}");
                    }
                }
            }
            "rollback" if event["ok"] == true => {
                let _ = writeln!(io::stdout(), "Echo rolled back and started.");
            }
            "uninstall" if event["ok"] == true => {
                let state = if options.purge { "purged" } else { "state retained" };
                let _ = writeln!(io::stdout(), "Echo uninstalled ({state}).");
            }
            _ => {
                let error = event["error"].as_str().unwrap_or("device_operation_failed");
                let _ = writeln!(io::stderr(), "echo-setup: {error}. {}", fix(error));
            }
        }
    }
}

fn fix(error: &str) -> &'static str {
    match error {
        "adb_required" => "Install Android platform-tools or select its executable with --adb PATH.",
        "hassmic_required" => "Hassmic must already be installed; install it separately before continuing.",
        "exactly_one_device_required" | "device_selection_failed" | "device_not_ready" => {
            "Connect and authorize one device, or select a ready device with --serial."
        }
        "invalid_checksum" | "checksum_mismatch" | "invalid_bundle" | "manifest_mismatch" => {
            "Download the release again or supply a complete verified offline bundle."
        }
        "https_required" => "Use an HTTPS release mirror.",
        "unsafe_archive_or_missing_device_tools" => {
            "Use a complete release and check device tar and sha256sum support."
        }
        "rollback_failed" => {
            "A verified previous release could not be restored. Run status and doctor; use a verified bundle to reinstall if needed."
        }
        "stop_failed" => "The bridge did not stop safely. Inspect status before retrying the update.",
        "start_failed" => "The installed files remain available. Run doctor to diagnose startup.",
        "unsafe_process_user" => {
            "The running bridge identity is unsafe. Run status and doctor before retrying; no process was signalled."
        }
        "command_not_implemented" => "This installer command is still under development.",
        _ => "Check device connectivity and prerequisites, then run install again.",
    }
}
