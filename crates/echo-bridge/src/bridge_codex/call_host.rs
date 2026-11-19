//! Authenticate once before handing the single process generation to voice work.
use crate::{Error, bridge_operator::codex_host};
use echo_codex::codex_io::RpcIo;
use echo_config::{CodexConfig, LiveMode};
use echo_contracts::secret::Secret;
use echo_signin::signin_codex::{self, AccountError};
use tokio::{
    process::{ChildStdin, ChildStdout},
    time::{Instant, timeout_at},
};

#[derive(Clone)]
pub struct HostOptions<'a> {
    pub config: &'a CodexConfig,
    pub mode: LiveMode,
    pub api_key: Option<&'a Secret<String>>,
    pub routes: Option<&'a crate::bridge_operator::LocalRoutes>,
    /// Absolute ceiling shared by process, account and device startup.
    pub deadline: Instant,
}

/// Run one authenticated session with exclusive initialized process IO.
/// The caller owns screen/readiness publication, calls and media cleanup inside
/// `session`. It must not publish Connected merely because account admission
/// succeeded. No legacy credential discovery, HTTP call or automatic restart.
///
/// # Errors
/// Admission failures never invoke `session`. Cancellation, callback failure
/// and normal completion all await child reaping before releasing the home.
/// The absolute deadline covers initialization and authentication, not the
/// admitted session's lifetime. A nominated key is used only in API-key mode.
pub async fn with_host<T>(
    config: &CodexConfig,
    mode: LiveMode,
    api_key: Option<&Secret<String>>,
    deadline: Instant,
    cancelled: impl std::future::Future<Output = ()>,
    session: impl AsyncFnOnce(&mut RpcIo<ChildStdout, ChildStdin>) -> Result<T, Error>,
) -> Result<T, Error> {
    with_host_options(
        HostOptions {
            config,
            mode,
            api_key,
            routes: None,
            deadline,
        },
        cancelled,
        session,
    )
    .await
}

/// Authenticate with optional process-local fake routing.
/// # Errors
/// The same admission, session and cleanup errors as `with_host`.
pub async fn with_host_options<T>(
    options: HostOptions<'_>,
    cancelled: impl std::future::Future<Output = ()>,
    session: impl AsyncFnOnce(&mut RpcIo<ChildStdout, ChildStdin>) -> Result<T, Error>,
) -> Result<T, Error> {
    let HostOptions {
        config,
        mode,
        api_key,
        routes,
        deadline,
    } = options;
    tokio::pin!(cancelled);
    let supervisor = tokio::select! {
        biased;
        () = &mut cancelled => return Err(Error::Control),
        result = timeout_at(deadline, async {
            if Instant::now() >= deadline { return Err(Error::Deadline); }
            codex_host::routed_supervisor(config, routes).await
        }) => {
            result.map_err(|_| Error::Deadline)??
        },
    };
    codex_host::run(supervisor, cancelled, deadline, async |supervisor| {
        let io = supervisor.io().map_err(codex_host::process_error)?;
        timeout_at(deadline, signin_codex::authenticate(io, mode, api_key))
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(account_error)?;
        session(io).await
    })
    .await
}

fn account_error(error: AccountError) -> Error {
    match error {
        AccountError::Deadline => Error::Deadline,
        AccountError::Transport => Error::Unavailable,
        _ => Error::Authorization,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn expired_admission_never_prepares_an_owner_and_ready_cancellation_wins() {
        let config = CodexConfig::default();
        let deadline = Instant::now();
        for cancelled in [false, true] {
            let result = with_host(
                &config,
                LiveMode::Chatgpt,
                None,
                deadline,
                async {
                    if !cancelled {
                        std::future::pending::<()>().await;
                    }
                },
                async |_| -> Result<(), Error> { unreachable!() },
            )
            .await;
            assert_eq!(result, Err(if cancelled { Error::Control } else { Error::Deadline }));
        }
    }
}
