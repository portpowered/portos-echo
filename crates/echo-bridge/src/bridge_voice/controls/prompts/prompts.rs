//! Per-runtime prompt state, scoped alongside the configured voice owner.
//! No payloads or nominated paths enter diagnostics or traces.
use crate::{Error, bridge_trace::Trace};
use echo_config::{LIVE_INSTRUCTIONS, RUNNER_INSTRUCTIONS};
use std::{
    cell::RefCell,
    future::Future,
    io::Read,
    path::{Path, PathBuf},
};

tokio::task_local! {
    static CURRENT: RefCell<Option<Prompts>>;
}

struct Prompt {
    kind: &'static str,
    text: String,
    missing: bool,
    reported: bool,
}

struct Prompts {
    dir: PathBuf,
    runner: Prompt,
    voice: Prompt,
}

pub(super) async fn scope<T>(home: Option<&Path>, work: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    let prompts = home.map(Prompts::open).transpose()?;
    CURRENT.scope(RefCell::new(prompts), work).await
}

impl Prompts {
    fn open(home: &Path) -> Result<Self, Error> {
        let dir = home.join("prompts");
        crate::bridge_private::directory(&dir).map_err(|_| Error::Prompt("home.dir: directory unavailable"))?;
        Ok(Self {
            runner: Prompt::open(&dir, "runner")?,
            voice: Prompt::open(&dir, "voice")?,
            dir,
        })
    }
}

impl Prompt {
    fn default_text(kind: &str) -> &'static str {
        if kind == "runner" {
            RUNNER_INSTRUCTIONS
        } else {
            LIVE_INSTRUCTIONS
        }
    }

    fn open(dir: &Path, kind: &'static str) -> Result<Self, Error> {
        let text = read(&dir.join(format!("{kind}.md"))).map_err(|reason| startup_error(kind, reason))?;
        let missing = text.is_none();
        Ok(Self {
            kind,
            text: text.unwrap_or_else(|| Self::default_text(kind).into()),
            missing,
            reported: false,
        })
    }

    fn publish(&mut self, trace: &mut Trace) -> Result<(), Error> {
        if self.missing && !self.reported {
            trace.record("prompt.defaulted", None, serde_json::json!({"prompt": self.kind}))?;
            self.reported = true;
        }
        Ok(())
    }

    fn reload(&mut self, dir: &Path, trace: &mut Trace) -> Result<(), Error> {
        match read(&dir.join(format!("{}.md", self.kind))) {
            Ok(text) => {
                self.missing = text.is_none();
                self.text = text.unwrap_or_else(|| Self::default_text(self.kind).into());
                self.publish(trace)?;
            }
            Err(reason) => trace.record(
                "prompt.reload_refused",
                None,
                serde_json::json!({"prompt": self.kind, "reason": reason}),
            )?,
        }
        Ok(())
    }
}

pub(super) fn publish(trace: &mut Trace) -> Result<(), Error> {
    CURRENT
        .try_with(|current| {
            if let Some(prompts) = current.borrow_mut().as_mut() {
                prompts.runner.publish(trace)?;
                prompts.voice.publish(trace)?;
            }
            Ok(())
        })
        .unwrap_or(Ok(()))
}

pub(super) fn snapshot(runner: &str, voice: &str, trace: &mut Option<Trace>) -> Result<(String, String), Error> {
    CURRENT
        .try_with(|current| {
            let mut current = current.borrow_mut();
            let Some(prompts) = current.as_mut() else {
                return Ok((runner.into(), voice.into()));
            };
            let trace = trace.as_mut().ok_or(Error::Trace)?;
            prompts.runner.reload(&prompts.dir, trace)?;
            prompts.voice.reload(&prompts.dir, trace)?;
            Ok((prompts.runner.text.clone(), prompts.voice.text.clone()))
        })
        .unwrap_or_else(|_| Ok((runner.into(), voice.into())))
}

fn read(path: &Path) -> Result<Option<String>, &'static str> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("unreadable"),
    };
    if !metadata.is_file() {
        return Err("not_regular");
    }
    let file = std::fs::File::open(path).map_err(|_| "unreadable")?;
    let mut bytes = Vec::new();
    file.take(32_769).read_to_end(&mut bytes).map_err(|_| "unreadable")?;
    if bytes.len() > 32_768 {
        return Err("oversized");
    }
    let text = String::from_utf8(bytes).map_err(|_| "invalid_utf8")?;
    if text.trim().is_empty() {
        return Err("empty");
    }
    Ok(Some(text))
}

fn startup_error(kind: &str, reason: &str) -> Error {
    let message = match (kind, reason) {
        ("runner", "empty") => "prompts/runner.md: empty prompt",
        ("runner", "oversized") => "prompts/runner.md: prompt exceeds 32768 bytes",
        ("runner", "invalid_utf8") => "prompts/runner.md: invalid UTF-8",
        ("runner", "not_regular") => "prompts/runner.md: expected regular file",
        ("runner", _) => "prompts/runner.md: unreadable prompt",
        (_, "empty") => "prompts/voice.md: empty prompt",
        (_, "oversized") => "prompts/voice.md: prompt exceeds 32768 bytes",
        (_, "invalid_utf8") => "prompts/voice.md: invalid UTF-8",
        (_, "not_regular") => "prompts/voice.md: expected regular file",
        _ => "prompts/voice.md: unreadable prompt",
    };
    Error::Prompt(message)
}

#[cfg(test)]
mod tests;
