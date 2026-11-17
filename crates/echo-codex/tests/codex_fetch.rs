//! T3 offline helper CLI/cache boundaries; provisioning happens before Cargo.
use std::{error::Error, io::Write, path::PathBuf, process::Command};

#[test]
fn t3_verified_cache_corruption_and_concurrent_writers() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg(root.join("crates/echo-codex/tests/support/codex_fetch.py"))
        .arg(&root)
        .output()?;
    std::io::stdout().write_all(&output.stdout)?;
    assert!(
        output.status.success(),
        "helper cache fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FETCH_CACHE_PASS"));
    Ok(())
}
