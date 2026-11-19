use std::{env, fs, io::Write, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut output = std::io::stdout();
    writeln!(output, "cargo::rustc-check-cfg=cfg(echo_screen_embedded)")?;
    writeln!(output, "cargo::rerun-if-env-changed=ECHO_SCREEN_BUNDLE")?;
    if let Some(source) = env::var_os("ECHO_SCREEN_BUNDLE") {
        let bytes = fs::read(&source)?;
        if bytes.is_empty() {
            return Err("compiled screen bundle is empty".into());
        }
        let destination = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing build output")?);
        fs::write(destination.join("screen.js"), bytes)?;
        writeln!(output, "cargo::rerun-if-changed={}", PathBuf::from(source).display())?;
        writeln!(output, "cargo::rustc-cfg=echo_screen_embedded")?;
    }
    Ok(())
}
