//! Explicit fake-time driver for guarded loopback runs only.
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
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

/// Run the normal bridge owner with an explicitly advanced Tokio clock.
/// # Errors
/// Rejects malformed controls, I/O failures and the normal runtime categories.
pub fn execute(settings: Settings) -> Result<(), Error> {
    if !settings.sim_clock || settings.model_url.is_none() || settings.lab {
        return Err(Error::Input);
    }
    let mut commands = input()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .map_err(|_| Error::Control)?;
    runtime.block_on(drive(async move {
        let (stop, shutdown) = oneshot::channel();
        let connector = echo_wyoming::adapter::connector(settings.wyoming, echo_wyoming::ClientOptions::default());
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
                    Some(Ok(Some(milliseconds))) => advance(milliseconds).await?,
                    Some(Ok(None)) => { let _ = stop.send(true); return work.await; }
                    Some(Err(error)) => { let _ = stop.send(false); let _ = work.await; return Err(error); }
                    None => return Err(Error::Control),
                }
            }
        }
    }))
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
    let value = json!({"event":"bridge.clock_advanced", "milliseconds":milliseconds});
    writeln!(std::io::stdout(), "{value}")
        .and_then(|()| std::io::stdout().flush())
        .map_err(|_| Error::Control)
}

fn input() -> Result<mpsc::Receiver<Result<Option<u64>, Error>>, Error> {
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

fn parse(bytes: &[u8]) -> Result<u64, Error> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Error::Control)?;
    if value.as_object().is_none_or(|object| object.len() != 2) || value["action"] != "advance_clock" {
        return Err(Error::Control);
    }
    value["milliseconds"].as_u64().ok_or(Error::Control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_link_sim_clock_accepts_only_bounded_complete_unsigned_controls() {
        assert_eq!(parse(br#"{"action":"advance_clock","milliseconds":5000}"#), Ok(5000));
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
