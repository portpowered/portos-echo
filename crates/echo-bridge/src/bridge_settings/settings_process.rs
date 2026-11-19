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

impl Input {
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
    static INPUT: OnceLock<Result<Input, Error>> = OnceLock::new();
    INPUT
        .get_or_init(|| Input::open(std::io::stdin()))
        .as_ref()
        .map(Input::subscribe)
        .map_err(|_| Error::Control)
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
