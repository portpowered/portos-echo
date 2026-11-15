//! Standalone loopback counterpart. No credential flags or external bindings.
use echo_spoof_mcp::{Fault, Options, Spoof};
use std::io::{self, Write};
use std::process::ExitCode;

const USAGE: &str = "echo-spoof-mcp [--port <0..65535>] [--fault <http_500|slow|malformed_json_rpc>] [--slow-ms <unsigned-ms>]\necho-spoof-mcp --help\n";

fn parse(args: &[String]) -> Result<Option<Options>, ()> {
    if args == ["--help"] {
        return Ok(None);
    }
    let mut options = Options::default();
    let mut seen = std::collections::HashSet::new();
    for pair in args.chunks(2) {
        if pair.len() != 2 || !seen.insert(pair[0].as_str()) {
            return Err(());
        }
        match pair[0].as_str() {
            "--port" => options.port = pair[1].parse().map_err(|_| ())?,
            "--slow-ms" => options.slow_ms = pair[1].parse().map_err(|_| ())?,
            "--fault" => options.fault = Fault::parse(&pair[1]).ok_or(())?,
            _ => return Err(()),
        }
    }
    Ok(Some(options))
}

#[tokio::main]
async fn main() -> ExitCode {
    if run().await.is_ok() {
        ExitCode::SUCCESS
    } else {
        let _ = io::stderr().write_all(b"echo-spoof-mcp: invalid arguments or server failure\n");
        ExitCode::FAILURE
    }
}

async fn run() -> Result<(), ()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let Some(options) = parse(&args)? else {
        io::stdout().write_all(USAGE.as_bytes()).map_err(|_| ())?;
        return Ok(());
    };
    let spoof = Spoof::start(options).await.map_err(|_| ())?;
    let line = serde_json::json!({"event":"ready","url":spoof.url()});
    writeln!(io::stdout(), "{line}").map_err(|_| ())?;
    io::stdout().flush().map_err(|_| ())?;
    tokio::signal::ctrl_c().await.map_err(|_| ())?;
    spoof.shutdown().await.map_err(|_| ())
}

#[cfg(test)]
mod spoof_cli_tests;
