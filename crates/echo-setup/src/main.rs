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
    let result: Result<String> = match options.command.as_str() {
        "install" => echo_setup::install::install(&options).await,
        _ => Err(Failure("command_not_implemented")),
    };
    let event = match &result {
        Ok(version) => {
            serde_json::json!({"type":"result", "command":options.command, "ok":true, "version":version, "setup":"pending"})
        }
        Err(error) => {
            serde_json::json!({"type":"result", "command":options.command, "ok":false, "error":error.0, "fix":fix(error.0)})
        }
    };
    if options.json {
        let _ = writeln!(io::stdout(), "{event}");
    } else {
        match &result {
            Ok(version) => {
                let _ = writeln!(
                    io::stdout(),
                    "Echo {version} installed and started; account setup is pending."
                );
            }
            Err(error) => {
                let _ = writeln!(io::stderr(), "echo-setup: {}. {}", error.0, fix(error.0));
            }
        }
    }
    i32::from(result.is_err())
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
        "command_not_implemented" => "This installer command is still under development.",
        _ => "Check device connectivity and prerequisites, then run install again.",
    }
}
