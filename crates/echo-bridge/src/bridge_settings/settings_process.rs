//! Process-generation resources retained across an unsuccessful settings exec.
use crate::Error;
use std::{ffi::OsString, io::Read, process::Command, sync::OnceLock};
use tokio::sync::{oneshot, watch};

#[derive(Clone, Copy)]
enum Stop {
    Waiting,
    Finished(bool),
}

struct Input {
    state: watch::Receiver<Stop>,
}

/// Final process-level decision after asynchronous resource cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacementGate {
    Ready,
    Shutdown,
    Killed,
}

impl Input {
    fn replacement_gate(&self, kill_path: Option<&std::path::Path>) -> Result<ReplacementGate, Error> {
        match *self.state.borrow() {
            Stop::Finished(true) => return Ok(ReplacementGate::Shutdown),
            Stop::Finished(false) => return Err(Error::Control),
            Stop::Waiting => (),
        }
        if kill_path
            .map(crate::bridge_runtime::bridge_kill::inspect)
            .transpose()?
            .unwrap_or(false)
        {
            Ok(ReplacementGate::Killed)
        } else {
            Ok(ReplacementGate::Ready)
        }
    }

    fn open(mut reader: impl Read + Send + 'static) -> Result<Self, Error> {
        let (send, state) = watch::channel(Stop::Waiting);
        // This is the sole blocking reader in this process generation. It owns
        // no runtime resource and successful exec replaces its thread as well.
        std::thread::Builder::new()
            .name("bridge-control".into())
            .spawn(move || {
                send.send_replace(Stop::Finished(matches!(reader.read(&mut [0]), Ok(0))));
            })
            .map_err(|_| Error::Control)?;
        Ok(Self { state })
    }

    fn subscribe(&self) -> oneshot::Receiver<bool> {
        let mut state = self.state.clone();
        let (mut send, receive) = oneshot::channel();
        tokio::spawn(async move {
            tokio::select! {
                () = send.closed() => (),
                clean = wait(&mut state) => { let _ = send.send(clean); },
            }
        });
        receive
    }
}

async fn wait(state: &mut watch::Receiver<Stop>) -> bool {
    loop {
        if let Stop::Finished(clean) = *state.borrow_and_update() {
            return clean;
        }
        if state.changed().await.is_err() {
            return false;
        }
    }
}

/// Subscribe to the original stdin observer, including after a failed exec.
/// Dropping a runtime or receiver never creates a second blocking reader.
///
/// # Errors
/// Returns a fixed control error if the OS reader thread cannot be started.
pub(crate) fn shutdown() -> Result<oneshot::Receiver<bool>, Error> {
    input().map(Input::subscribe)
}

fn input() -> Result<&'static Input, Error> {
    static INPUT: OnceLock<Result<Input, Error>> = OnceLock::new();
    INPUT
        .get_or_init(|| Input::open(std::io::stdin()))
        .as_ref()
        .map_err(|_| Error::Control)
}

/// Recheck held stdin and the durable kill marker after dropping the runtime.
/// This never creates an async task, retries exec, or clears a kill marker.
/// A blocked replacement must remain controllable under the old active config.
///
/// # Errors
/// Unexpected stdin input/read failure or an invalid/inaccessible kill marker
/// refuses replacement with the fixed control category.
pub fn replacement_gate(kill_path: Option<&std::path::Path>) -> Result<ReplacementGate, Error> {
    input()?.replacement_gate(kill_path)
}

/// Original process arguments, captured before any runtime or settings edit.
/// No config value chooses an executable, shell or argument for restart.
pub struct ProcessImage {
    argv: Vec<OsString>,
}

impl ProcessImage {
    #[must_use]
    pub fn capture() -> Self {
        Self {
            argv: std::env::args_os().collect(),
        }
    }

    #[cfg(unix)]
    fn command(&self) -> Result<Command, Error> {
        use std::os::unix::process::CommandExt;
        let (name, args) = self.argv.split_first().ok_or(Error::Control)?;
        // /proc refers to the currently running image even if its installed
        // pathname was replaced. exec inherits fd 0, rather than reopening it.
        let mut command = Command::new("/proc/self/exe");
        command.arg0(name).args(args);
        Ok(command)
    }

    /// Replace this process after all runtime owners have released resources.
    /// Success never returns; failure leaves the process and stdin observer alive.
    /// The caller owns safe-turn admission, shutdown/kill precedence and recovery.
    ///
    /// # Errors
    /// Returns only a fixed error, without OS paths, arguments or environment.
    pub fn replace(&self) -> Result<std::convert::Infallible, Error> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let _ = self.command()?.exec();
        }
        Err(Error::Control)
    }
}

#[cfg(test)]
#[path = "settings_process_tests.rs"]
mod tests;
