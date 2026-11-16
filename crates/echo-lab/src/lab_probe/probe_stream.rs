// Bounded read-only adb stream. Drop closes the channel, kills/reaps and joins readers.
use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

pub(crate) struct Stream {
    child: Child,
    receiver: Option<Receiver<io::Result<Vec<u8>>>>,
    readers: Vec<JoinHandle<()>>,
}

impl Stream {
    pub fn start(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| "calibrate: cannot launch explicit adb")?;
        let stdout = child.stdout.take().ok_or("calibrate: missing child stdout")?;
        let stderr = child.stderr.take().ok_or("calibrate: missing child stderr")?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let output = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                let result = reader.by_ref().take(65_537).read_until(b'\n', &mut bytes);
                match result {
                    Ok(0) => break,
                    Ok(_) if bytes.len() <= 65_536 => {
                        if sender.send(Ok(bytes)).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = sender.send(Err(io::Error::other("invalid adb stream")));
                        break;
                    }
                }
            }
        });
        let errors = std::thread::spawn(move || {
            let _ = io::copy(&mut BufReader::new(stderr), &mut io::sink());
        });
        Ok(Self {
            child,
            receiver: Some(receiver),
            readers: vec![output, errors],
        })
    }

    pub fn next(&self, remaining: Duration, taps: usize) -> Result<Option<Vec<u8>>, String> {
        if remaining.is_zero() {
            return Err(format!("calibrate: timed out after {taps} of 5 taps"));
        }
        match self
            .receiver
            .as_ref()
            .ok_or("calibrate: closed stream")?
            .recv_timeout(remaining)
        {
            Ok(Ok(bytes)) => Ok(Some(bytes)),
            Ok(Err(_)) => Err("calibrate: event stream read failed".into()),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
            Err(RecvTimeoutError::Timeout) => Err(format!("calibrate: timed out after {taps} of 5 taps")),
        }
    }

    pub fn exited_successfully(&mut self, mut remaining: impl FnMut() -> Duration) -> Result<(), String> {
        loop {
            if let Some(status) = self.child.try_wait().map_err(|_| "calibrate: adb wait failed")? {
                return if status.success() {
                    Ok(())
                } else {
                    Err("calibrate: adb read failed".into())
                };
            }
            if remaining().is_zero() {
                return Err("calibrate: timed out after 0 of 5 taps".into());
            }
            std::thread::yield_now();
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        drop(self.receiver.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}
