//! CLI sends operations to the existing runtime; it cannot write a grant.
use super::{Settings, bridge_operations::Operation};
use crate::Error;
use echo_config::BridgeConfig;
use echo_contracts::secret::Secret;
use echo_mcp_auth::AuthError;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Command {
    config: PathBuf,
    operation: Operation,
}

pub struct Prepared {
    settings: Settings,
    operation: Operation,
}

/// Parse MCP verbs without reading configuration or credentials.
/// # Errors
/// Rejects mixed lab inputs, duplicates and unsupported flags offline.
pub fn parse(args: &[String], lab_present: bool) -> Result<Option<Command>, Error> {
    let Some(verb) = args.first().map(String::as_str) else {
        return Ok(None);
    };
    let operation = match verb {
        "link" => Operation::Link,
        "--refresh-now" => Operation::Refresh,
        "unlink" => Operation::Unlink,
        _ => return Ok(None),
    };
    if lab_present || args.len() != 3 || args[1] != "--config" || args[2].is_empty() || args[2].starts_with("--") {
        return Err(Error::Input);
    }
    Ok(Some(Command {
        config: args[2].clone().into(),
        operation,
    }))
}

impl Command {
    /// Validate nominated configuration without contacting any peer or opening a store.
    /// # Errors
    /// Returns a fixed input category for invalid configuration.
    pub fn prepare(self) -> Result<Prepared, Error> {
        let config = BridgeConfig::load(self.config).map_err(|_| Error::Input)?;
        Ok(Prepared {
            settings: Settings::configured(&config)?.ok_or(Error::Input)?,
            operation: self.operation,
        })
    }
}

impl Prepared {
    /// Reads only nominated configuration and the owner pairing credential.
    /// # Errors
    /// Returns fixed input/control/OAuth categories; never opens an auth store.
    pub async fn execute(self, output: &mut impl Write) -> Result<(), Error> {
        let settings = self.settings;
        let token = pairing(&settings.pairing_file)?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| Error::Control)?;
        let mut response = http
            .post(format!("http://{}/operator/mcp", settings.listen))
            .bearer_auth(token.expose())
            .json(&serde_json::json!({"operation":self.operation.name()}))
            .send()
            .await
            .map_err(|_| Error::Control)?;
        let code = response.status().as_u16();
        if ![200, 422].contains(&code) || response.content_length().is_some_and(|n| n > 4096) {
            return Err(Error::Control);
        }
        let body = bounded_body(&mut response).await?;
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| Error::Control)?;
        if code == 422 {
            return Err(Error::Mcp(error(value["error"].as_str().ok_or(Error::Control)?)?));
        }
        if value != serde_json::json!({"outcome":self.operation.outcome()}) {
            return Err(Error::Control);
        }
        writeln!(output, "MCP {}", self.operation.outcome()).map_err(|_| Error::Control)
    }
}

async fn bounded_body(response: &mut reqwest::Response) -> Result<Vec<u8>, Error> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Control)? {
            if chunk.len() > 4096 - bytes.len() {
                return Err(Error::Control);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| Error::Control)?
}

pub(crate) fn pairing(path: &Path) -> Result<Secret<String>, Error> {
    let file = std::fs::File::open(path).map_err(|_| Error::Control)?;
    let mut bytes = Vec::new();
    file.take(16_385).read_to_end(&mut bytes).map_err(|_| Error::Control)?;
    if bytes.len() > 16_384 {
        return Err(Error::Control);
    }
    let text = String::from_utf8(bytes).map_err(|_| Error::Control)?;
    let token = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(&text);
    if token.trim().is_empty() || token.chars().any(char::is_control) {
        return Err(Error::Control);
    }
    Ok(Secret::new(token.into()))
}

fn error(category: &str) -> Result<AuthError, Error> {
    match category {
        "discovery" => Ok(AuthError::Discovery),
        "transport" => Ok(AuthError::Transport),
        "protocol" => Ok(AuthError::Protocol),
        "access_denied" => Ok(AuthError::AccessDenied),
        "expired" => Ok(AuthError::Expired),
        "relink_required" => Ok(AuthError::RelinkRequired),
        "storage" => Ok(AuthError::Storage),
        _ => Err(Error::Control),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_link_cli_rejects_invalid_arguments_before_reading_config() {
        for verb in ["link", "unlink", "--refresh-now"] {
            let args = [verb.into(), "--config".into(), "missing".into()];
            assert!(parse(&args, false).unwrap().is_some());
            assert_eq!(parse(&args, true).err(), Some(Error::Input));
            assert_eq!(parse(&args[..2], false).err(), Some(Error::Input));
        }
        assert!(parse(&["status".into()], false).unwrap().is_none());
        assert_eq!(error("access_denied"), Ok(AuthError::AccessDenied));
        assert_eq!(error("hostile text"), Err(Error::Control));
    }
}
