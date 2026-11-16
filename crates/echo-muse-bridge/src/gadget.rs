use std::{future::Future, path::PathBuf, process::Stdio};

use serde::{Deserialize, Serialize};
use tokio::{io::AsyncReadExt, process::Command};
use uuid::Uuid;

/// A dedicated UUID per task, reused only for that task's cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SideChat(Uuid);

impl TryFrom<String> for SideChat {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let uuid = Uuid::parse_str(&value).map_err(|error| error.to_string())?;
        if uuid.to_string() != value {
            return Err("side chat must be a canonical UUID".into());
        }
        Ok(Self(uuid))
    }
}

impl From<SideChat> for String {
    fn from(value: SideChat) -> Self {
        value.to_string()
    }
}

impl SideChat {
    /// Create an independent v4 UUID using the operating system entropy source.
    /// # Errors
    /// Returns an entropy error without creating an identity if randomness fails.
    pub fn fresh() -> Result<Self, GadgetError> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| GadgetError::Entropy)?;
        Ok(Self(uuid::Builder::from_random_bytes(bytes).into_uuid()))
    }
}

impl std::fmt::Display for SideChat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reply {
    pub text: String,
    pub complete: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum GadgetError {
    #[error("wait must be between 1 and 600 seconds")]
    Wait,
    #[error("trust store unreadable")]
    TrustStore,
    #[error("gadget exited {code}: {reason}")]
    Exit { code: i32, reason: String },
    #[error("gadget process I/O failed")]
    Io(#[from] std::io::Error),
    #[error("gadget terminated without an exit code")]
    Signal,
    #[error("gadget output is not UTF-8")]
    Encoding,
    #[error("identity entropy unavailable")]
    Entropy,
}

pub trait Gadget: Send + Sync {
    fn send_user_msg(
        &self,
        session: SideChat,
        text: &str,
        wait: u64,
    ) -> impl Future<Output = Result<Reply, GadgetError>> + Send;
}

/// No shell, retries, or interpretation of conversational text.
pub struct CliGadget {
    pub program: PathBuf,
}

impl Gadget for CliGadget {
    async fn send_user_msg(&self, session: SideChat, text: &str, wait: u64) -> Result<Reply, GadgetError> {
        if !(1..=600).contains(&wait) {
            return Err(GadgetError::Wait);
        }
        let mut child = Command::new(&self.program)
            .args([
                "send-user-msg",
                "--session-id",
                &session.to_string(),
                "--wait",
                &wait.to_string(),
                text,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let output = bounded_output(&mut child).await?;
        match output.status.code().ok_or(GadgetError::Signal)? {
            code @ (0 | 3) => Ok(Reply {
                text: String::from_utf8(output.stdout).map_err(|_| GadgetError::Encoding)?,
                complete: code == 0,
            }),
            78 => Err(GadgetError::TrustStore),
            code => Err(GadgetError::Exit {
                code,
                reason: String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .last()
                    .unwrap_or_default()
                    .into(),
            }),
        }
    }
}

async fn bounded_output(child: &mut tokio::process::Child) -> Result<std::process::Output, GadgetError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("missing stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("missing stderr"))?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut stdout = stdout.take(98305);
    let mut stderr = stderr.take(98305);
    let operation = async {
        let (status, _, _) =
            tokio::try_join!(child.wait(), stdout.read_to_end(&mut out), stderr.read_to_end(&mut err))?;
        if out.len() > 98304 || err.len() > 98304 {
            return Err(std::io::Error::other("gadget output limit"));
        }
        Ok(std::process::Output {
            status,
            stdout: out,
            stderr: err,
        })
    };
    if let Ok(Ok(output)) = tokio::time::timeout(std::time::Duration::from_secs(2), operation).await {
        Ok(output)
    } else {
        let _killed = child.kill().await;
        let _reaped = child.wait().await;
        Err(std::io::Error::other("gadget transport failed").into())
    }
}
