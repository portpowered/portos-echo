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
        let request = http
            .post(format!("http://{}/operator/mcp", settings.listen))
            .bearer_auth(token.expose())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&serde_json::json!({"operation":self.operation.name()}))
            .send();
        let mut response = tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .map_err(|_| Error::Control)?
            .map_err(|_| Error::Control)?;
        if response.status().as_u16() != 200
            || response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                != Some("text/event-stream")
        {
            return Err(Error::Control);
        }
        progress(&mut response, self.operation, output).await
    }
}

// Request-scoped progress has no replayed screen snapshot and cannot belong to
// another operator. The owner emits the very same LinkCode to both observers.
async fn progress(
    response: &mut reqwest::Response,
    operation: Operation,
    output: &mut impl Write,
) -> Result<(), Error> {
    let mut pending = Vec::new();
    let mut shown = false;
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Control)? {
        for byte in chunk {
            pending.push(byte);
            if pending.len() > 4096 {
                return Err(Error::Control);
            }
            if pending.ends_with(b"\n\n") {
                let frame = std::str::from_utf8(&pending).map_err(|_| Error::Control)?;
                let data = frame
                    .strip_prefix("data: ")
                    .and_then(|s| s.strip_suffix("\n\n"))
                    .ok_or(Error::Control)?;
                let value: serde_json::Value = serde_json::from_str(data).map_err(|_| Error::Control)?;
                if present(&value, operation, output, &mut shown)? {
                    return Ok(());
                }
                pending.clear();
            }
        }
    }
    Err(Error::Control)
}

