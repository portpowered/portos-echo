//! T3 test support only; scripts and captures live beside the copied executable.
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
