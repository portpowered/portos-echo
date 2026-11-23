use super::{Endpoint, permissions};
use crate::WifiHandler;
use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::{Mutex, OnceLock},
    time::Duration,
};

const FRAME_LIMIT: usize = 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
static HANDLER: OnceLock<WifiHandler> = OnceLock::new();
static ENDPOINT: Mutex<Option<Endpoint>> = Mutex::new(None);

pub(super) fn select(handler: WifiHandler) {
    let _ = HANDLER.set(handler);
}

pub(super) fn start(path: &Path) {
    if let Some(handler) = HANDLER.get()
        && bind(path, *handler).is_err()
    {
        super::log(format_args!("hassmic: Wi-Fi socket unavailable"));
    }
}

fn bind(path: &Path, handler: WifiHandler) -> io::Result<()> {
    let endpoint = permissions::with_socket_group(path, || Endpoint::bind(path))?;
    let listener = endpoint.listener.try_clone()?;
    let path = path.to_owned();
    let mut owner = ENDPOINT
        .lock()
        .map_err(|_| io::Error::other("Wi-Fi listener owner poisoned"))?;
    std::thread::Builder::new().name("device-wifi".into()).spawn(move || {
        serve(&listener, &path, handler);
    })?;
    *owner = Some(endpoint);
    Ok(())
}

pub(super) fn cleanup() {
    if let Ok(mut owner) = ENDPOINT.lock() {
        drop(owner.take());
    }
}

fn serve(listener: &UnixListener, path: &Path, handler: WifiHandler) {
    for accepted in listener.incoming() {
        match accepted {
            Ok(mut stream) => {
                let _ = exchange(&mut stream, path, handler);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
            Err(_) => break,
        }
    }
}

fn exchange(stream: &mut UnixStream, path: &Path, handler: WifiHandler) -> io::Result<()> {
    let mut reader = BufReader::new(&*stream);
    let deadline = (handler.monotonic)() + IO_TIMEOUT;
    let mut frame = Vec::new();
    let request = loop {
        let left = deadline.saturating_sub((handler.monotonic)());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(left))?;
        let pending = reader.fill_buf()?;
        if pending.is_empty() {
            break &[][..];
        }
        let newline = pending.iter().position(|&byte| byte == b'\n');
        let count = newline.unwrap_or(pending.len());
        if count > FRAME_LIMIT - frame.len() {
            break &[];
        }
        frame.extend_from_slice(&pending[..count]);
        if newline.is_some() {
            break &frame;
        }
        reader.consume(count);
    };
    let reply = (handler.respond)(path, request);
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(&reply)?;
    stream.write_all(b"\n")
}
