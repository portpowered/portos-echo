//! `OpenAI` API key intake and private delivery. The key never reaches argv,
//! diagnostics or a host temporary file; only fixed categories escape.
use crate::{Failure, Result, adb::Adb};
use echo_contracts::secret::Secret;
use serde::Deserialize;
use std::io::Read;
use std::path::Path;

const LIMIT: usize = 16_384;
const PRIVATE: &str = "/data/local/echo-bridge/private/";
const RESERVED: [&str; 5] = [
    "bridge.toml",
    "bridge.log",
    "bridge.trace",
    "screen-pairing.txt",
    "launch.sh",
];

/// The installed `[live] mode`, projected without its credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    Chatgpt,
    ApiKey,
}

impl AuthMode {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Chatgpt => "chatgpt",
            Self::ApiKey => "api_key",
        }
    }
}

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    live: Live,
}

#[derive(Deserialize, Default)]
struct Live {
    mode: Option<String>,
    credential_file: Option<String>,
}

/// Project only `live.mode` and `live.credential_file` from config text.
/// # Errors
/// `setup_config_invalid` for malformed TOML or an unknown mode.
pub fn project(raw: &str) -> Result<(AuthMode, Option<String>)> {
    let config: Config = toml::from_str(raw).map_err(|_| Failure("setup_config_invalid"))?;
    let mode = match config.live.mode.as_deref() {
        None | Some("chatgpt") => AuthMode::Chatgpt,
        Some("api_key") => AuthMode::ApiKey,
        Some(_) => return Err(Failure("setup_config_invalid")),
    };
    Ok((mode, config.live.credential_file))
}

/// Read a local key file into `Secret`; the path and bytes never reach output.
/// # Errors
/// `api_key_file_unreadable` or `api_key_file_invalid`.
pub fn read(path: &Path) -> Result<Secret<String>> {
    let file = std::fs::File::open(path).map_err(|_| Failure("api_key_file_unreadable"))?;
    let mut buffer = Vec::new();
    let read = file
        .take(u64::try_from(LIMIT + 1).map_err(|_| Failure("api_key_file_invalid"))?)
        .read_to_end(&mut buffer);
    let bytes = Secret::new(buffer);
    read.map_err(|_| Failure("api_key_file_unreadable"))?;
    parse(bytes.expose())
}

/// The bridge's rule (`echo_signin::api_key`): one trailing LF or CRLF is
/// dropped; the rest must be non-blank and free of control characters.
/// # Errors
/// `api_key_file_invalid` for empty, oversized, non-UTF-8 or control text.
pub fn parse(bytes: &[u8]) -> Result<Secret<String>> {
    const INVALID: Failure = Failure("api_key_file_invalid");
    if bytes.len() > LIMIT {
        return Err(INVALID);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| INVALID)?;
    let key = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if key.trim().is_empty() || key.chars().any(char::is_control) {
        return Err(INVALID);
    }
    Ok(Secret::new(key.to_owned()))
}

/// Accept only a plain leaf directly in the bridge's private directory.
/// # Errors
/// `api_key_path_unsupported` for any other configured path.
pub fn target(credential_file: Option<&str>) -> Result<&str> {
    let path = credential_file.ok_or(Failure("api_key_path_unsupported"))?;
    let leaf = path.strip_prefix(PRIVATE).ok_or(Failure("api_key_path_unsupported"))?;
    if leaf.is_empty()
        || leaf.len() > 64
        || leaf.starts_with('.')
        || RESERVED.contains(&leaf)
        || !leaf
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(Failure("api_key_path_unsupported"));
    }
    Ok(path)
}

