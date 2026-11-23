use echo_setup::{Failure, Result, cli};
use std::io::{self, Write};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    std::process::exit(run().await);
}

async fn run() -> i32 {
    let mut args = std::env::args_os().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "wifi") {
        return echo_setup::wifi::run(args.skip(1))
            .await
            .unwrap_or_else(|_| invalid_usage());
    }
    let Ok(options) = cli::Options::parse(args) else {
        return invalid_usage();
    };
    if options.command == "--help" {
        let _ = write!(io::stdout(), "{}", cli::HELP);
        return 0;
    }
    if options.command == "--version" {
        let _ = writeln!(io::stdout(), "echo-setup {}", cli::VERSION);
        return 0;
    }
    let cancelled = echo_setup::Cancellation::default();
    let signal = cancelled.clone();
    let interrupt = matches!(options.command.as_str(), "install" | "update").then(|| {
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            signal.cancel();
        })
    });
    let result: Result<serde_json::Value> = match options.command.as_str() {
        "install" | "update" => echo_setup::install::install(
            &options, |prompt| present(&options, prompt), || instruct(&options), &cancelled,
        ).await.map(|version| {
            serde_json::json!({"type":"result", "command":options.command, "ok":true, "version":version, "setup":"complete"})
        }),
        "status" | "uninstall" | "rollback" => echo_setup::lifecycle::execute(&options).await,
        "doctor" => Ok(echo_setup::doctor::execute(&options).await),
        _ => Err(Failure("command_not_implemented")),
    };
    if let Some(interrupt) = interrupt {
        interrupt.abort();
    }
    let event = match &result {
        Ok(event) => event.clone(),
        Err(error) => {
            serde_json::json!({"type":"result", "command":options.command, "ok":false, "error":error.0, "fix":fix(error.0)})
        }
    };
    render(&options, &event);
    exit_code(&event)
}

fn invalid_usage() -> i32 {
    let _ = writeln!(io::stderr(), "echo-setup: invalid usage; see --help");
    2
}

fn exit_code(event: &serde_json::Value) -> i32 {
    if matches!(
        event["error"].as_str(),
        Some(
            "setup_timeout"
                | "setup_chatgpt_timeout"
                | "setup_portos_timeout"
                | "setup_chatgpt_code_expired"
                | "setup_portos_code_expired"
        )
    ) {
        3
    } else {
        i32::from(event["ok"] != true)
    }
}

fn present(options: &cli::Options, prompt: &echo_setup::setup::Prompt) -> Result<()> {
    let event = serde_json::to_value(prompt).map_err(|_| Failure("output_failed"))?;
    let mut output = io::stdout().lock();
    if options.json {
        writeln!(output, "{event}")
    } else {
        writeln!(
            output,
            "{}: visit {} and enter {}.",
            event["provider"].as_str().unwrap_or("account"),
            event["verification_uri"].as_str().unwrap_or(""),
            event["user_code"].as_str().unwrap_or("")
        )
    }
    .and_then(|()| output.flush())
    .map_err(|_| Failure("output_failed"))
}

fn instruct(options: &cli::Options) -> Result<()> {
    let mut output = io::stdout().lock();
    let message = "Say Computer near the Echo. Waiting for a fresh wake and bridge run.";
    if options.json {
        let event = serde_json::json!({"type":"instruction", "message":message});
        writeln!(output, "{event}")
    } else {
        writeln!(output, "{message}")
    }
    .and_then(|()| output.flush())
    .map_err(|_| Failure("output_failed"))
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
                    "Echo {version} installed, linked and confirmed a fresh wake and bridge run."
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
                let mode = event["auth_mode"].as_str().unwrap_or("unavailable");
                let _ = writeln!(io::stdout(), "auth_mode: {mode}");
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
        "screen_setup_failed" => {
            "Screen setup failed. Installed files remain. Check the device package manager and home-app state, then run install again."
        }
        "unsafe_process_user" => {
            "The running bridge identity is unsafe. Run status and doctor before retrying; no process was signalled."
        }
        "setup_cancelled" => "Setup cancelled. Installed files remain. Run install again to resume setup.",
        "setup_timeout" => "Setup timed out. Run install again to resume setup.",
        "setup_chatgpt_timeout" | "setup_chatgpt_code_expired" => {
            "ChatGPT sign-in did not complete before its deadline. Installed files remain. Run install again for a fresh ChatGPT code."
        }
        "setup_portos_timeout" | "setup_portos_code_expired" => {
            "Port OS linking did not complete before its deadline. Installed files remain. Run install again for a fresh Port OS code."
        }
        "setup_relink_required" => "ChatGPT still requires sign-in. Run install again to resume setup.",
        "setup_kill_switch" => "The bridge kill switch is active. Inspect status before resuming setup.",
        "setup_account_unsettled" => "Account cleanup could not be confirmed. Inspect status before resuming setup.",
        "api_key_file_unreadable" => "Check that the --openai-api-key-file path exists and is readable.",
        "api_key_file_invalid" => {
            "Put only the OpenAI API key in the file: one line without control characters, at most 16 KiB."
        }
        "api_key_config_mode_chatgpt" => {
            "The --config file uses ChatGPT sign-in. Set [live] mode = \"api_key\" to use an API key."
        }
        "api_key_mode_required" => {
            "The installed config uses ChatGPT sign-in. Pass --config FILE with [live] mode = \"api_key\" and --force."
        }
        "api_key_path_unsupported" => {
            "Set live.credential_file to a file directly in /data/local/echo-bridge/private/, such as live.json."
        }
        "api_key_install_failed" => "The API key could not be written on the Echo. Run install again.",
        "setup_api_key_unconfirmed" => {
            "The bridge did not report API-key mode. Check the installed config, then run install again."
        }
        "setup_api_key_rejected" => {
            "The bridge could not authenticate with the API key. Run install again with --openai-api-key-file and a valid key."
        }
        "command_not_implemented" => "This installer command is still under development.",
        _ => "Check device connectivity and prerequisites, then run install again.",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn terminal_exit_distinguishes_bounded_setup_timeout() {
        for (ok, error, code) in [
            (true, "", 0),
            (false, "setup_timeout", 3),
            (false, "setup_chatgpt_timeout", 3),
            (false, "setup_portos_timeout", 3),
            (false, "setup_chatgpt_code_expired", 3),
            (false, "setup_portos_code_expired", 3),
            (false, "setup_account_unsettled", 1),
        ] {
            assert_eq!(super::exit_code(&serde_json::json!({"ok":ok,"error":error})), code);
        }
    }
}
