//! Explicit fake-time driver for guarded loopback runs only.
//!
//! `advance_playout {milliseconds: u64}` is the opt-in receipt contract.
//! Success emits `bridge.clock_advanced {milliseconds, epoch_ms, pcm, states}`.
//! PCM rows contain `epoch_ms/pcm_bytes/nonzero_samples/audible_samples` after
//! completed writes through the guarded local Wyoming forwarder. State rows
//! contain `epoch_ms/state` at projected publication; the step flushes the screen
//! snapshot before acknowledging. Epoch is paused Tokio time since owner startup.
//! An explicit `--sim-epoch` also anchors model-facing UTC to that clock.
//! Both pre-advance and post-advance drains require a voice-owner acknowledgement,
//! matching submitted/written PCM counts, and a screen publication receipt.
//! Missing/failed acknowledgements, overflow, a missing screen or a five-second
//! wall-clock watchdog expiry fail Control without a success receipt. This proves
//! bridge clock alignment, not socket delivery timing. Independent relay/SSE
//! observations corroborate bytes/order. Legacy `advance_clock` stays compatible.
mod proxy;
pub(crate) mod receipt;
#[cfg(test)]
#[path = "bridge_clock/tests.rs"]
mod receipt_tests;
use crate::{
    Error,
    bridge_input::Settings,
    bridge_runtime::{self, Control},
};
use serde_json::{Value, json};
use std::{
    future::Future,
    io::{Read, Write},
    task::Poll,
    time::{Duration, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot};

/// Run the normal bridge owner with an explicitly advanced Tokio clock.
/// # Errors
/// Rejects malformed controls, I/O failures and the normal runtime categories.
pub fn execute(settings: Settings) -> Result<(), Error> {
    if !settings.sim_clock || settings.model_url.is_none() || settings.lab {
        return Err(Error::Input);
    }
    let epoch = settings
        .sim_epoch
        .map(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)).ok_or(Error::Input))
        .transpose()?;
    let commands = input()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .map_err(|_| Error::Control)?;
    runtime.block_on(async {
        let context = receipt::Context::new();
        let clock = epoch.map(|time| (time, tokio::time::Instant::now()));
        crate::bridge_time::SIM_TIME
            .scope(
                clock,
                receipt::CURRENT.scope(context.clone(), drive(Box::pin(run(settings, commands, context)))),
            )
            .await
    })
}

#[derive(Debug, PartialEq, Eq)]
struct Advance {
    milliseconds: u64,
    observed: bool,
}

async fn run(
    mut settings: Settings,
    mut commands: mpsc::Receiver<Result<Option<Advance>, Error>>,
    context: receipt::Context,
) -> Result<(), Error> {
    let proxy = proxy::Proxy::bind(settings.wyoming.tcp_addr(), context.clone()).await?;
    settings.wyoming = proxy.address.into();
    let (stop, shutdown) = oneshot::channel();
    let connector = crate::bridge_world::connector(settings.wyoming.clone());
    let work = bridge_runtime::execute_with_control(
        settings,
        Control {
            utc_day: std::sync::Arc::new(|| 0),
            shutdown,
        },
        connector,
    );
    tokio::pin!(work);
    loop {
        tokio::select! {
            result = &mut work => return result,
            command = commands.recv() => match command {
                Some(Ok(Some(command))) => tokio::select! {
                    result = &mut work => return result,
                    result = advance_control(command, &context) => result?,
                },
                Some(Ok(None)) => return finish(work, stop, Ok(())).await,
                Some(Err(error)) => return finish(work, stop, Err(error)).await,
                None => return finish(work, stop, Err(Error::Control)).await,
            }
        }
    }
}

async fn finish(
    work: impl Future<Output = Result<(), Error>>,
    stop: oneshot::Sender<bool>,
    control: Result<(), Error>,
) -> Result<(), Error> {
    // No more clock controls can arrive. The always-ready driver prevents
    // paused time from auto-advancing, so teardown deadlines need real time.
    tokio::time::resume();
    let _ = stop.send(control.is_ok());
    let result = work.await;
    control.and(result)
}

async fn drive<F: Future>(future: F) -> F::Output {
    tokio::pin!(future);
    std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
        Poll::Ready(output) => Poll::Ready(output),
        Poll::Pending => {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

async fn advance(milliseconds: u64) -> Result<(), Error> {
    let duration = Duration::from_millis(milliseconds);
    tokio::time::Instant::now()
        .checked_add(duration)
        .ok_or(Error::Control)?;
    tokio::time::advance(duration).await;
    emit(&json!({"event":"bridge.clock_advanced", "milliseconds":milliseconds}))
}

async fn advance_control(command: Advance, context: &receipt::Context) -> Result<(), Error> {
    if !command.observed {
        return advance(command.milliseconds).await;
    }
    let value = receipt::bounded(context.advance(command.milliseconds), Duration::from_secs(5)).await?;
    emit(&value)
}

fn emit(value: &Value) -> Result<(), Error> {
    writeln!(std::io::stdout(), "{value}")
        .and_then(|()| std::io::stdout().flush())
        .map_err(|_| Error::Control)
}

fn input() -> Result<mpsc::Receiver<Result<Option<Advance>, Error>>, Error> {
    let (send, receive) = mpsc::channel(1);
    std::thread::Builder::new()
        .name("bridge-sim-control".into())
        .spawn(move || {
            let mut input = std::io::stdin().lock();
            loop {
                let command = line(&mut input).and_then(|line| line.map(|line| parse(&line)).transpose());
                let done = !matches!(command, Ok(Some(_)));
                if send.blocking_send(command).is_err() || done {
                    break;
                }
            }
        })
        .map_err(|_| Error::Control)?;
    Ok(receive)
}

fn line(input: &mut impl Read) -> Result<Option<Vec<u8>>, Error> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        match input.read(&mut byte).map_err(|_| Error::Control)? {
            0 if bytes.is_empty() => return Ok(None),
            0 => return Err(Error::Control),
            _ if byte[0] == b'\n' => return Ok(Some(bytes)),
            _ if bytes.len() >= 4096 => return Err(Error::Control),
            _ => bytes.push(byte[0]),
        }
    }
}

fn parse(bytes: &[u8]) -> Result<Advance, Error> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Error::Control)?;
    if value.as_object().is_none_or(|object| object.len() != 2)
        || !matches!(value["action"].as_str(), Some("advance_clock" | "advance_playout"))
    {
        return Err(Error::Control);
    }
    Ok(Advance {
        milliseconds: value["milliseconds"].as_u64().ok_or(Error::Control)?,
        observed: value["action"] == "advance_playout",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_link_sim_clock_accepts_only_bounded_complete_unsigned_controls() {
        assert_eq!(
            parse(br#"{"action":"advance_clock","milliseconds":5000}"#),
            Ok(Advance {
                milliseconds: 5000,
                observed: false
            })
        );
        for text in [
            r#"{"action":"advance_clock","milliseconds":-1}"#,
            r#"{"action":"advance_clock","milliseconds":1.5}"#,
            r#"{"action":"unknown","milliseconds":0}"#,
            r#"{"action":"advance_clock","milliseconds":0,"extra":1}"#,
        ] {
            assert_eq!(parse(text.as_bytes()), Err(Error::Control));
        }
        assert_eq!(line(&mut &b""[..]), Ok(None));
        assert_eq!(line(&mut &b"partial"[..]), Err(Error::Control));
        assert_eq!(line(&mut &vec![b'x'; 4097][..]), Err(Error::Control));
    }
}
