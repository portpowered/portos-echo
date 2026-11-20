#[path = "src/version.rs"]
mod version;

use std::io::{self, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let supplied = std::env::var_os("ECHO_SETUP_RELEASE_VERSION");
    let release = supplied
        .as_ref()
        .map_or(Some(env!("CARGO_PKG_VERSION")), |value| value.to_str())
        .and_then(version::normalized)
        .ok_or("invalid ECHO_SETUP_RELEASE_VERSION")?;
    // Build identity is canonical, so an explicitly supplied leading v is refused.
    if supplied.is_some_and(|value| value != release.as_str()) {
        return Err("invalid ECHO_SETUP_RELEASE_VERSION".into());
    }
    writeln!(io::stdout(), "cargo:rerun-if-env-changed=ECHO_SETUP_RELEASE_VERSION")?;
    writeln!(io::stdout(), "cargo:rerun-if-changed=src/version.rs")?;
    writeln!(io::stdout(), "cargo:rerun-if-changed=build.rs")?;
    writeln!(io::stdout(), "cargo:rustc-env=ECHO_SETUP_RELEASE_VERSION={release}")?;
    Ok(())
}