/// Stream the key over adb stdin into the root-only stage, then let the
/// authored shell publish it for the bridge user. The stage removes the copy.
/// # Errors
/// `api_key_install_failed`, never adb output.
pub async fn deliver(adb: &Adb, stage: &str, key: &Secret<String>, target: &str) -> Result<()> {
    const FAILED: Failure = Failure("api_key_install_failed");
    let source = format!("{stage}/api-key");
    adb.shell_input(&format!("umask 077 && cat > '{source}'"), key.expose().as_bytes())
        .await
        .map_err(|_| FAILED)?;
    let output = adb
        .shell(&format!(
            "sh '{stage}/bundle/scripts/echo-install.sh' --device api-key --source '{source}' --file '{target}'"
        ))
        .await
        .map_err(|_| FAILED)?;
    if output != "echo-install: api-key completed\n" {
        return Err(FAILED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_files_drop_one_line_ending_and_refuse_unsafe_text() -> Result<()> {
        for (raw, expected) in [
            ("dummy-local-only", "dummy-local-only"),
            ("dummy-local-only\n", "dummy-local-only"),
            ("dummy-local-only\r\n", "dummy-local-only"),
            ("dummy local only", "dummy local only"),
            ("dummy-ü", "dummy-ü"),
        ] {
            assert_eq!(parse(raw.as_bytes())?.expose(), expected);
        }
        let oversized = "k".repeat(LIMIT + 1);
        for invalid in [
            "",
            "\n",
            "   \r\n",
            "dummy\n\n",
            "dummy\rlocal",
            "dummy\tlocal",
            "dummy\u{7f}",
            "dummy\u{85}",
            oversized.as_str(),
        ] {
            assert_eq!(parse(invalid.as_bytes()).err(), Some(Failure("api_key_file_invalid")));
        }
        assert_eq!(parse(&[0xff, 0xfe]).err(), Some(Failure("api_key_file_invalid")));
        Ok(())
    }

    #[test]
    fn key_files_are_read_bounded_and_missing_files_have_a_fixed_category() -> Result<()> {
        let directory = std::env::temp_dir().join(format!("echo-setup-key-{}", std::process::id()));
        std::fs::create_dir_all(&directory).map_err(|_| Failure("test"))?;
        let path = directory.join("key.txt");
        std::fs::write(&path, "dummy-local-only\r\n").map_err(|_| Failure("test"))?;
        assert_eq!(read(&path)?.expose(), "dummy-local-only");
        std::fs::write(&path, "k".repeat(LIMIT + 1)).map_err(|_| Failure("test"))?;
        assert_eq!(read(&path).err(), Some(Failure("api_key_file_invalid")));
        std::fs::remove_dir_all(&directory).map_err(|_| Failure("test"))?;
        assert_eq!(read(&path).err(), Some(Failure("api_key_file_unreadable")));
        Ok(())
    }

    #[test]
    fn modes_project_without_credentials_and_targets_stay_private_leaves() -> Result<()> {
        let base = include_str!("../../../../examples/bridge.echo.toml");
        assert_eq!(
            project(base)?,
            (
                AuthMode::Chatgpt,
                Some("/data/local/echo-bridge/private/live.json".into())
            )
        );
        let api = base.replace("mode = \"chatgpt\"", "mode = \"api_key\"");
        assert_eq!(project(&api)?.0, AuthMode::ApiKey);
        assert_eq!(project("[live]\ncredential_file = 'x'\n")?.0, AuthMode::Chatgpt);
        assert_eq!(
            project("[live]\nmode = 'other'\n"),
            Err(Failure("setup_config_invalid"))
        );
        assert_eq!(
            target(Some("/data/local/echo-bridge/private/live.json"))?,
            "/data/local/echo-bridge/private/live.json"
        );
        for path in [
            None,
            Some("/data/local/echo-bridge/private/"),
            Some("/data/local/echo-bridge/private/bridge.toml"),
            Some("/data/local/echo-bridge/private/screen-pairing.txt"),
            Some("/data/local/echo-bridge/private/.install-stage"),
            Some("/data/local/echo-bridge/private/codex-home/auth.json"),
            Some("/data/local/echo-bridge/private/../bin/key"),
            Some("/data/local/echo-bridge/private/key'$(id)"),
            Some("/data/local/tmp/key"),
        ] {
            assert_eq!(target(path), Err(Failure("api_key_path_unsupported")));
        }
        Ok(())
    }
}
