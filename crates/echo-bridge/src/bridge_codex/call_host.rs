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

tokio::task_local! {
    /// The realtime voices this host generation's app server listed.
    static LISTED: Option<Vec<String>>;
}

/// The voices the running app server accepts for the realtime version the
/// bridge sends (`v3`). No release is pinned, so the list is read from the
/// process itself. `None` when it cannot be read.
async fn listed_voices(io: &mut RpcIo<ChildStdout, ChildStdin>) -> Option<Vec<String>> {
    let request = io
        .begin_request("thread/realtime/listVoices", &serde_json::json!({}))
        .ok()?;
    let result = io.wait_reply(&request).await.ok()?;
    let names = result["voices"]["v3"].as_array()?;
    let names: Vec<String> = names
        .iter()
        .filter_map(|name| name.as_str().map(String::from))
        .collect();
    (!names.is_empty()).then_some(names)
}

/// The voice to send with a realtime start. The app server refuses the whole
/// start for a voice it does not list, after the wake, so such a voice is left
/// out and the server's default speaks instead. Outside a host, and when the
/// list could not be read, the configured voice is sent as it is.
pub(super) fn admitted_voice<'a>(
    voice: Option<&'a str>,
    trace: Option<&mut crate::bridge_trace::Trace>,
) -> Result<(Option<&'a str>, Option<bool>), Error> {
    let catalog = LISTED
        .try_with(|listed| {
            listed.as_ref().map(|names| {
                let contains = voice.is_some_and(|voice| names.iter().any(|name| name == voice));
                (contains, names.len())
            })
        })
        .ok()
        .flatten();
    if let (Some(configured_voice), Some((false, listed_count))) = (voice, catalog) {
        use std::io::Write as _;
        let warning = "codex.voice is not listed by the app server; using its default voice";
        let _ = writeln!(std::io::stderr(), "echo-bridge: {warning}");
        if let Some(trace) = trace {
            trace.record(
                "call.voice_dropped",
                None,
                serde_json::json!({"configured_voice":configured_voice,"listed_count":listed_count}),
            )?;
        }
        return Ok((None, Some(false)));
    }
    Ok((voice, catalog.map(|(contains, _)| contains)))
}

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
        let account = timeout_at(deadline, signin_codex::authenticate(io, mode, api_key))
            .await
            .map_err(|_| Error::Deadline)?;
        crate::bridge_settings::openai_account::observed(account);
        account.map_err(account_error)?;
        let voices = timeout_at(deadline, listed_voices(io)).await.ok().flatten();
        crate::bridge_settings::openai_account::voices_observed(voices.as_deref());
        LISTED.scope(voices, session(io)).await
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
