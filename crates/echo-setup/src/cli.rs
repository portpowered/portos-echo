//! Parse all arguments before any external effect.
use crate::{Failure, Result};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;

pub const VERSION: &str = env!("ECHO_SETUP_RELEASE_VERSION");
pub const HELP: &str = "echo-setup --help | --version
echo-setup install|update [--version VERSION | --from-dir DIR] [--config FILE] [OPTIONS]
echo-setup status|rollback|doctor [OPTIONS]
echo-setup uninstall [--purge] [OPTIONS]
OPTIONS: --adb PATH --serial DEVICE --json --timeout-seconds N --release-base URL
Default release matches this executable; never latest. Versions omit leading v.
Downloads require HTTPS; explicit http://127.0.0.1:PORT mirrors are for local tests.
Exit 0: completed; 1: prerequisite/integrity/device failure; 2: usage; 3: setup timeout.
Expired provider codes also exit 3. Run install again to resume with a fresh code.
Ctrl+C cancels setup with exit 1 after bounded cleanup; run install again to resume.
Stdin is not consumed; closed stdin is supported for bootstrap/script invocation.
Device identifiers, credentials and dependency logs are never printed.
Install/update confirm the running nobody identity and requested version after start.
Rollback confirms the running nobody identity after the shell restores and starts.
Doctor: read-only checks, at most 15 seconds; clock within 300 seconds of host,
256 MiB free on data, TCP 443 egress as nobody (device nc required).
";

pub struct Options {
    pub command: String,
    pub version: String,
    pub from_dir: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub adb: Option<PathBuf>,
    // Device identifiers deliberately implement neither Debug nor Display.
    pub serial: Option<OsString>,
    pub json: bool,
    pub purge: bool,
    pub timeout_seconds: u64,
    pub release_base: String,
}

impl Options {
    /// # Errors
    /// Returns a fixed category for invalid or conflicting arguments.
    pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let mut arguments = arguments.into_iter();
        let command = arguments.next().ok_or(Failure("usage"))?;
        let command = command.to_str().ok_or(Failure("usage"))?.to_owned();
        if ![
            "install",
            "update",
            "status",
            "rollback",
            "uninstall",
            "doctor",
            "--help",
            "--version",
        ]
        .contains(&command.as_str())
        {
            return Err(Failure("usage"));
        }
        let mut result = Self::defaults(command);
        let mut seen = BTreeSet::new();
        while let Some(flag) = arguments.next() {
            let flag = flag.to_str().ok_or(Failure("usage"))?;
            if !seen.insert(flag.to_owned()) || result.command.starts_with("--") {
                return Err(Failure("usage"));
            }
            match flag {
                "--json" => result.json = true,
                "--purge" => result.purge = true,
                _ => result.value(flag, arguments.next().ok_or(Failure("usage"))?)?,
            }
        }
        result.validate(&seen)?;
        Ok(result)
    }

    fn defaults(command: String) -> Self {
        Self {
            command,
            version: VERSION.to_owned(),
            from_dir: None,
            config: None,
            adb: None,
            serial: None,
            json: false,
            purge: false,
            timeout_seconds: 300,
            release_base: "https://github.com/portpowered/portos-echo/releases/download".to_owned(),
        }
    }

    fn value(&mut self, flag: &str, value: OsString) -> Result<()> {
        if value.is_empty() || value.to_string_lossy().starts_with("--") {
            return Err(Failure("usage"));
        }
        match flag {
            "--adb" => self.adb = Some(value.into()),
            "--serial" => self.serial = Some(value),
            "--config" => self.config = Some(value.into()),
            "--from-dir" => self.from_dir = Some(value.into()),
            "--version" => self.version = version(value.to_str().ok_or(Failure("usage"))?)?,
            "--release-base" => self.release_base = value.into_string().map_err(|_| Failure("usage"))?,
            "--timeout-seconds" => {
                self.timeout_seconds = value
                    .to_str()
                    .and_then(|v| v.parse().ok())
                    .filter(|v| *v > 0)
                    .ok_or(Failure("usage"))?;
            }
            _ => return Err(Failure("usage")),
        }
        Ok(())
    }

    fn validate(&self, seen: &BTreeSet<String>) -> Result<()> {
        let install = matches!(self.command.as_str(), "install" | "update");
        if (self.purge && self.command != "uninstall")
            || (!install
                && ["--version", "--from-dir", "--config", "--release-base"]
                    .iter()
                    .any(|v| seen.contains(*v)))
            || (self.from_dir.is_some() && seen.contains("--version"))
            || (self.from_dir.is_some() && seen.contains("--release-base"))
        {
            return Err(Failure("usage"));
        }
        Ok(())
    }
}

/// Normalize once and reject path syntax, whitespace and malformed `SemVer`.
/// # Errors
/// Returns `invalid_version` for any unsupported release identity.
pub fn version(value: &str) -> Result<String> {
    crate::version::normalized(value).ok_or(Failure("invalid_version"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        Options::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn default_is_embedded_and_version_normalizes_once() -> Result<()> {
        assert_eq!(parse(&["install"])?.version, VERSION);
        assert_eq!(parse(&["install", "--version", "v1.2.3-rc.1"])?.version, "1.2.3-rc.1");
        for invalid in ["vv1.2.3", "../1.2.3", "1.2", "01.2.3", "1.2.3-", "1.2.3-01", "1.2.3\n"] {
            assert_eq!(version(invalid), Err(Failure("invalid_version")));
        }
        Ok(())
    }

    #[test]
    fn parser_rejects_conflicts_and_invalid_flags_before_effects() {
        for args in [
            vec!["install", "--json", "--json"],
            vec!["install", "--from-dir", "x", "--version", "1.2.3"],
            vec!["status", "--config", "x"],
            vec!["install", "--purge"],
            vec!["doctor", "--timeout-seconds", "0"],
            vec!["--help", "--json"],
            vec!["install", "--unknown", "x"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn local_paths_are_opaque_arguments() -> Result<()> {
        let options = parse(&["install", "--config", "space ü ' ; $ config", "--adb", "adb path"])?;
        assert_eq!(options.config, Some(PathBuf::from("space ü ' ; $ config")));
        assert_eq!(options.adb, Some(PathBuf::from("adb path")));
        Ok(())
    }
}
