use crate::{OperatorCommand, SigninError};
use echo_chatgpt_auth::AuthSource;
use std::{future::pending, io::Write};

pub(super) fn output(writer: &mut impl Write, text: &str) -> Result<(), SigninError> {
    writer
        .write_all(text.as_bytes())
        .and_then(|()| writer.flush())
        .map_err(|_| SigninError::Output)
}

fn redacted(account: &str) -> String {
    let start = account.char_indices().rev().nth(3).map_or(0, |(index, _)| index);
    format!("****{}", &account[start..])
}

pub(super) async fn status(source: Option<&AuthSource>, writer: &mut impl Write) -> Result<(), SigninError> {
    let Some(source) = source else {
        return output(writer, "mode: api-key\naccount: n/a\nexpires: n/a\n");
    };
    let stored = source.status().await?;
    output(
        writer,
        &format!(
            "mode: chatgpt\naccount: {}\nexpires: {}\n",
            redacted(&stored.account_id),
            stored.expires
        ),
    )
}

pub(super) async fn setup(
    source: &AuthSource,
    command: OperatorCommand,
    writer: &mut impl Write,
) -> Result<(), SigninError> {
    match command {
        OperatorCommand::ImportCodex { path } => {
            source.import_codex(&path).await?;
            output(writer, "imported\n")
        }
        OperatorCommand::Signin { browser } => signin(source, browser, writer).await,
        // The public dispatcher handles these before selecting the auth source.
        OperatorCommand::Status | OperatorCommand::Signout | OperatorCommand::KillSwitch { .. } => {
            Err(SigninError::UnsupportedMode)
        }
    }
}

async fn signin(source: &AuthSource, browser: bool, writer: &mut impl Write) -> Result<(), SigninError> {
    let (failed, failure) = tokio::sync::oneshot::channel();
    let operation = async {
        if browser {
            source
                .browser_sign_in(|url| {
                    let _ = failed.send(output(writer, &format!("url: {url}\n")).is_err());
                })
                .await
        } else {
            source
                .device_sign_in(|prompt| {
                    let text = format!("code: {}\nurl: {}\n", prompt.user_code, prompt.verification_url);
                    let _ = failed.send(output(writer, &text).is_err());
                })
                .await
        }
    };
    tokio::select! {
        biased;
        () = async { if !matches!(failure.await, Ok(true)) { pending::<()>().await; } } => {
            return Err(SigninError::Output);
        }
        result = operation => { result?; }
    }
    output(writer, "signed in\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_uses_last_four_characters_with_prefix() {
        for (account, expected) in [
            ("synthetic-account-1234", "****1234"),
            ("abc", "****abc"),
            ("", "****"),
            ("prefix-é水🌻λ", "****é水🌻λ"),
            ("水🌻", "****水🌻"),
        ] {
            assert_eq!(redacted(account), expected);
        }
    }

    #[test]
    fn writer_failures_and_auth_errors_are_sanitized() {
        let mut writer = std::io::Cursor::new([0_u8; 3]);
        assert_eq!(output(&mut writer, "mode: api-key\n"), Err(SigninError::Output));
        assert_eq!(writer.into_inner(), *b"mod");
        assert_eq!(SigninError::Output.to_string(), "command output failed");
        let error = SigninError::from(echo_chatgpt_auth::AuthError::AccountMismatch);
        assert_eq!(error.to_string(), "account mismatch");
    }
}
