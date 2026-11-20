//! App-server account commands. The host owns process lifetime and screen lifetime.
//! No credential discovery, copying, refresh implementation or provider HTTP here.
use crate::{OperatorCommand, signin_commands::output};
use echo_codex::{
    codex_io::RpcIo,
    codex_protocol::{Account, AccountRead, AccountReadResult, LoginParams, Message},
    codex_rpc::RpcError,
};
use echo_config::LiveMode;
use echo_contracts::secret::Secret;
use serde_json::{Value, json};
use std::io::Write;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Instant, timeout_at},
};

/// Categories never retain provider error bodies, paths, login IDs or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    #[error("authorization")]
    Authorization,
    #[error("account response invalid")]
    InvalidResponse,
    #[error("account deadline")]
    Deadline,
    #[error("command output failed")]
    Output,
    #[error("command unavailable in this credential mode")]
    UnsupportedMode,
    #[error("use signin with the configured Codex home")]
    ImportUnsupported,
    #[error("account transport unavailable")]
    Transport,
}

impl From<RpcError> for AccountError {
    fn from(error: RpcError) -> Self {
        match error {
            RpcError::Remote(_) => Self::Authorization,
            RpcError::Deadline => Self::Deadline,
            // Includes the decoder's payload-free remote discovery failures.
            _ => Self::Transport,
        }
    }
}

/// Operator-visible prompt; only this explicitly public presentation reaches UI.
/// Payload-bearing values deliberately have neither Debug nor Display.
pub struct Prompt {
    pub url: String,
    pub code: Option<String>,
}

impl Prompt {
    #[must_use]
    pub fn message(&self) -> String {
        self.code.as_ref().map_or_else(
            || format!("url: {}\n", self.url),
            |code| format!("code: {code}\nurl: {}\n", self.url),
        )
    }
}

/// Execute on initialized IO exclusively borrowed from the host's supervisor.
/// `present` publishes the same prompt to the existing screen presentation.
/// A fixed absolute deadline includes RPC, prompt output and completion wait.
///
/// # Errors
/// Failure closes this IO generation; the host must await supervisor shutdown.
pub async fn execute<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    io: &mut RpcIo<R, W>,
    mode: LiveMode,
    command: OperatorCommand,
    writer: &mut impl Write,
    present: impl FnOnce(&Prompt) -> Result<(), AccountError>,
    deadline: Instant,
) -> Result<(), AccountError> {
    let operation = async {
        match command {
            OperatorCommand::Status => status(io, mode, writer).await,
            OperatorCommand::Signin { browser } if mode == LiveMode::Chatgpt => {
                signin(io, browser, writer, present).await
            }
            OperatorCommand::Signout if mode == LiveMode::Chatgpt => {
                let id = io.begin_request("account/logout", &())?;
                if io.wait_reply(&id).await? != json!({}) {
                    return Err(AccountError::InvalidResponse);
                }
                Ok(())
            }
            OperatorCommand::ImportCodex { .. } => Err(AccountError::ImportUnsupported),
            _ => Err(AccountError::UnsupportedMode),
        }
    };
    let result = timeout_at(deadline, operation)
        .await
        .unwrap_or(Err(AccountError::Deadline));
    if result.is_err() {
        io.close(RpcError::Disconnected);
    }
    result
}

/// Authenticate only a nominated API key; `ChatGPT` uses the app-server home.
/// The caller loads its nominated file into Secret and never inherits a key.
///
/// # Errors
/// Reject a failed login or an account whose mode does not match the request.
pub async fn authenticate<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    io: &mut RpcIo<R, W>,
    mode: LiveMode,
    api_key: Option<&Secret<String>>,
) -> Result<(), AccountError> {
    if mode == LiveMode::ApiKey {
        let key = api_key.ok_or(AccountError::Authorization)?;
        let id = io.begin_request("account/login/start", &LoginParams::ApiKey { api_key: key })?;
        let result = io.wait_reply(&id).await?;
        if result["type"] != "apiKey" {
            return Err(AccountError::InvalidResponse);
        }
    }
    let account = read_account(io).await?;
    match (mode, account.account) {
        (LiveMode::ApiKey, Some(Account::ApiKey)) | (LiveMode::Chatgpt, Some(Account::Chatgpt { .. })) => Ok(()),
        _ => Err(AccountError::Authorization),
    }
}

async fn read_account<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    io: &mut RpcIo<R, W>,
) -> Result<AccountReadResult, AccountError> {
    let id = io.begin_request(
        "account/read",
        &AccountRead {
            refresh_token: Some(false),
        },
    )?;
    serde_json::from_value(io.wait_reply(&id).await?).map_err(|_| AccountError::InvalidResponse)
}

async fn status<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    io: &mut RpcIo<R, W>,
    mode: LiveMode,
    writer: &mut impl Write,
) -> Result<(), AccountError> {
    let stored = read_account(io).await?;
    let (label, account) = match (mode, stored.account) {
        (LiveMode::ApiKey, None | Some(Account::ApiKey)) => ("api-key", "n/a"),
        (LiveMode::Chatgpt, Some(Account::Chatgpt { .. })) => ("chatgpt", "****"),
        (LiveMode::Chatgpt, None) => return Err(AccountError::Authorization),
        _ => return Err(AccountError::InvalidResponse),
    };
    output(writer, &format!("mode: {label}\naccount: {account}\nexpires: n/a\n")).map_err(|_| AccountError::Output)
}

async fn signin<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    io: &mut RpcIo<R, W>,
    browser: bool,
    writer: &mut impl Write,
    present: impl FnOnce(&Prompt) -> Result<(), AccountError>,
) -> Result<(), AccountError> {
    let kind = if browser { "chatgpt" } else { "chatgptDeviceCode" };
    let id = io.begin_request("account/login/start", &json!({"type": kind}))?;
    let result = io.wait_reply(&id).await?;
    if result["type"] != kind {
        return Err(AccountError::InvalidResponse);
    }
    let login_id = text(&result, "loginId")?;
    let prompt = Prompt {
        url: text(&result, if browser { "authUrl" } else { "verificationUrl" })?.into(),
        code: if browser {
            None
        } else {
            Some(text(&result, "userCode")?.into())
        },
    };
    validate_url(&prompt.url)?;
    output(writer, &prompt.message()).map_err(|_| AccountError::Output)?;
    present(&prompt)?;
    completion(io, login_id).await?;
    output(writer, "signed in\n").map_err(|_| AccountError::Output)
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, AccountError> {
    value[field]
        .as_str()
        .filter(|text| !text.trim().is_empty() && !text.chars().any(char::is_control))
        .ok_or(AccountError::InvalidResponse)
}

fn validate_url(value: &str) -> Result<(), AccountError> {
    let url = reqwest::Url::parse(value).map_err(|_| AccountError::InvalidResponse)?;
    let local = url.host_str().is_some_and(|host| matches!(host, "127.0.0.1" | "[::1]"));
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && local))
    {
        return Err(AccountError::InvalidResponse);
    }
    Ok(())
}

async fn completion<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    io: &mut RpcIo<R, W>,
    login_id: &str,
) -> Result<(), AccountError> {
    loop {
        if let Message::Notification { method, params, .. } = io.next_event().await?
            && method == "account/login/completed"
            && params["loginId"].as_str() == Some(login_id)
        {
            return match params["success"].as_bool() {
                Some(true) => Ok(()),
                Some(false) => Err(AccountError::Authorization),
                None => Err(AccountError::InvalidResponse),
            };
        }
    }
}