fn present(
    value: &serde_json::Value,
    operation: Operation,
    output: &mut impl Write,
    shown: &mut bool,
) -> Result<bool, Error> {
    if let Some(state) = value.get("state") {
        let echo_screen_contract::UiState::LinkCode {
            user_code,
            verification_uri,
            ..
        } = serde_json::from_value(state.clone()).map_err(|_| Error::Control)?
        else {
            return Err(Error::Control);
        };
        if operation != Operation::Link
            || *shown
            || user_code.chars().any(char::is_control)
            || verification_uri.chars().any(char::is_control)
        {
            return Err(Error::Control);
        }
        writeln!(output, "Verification URL: {verification_uri}\nUser code: {user_code}").map_err(|_| Error::Control)?;
        output.flush().map_err(|_| Error::Control)?;
        *shown = true;
        return Ok(false);
    }
    if let Some(category) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(Error::Mcp(error(category)?));
    }
    if *value != serde_json::json!({"outcome":operation.outcome()}) || (operation == Operation::Link && !*shown) {
        return Err(Error::Control);
    }
    writeln!(output, "MCP {}", operation.outcome()).map_err(|_| Error::Control)?;
    output.flush().map_err(|_| Error::Control)?;
    Ok(true)
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

    struct Probe(tokio::sync::mpsc::UnboundedSender<Vec<u8>>, Vec<u8>);

    impl Write for Probe {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.1.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.send(self.1.clone()).map_err(std::io::Error::other)
        }
    }

    async fn owner(
        mock: &echo_mcp_auth::mock::Mock,
        root: &Path,
    ) -> (
        super::super::Runtime,
        tokio::sync::mpsc::Receiver<echo_screen_contract::UiState>,
    ) {
        let settings = settings(mock, root, ([127, 0, 0, 1], 0).into());
        std::fs::write(&settings.pairing_file, "synthetic-owner").unwrap();
        let (send, receive) = tokio::sync::mpsc::channel(16);
        let trace = crate::bridge_trace::Trace::open(&root.join("trace"), "api_key").unwrap();
        let runtime = super::super::Runtime::open(settings, super::super::Presentation::new(Some(send)), trace, None)
            .await
            .unwrap();
        (runtime, receive)
    }

    fn settings(mock: &echo_mcp_auth::mock::Mock, root: &Path, listen: std::net::SocketAddr) -> Settings {
        Settings {
            ca_pem: None,
            options: echo_mcp_auth::AuthOptions {
                resource_url: format!("{}/mcp", mock.base),
                client_id: "synthetic-cli".into(),
                scopes: vec!["endpoint:read".into()],
                storage_dir: root.join("grant"),
                request_timeout: Duration::from_secs(5),
            },
            listen,
            pairing_file: root.join("pair"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn t3_portos_link_registers_default_client() {
        super::super::bridge_component::drive(async {
            let mock = echo_mcp_auth::mock::Mock::bind().await.unwrap();
            let mut metadata = echo_mcp_auth::mock::metadata(&mock.base);
            metadata["registration_endpoint"] = format!("{}/custom/register", mock.base).into();
            {
                let mut state = mock.state.lock().unwrap();
                state.metadata = Some(metadata);
                state.fault = Some((
                    "/custom/register".into(),
                    201,
                    serde_json::json!({"client_id":"dcr_synthetic"}),
                ));
            }
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            let mut config =
                BridgeConfig::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/bridge.echo.toml")).unwrap();
            config.codex.ca_file = None;
            config.mcp.client_id = echo_config::McpConfig::default().client_id;
            config.mcp.state_dir = Some(root.join("grant"));
            config.mcp.operator_listen = ([127, 0, 0, 1], 0).into();
            config.mcp.server[0].url = format!("{}/mcp", mock.base);
            config.screen.pairing_token_file = root.join("pair");
            std::fs::write(&config.screen.pairing_token_file, "synthetic-owner").unwrap();
            let trace = crate::bridge_trace::Trace::open(&root.join("trace"), "api_key").unwrap();
            let mut owner = super::super::Runtime::open(
                Settings::configured(&config).unwrap().unwrap(),
                super::super::Presentation::new(None),
                trace,
                None,
            )
            .await
            .unwrap();
            config.mcp.operator_listen = owner.address;
            let prepared = Prepared {
                settings: Settings::configured(&config).unwrap().unwrap(),
                operation: Operation::Link,
            };
            let (send, mut flushed) = tokio::sync::mpsc::unbounded_channel();
            let mut output = Probe(send, Vec::new());
            let mut operation = Box::pin(prepared.execute(&mut output));
            let prefix = format!("Verification URL: {}/approve\nUser code: TEST-CODE\n", mock.base);
            assert_guidance(operation.as_mut(), &mut flushed, &prefix).await;
            let registrations = mock
                .state
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|r| r.path == "/custom/register")
                .count();
            assert_eq!(registrations, 1, "first default link must POST one registration");
            tokio::time::advance(Duration::from_secs(5)).await;
            assert_eq!(operation.await, Ok(()));
            owner.close().await;
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn t3_portos_link_flushes_fresh_code_before_approval_and_refuses_competing_cli() {
        super::super::bridge_component::drive(async {
            for failure in [None, Some("access_denied"), Some("expired_token")] {
                link_outcome(failure).await;
            }
        })
        .await;
    }

    async fn link_outcome(failure: Option<&str>) {
        use echo_screen_contract::UiState;
        let mock = echo_mcp_auth::mock::Mock::bind().await.unwrap();
        if let Some(error) = failure {
            mock.state.lock().unwrap().polling.push_back(error.into());
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let (mut owner, mut screen) = owner(&mock, root).await;
        owner
            .grant
            .presentation
            .overlay(Some(UiState::LinkCode {
                user_code: "STALE".into(),
                verification_uri: "http://127.0.0.1/stale".into(),
                expires_at_unix: 1,
            }))
            .unwrap();
        assert!(screen.recv().await.is_some());
        let prepared = Prepared {
            settings: settings(&mock, root, owner.address),
            operation: Operation::Link,
        };
        let (send, mut flushed) = tokio::sync::mpsc::unbounded_channel();
        let mut output = Probe(send, Vec::new());
        let mut operation = Box::pin(prepared.execute(&mut output));
        let prefix = format!("Verification URL: {}/approve\nUser code: TEST-CODE\n", mock.base);
        assert_guidance(operation.as_mut(), &mut flushed, &prefix).await;
        assert_pending(&mock, root, &owner, screen.recv().await).await;
        tokio::time::advance(Duration::from_secs(5)).await;
        let result = operation.await;
        assert_outcome(result, failure, &output.1, &prefix, &owner, &mut screen).await;
        {
            let state = mock.state.lock().unwrap();
            assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/link").count(), 1);
            assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/token").count(), 1);
        }
        owner.close().await;
    }

    async fn assert_guidance(
        operation: std::pin::Pin<&mut impl Future<Output = Result<(), Error>>>,
        flushed: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
        prefix: &str,
    ) {
        let pending = tokio::select! {
            result = operation => Err(result),
            value = flushed.recv() => Ok(value),
        };
        assert_eq!(pending, Ok(Some(prefix.as_bytes().to_vec())));
    }

    async fn assert_pending(
        mock: &echo_mcp_auth::mock::Mock,
        root: &std::path::Path,
        owner: &super::super::Runtime,
        state: Option<echo_screen_contract::UiState>,
    ) {
        assert!(
            matches!(state, Some(echo_screen_contract::UiState::LinkCode { user_code, verification_uri, .. })
            if user_code == "TEST-CODE" && verification_uri == format!("{}/approve", mock.base))
        );
        assert!(!owner.grant.client.has_grant());
        let competing = Prepared {
            settings: settings(mock, root, owner.address),
            operation: Operation::Link,
        };
        let mut refused = Vec::new();
        assert_eq!(competing.execute(&mut refused).await, Err(Error::Control));
        assert_eq!(refused, b"");
    }

    async fn assert_outcome(
        result: Result<(), Error>,
        failure: Option<&str>,
        output: &[u8],
        prefix: &str,
        owner: &super::super::Runtime,
        screen: &mut tokio::sync::mpsc::Receiver<echo_screen_contract::UiState>,
    ) {
        use echo_screen_contract::UiState;
        match failure {
            None => {
                assert_eq!(result, Ok(()));
                assert_eq!(output, format!("{prefix}MCP linked\n").as_bytes());
                assert!(owner.grant.client.has_grant());
                assert!(matches!(screen.recv().await.unwrap(), UiState::Ready { .. }));
            }
            Some(error) => {
                let expected = if error == "access_denied" {
                    AuthError::AccessDenied
                } else {
                    AuthError::Expired
                };
                assert_eq!(result, Err(Error::Mcp(expected)));
                assert_eq!(output, prefix.as_bytes());
                assert!(!owner.grant.client.has_grant());
                assert!(matches!(screen.recv().await.unwrap(), UiState::Error { .. }));
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn t3_portos_link_cancellation_clears_code_and_releases_owner() {
        super::super::bridge_component::drive(async {
            let mock = echo_mcp_auth::mock::Mock::bind().await.unwrap();
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            let (mut owner, mut screen) = owner(&mock, root).await;
            let prepared = Prepared { settings: settings(&mock, root, owner.address), operation: Operation::Link };
            let (send, mut flushed) = tokio::sync::mpsc::unbounded_channel();
            let mut output = Probe(send, Vec::new());
            let mut operation = Box::pin(prepared.execute(&mut output));
            tokio::select! {
                result = &mut operation => panic!("completed before approval: {result:?}"),
                value = flushed.recv() => assert_eq!(value.unwrap(), format!("Verification URL: {}/approve\nUser code: TEST-CODE\n", mock.base).as_bytes()),
            }
            screen.recv().await.unwrap();
            drop(operation);
            assert!(matches!(screen.recv().await.unwrap(), echo_screen_contract::UiState::Error {relink: true, ..}));
            assert!(!owner.grant.client.has_grant());
            let unlink = Prepared { settings: settings(&mock, root, owner.address), operation: Operation::Unlink };
            let mut completion = Vec::new();
            assert_eq!(unlink.execute(&mut completion).await, Ok(()));
            assert_eq!(completion, b"MCP unlinked\n");
            owner.close().await;
        }).await;
    }

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

    #[tokio::test(start_paused = true)]
    async fn t3_portos_link_invalid_pairing_and_unavailable_owner_print_nothing() {
        super::super::bridge_component::drive(async {
            let mock = echo_mcp_auth::mock::Mock::bind().await.unwrap();
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            let (mut owner, _) = owner(&mock, root).await;
            std::fs::write(root.join("pair"), "wrong-owner").unwrap();
            for stopped in [false, true] {
                if stopped {
                    owner.close().await;
                }
                let command = Prepared {
                    settings: settings(&mock, root, owner.address),
                    operation: Operation::Link,
                };
                let mut output = Vec::new();
                assert_eq!(command.execute(&mut output).await, Err(Error::Control));
                assert_eq!(output.as_slice(), b"");
            }
            assert_eq!(mock.state.lock().unwrap().requests.len(), 2); // discovery only
        })
        .await;
    }

    #[test]
    fn link_output_requires_fresh_code_and_never_turns_an_error_into_success() {
        use serde_json::json;
        let mut output = Vec::new();
        let mut shown = false;
        assert_eq!(
            present(&json!({"outcome":"linked"}), Operation::Link, &mut output, &mut shown),
            Err(Error::Control)
        );
        assert_eq!(output.as_slice(), b"");
        for category in ["access_denied", "expired", "untrusted text"] {
            assert!(present(&json!({"error":category}), Operation::Link, &mut output, &mut shown).is_err());
            assert_eq!(output.as_slice(), b"");
        }
        let state = json!({"state":{"state":"link_code","user_code":"CODE\nMCP linked","verification_uri":"http://127.0.0.1","expires_at_unix":1}});
        assert_eq!(
            present(&state, Operation::Link, &mut output, &mut shown),
            Err(Error::Control)
        );
        assert_eq!(output.as_slice(), b"");
    }
}
