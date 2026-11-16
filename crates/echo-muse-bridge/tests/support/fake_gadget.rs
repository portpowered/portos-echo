//! T3 test support only; scripts and captures live beside the hard-linked executable.
use std::{
    io::{Read, Write},
    process::ExitCode,
};

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let program = std::env::current_exe()?;
    let directory = program.parent().ok_or("missing executable directory")?;
    let script: serde_json::Value = serde_json::from_slice(&std::fs::read(directory.join("script.json"))?)?;
    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin)?;
    let capture = serde_json::json!({"argv": std::env::args().skip(1).collect::<Vec<_>>(), "stdin": stdin});
    std::fs::write(directory.join("capture.json"), capture.to_string())?;
    std::fs::write(directory.join("child.pid"), std::process::id().to_string())?;
    let mut calls = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("calls.jsonl"))?;
    writeln!(calls, "{capture}")?;
    #[cfg(unix)]
    if let Some(path) = script["gate_socket"].as_str() {
        let mut gate = std::os::unix::net::UnixStream::connect(path)?;
        gate.write_all(&[1])?;
        let mut release = [0];
        gate.read_exact(&mut release)?;
    }
    std::io::stdout().write_all(script["stdout"].as_str().ok_or("missing stdout")?.as_bytes())?;
    std::io::stderr().write_all(script["stderr"].as_str().ok_or("missing stderr")?.as_bytes())?;
    Ok(u8::try_from(script["exit"].as_u64().ok_or("missing exit")?)?)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _written = writeln!(std::io::stderr(), "{error}");
            ExitCode::FAILURE
        }
    }
}
