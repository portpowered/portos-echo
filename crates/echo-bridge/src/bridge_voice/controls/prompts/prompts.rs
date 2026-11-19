//! Per-runtime prompt state, scoped alongside the configured voice owner.
//! No payloads or nominated paths enter diagnostics or traces.
use crate::{Error, bridge_trace::Trace};
use echo_config::{LIVE_INSTRUCTIONS, RUNNER_INSTRUCTIONS};
use std::{
    cell::RefCell,
    future::Future,
    path::{Path, PathBuf},
};

mod storage;

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
    scope_retained(home, None, work).await
}

/// Recovery starts with accepted text, even if a later external edit is invalid.
/// Normal call admission still reloads disk and refuses invalid replacements.
pub(super) async fn scope_retained<T>(
    home: Option<&Path>,
    retained: Option<echo_screen_contract::settings_v1::PromptTextsV1>,
    work: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    let prompts = match (home, retained) {
        (Some(home), Some(text)) => Some(Prompts {
            dir: home.join("prompts"),
            runner: Prompt::retained("runner", text.runner),
            voice: Prompt::retained("voice", text.voice),
        }),
        (home, None) => home.map(Prompts::open).transpose()?,
        (None, Some(_)) => return Err(Error::Input),
    };
    CURRENT.scope(RefCell::new(prompts), work).await
}

impl Prompts {
    fn open(home: &Path) -> Result<Self, Error> {
        let dir = home.join("prompts");
        crate::bridge_notes::notes_files::root_named(home, "prompts", true)
            .map_err(|_| Error::Prompt("home.dir: directory unavailable"))?;
        Ok(Self {
            runner: Prompt::open(&dir, "runner")?,
            voice: Prompt::open(&dir, "voice")?,
            dir,
        })
    }
}

impl Prompt {
    fn retained(kind: &'static str, text: String) -> Self {
        Self {
            kind,
            text,
            missing: false,
            reported: false,
        }
    }

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

/// The settings snapshot starts from the runtime's accepted cache, never a
/// second filesystem read that could admit an edit after startup validation.
pub(super) fn accepted() -> Result<echo_screen_contract::settings_v1::PromptTextsV1, Error> {
    CURRENT
        .try_with(|current| {
            let current = current.borrow();
            let prompts = current.as_ref().ok_or(Error::Input)?;
            Ok(echo_screen_contract::settings_v1::PromptTextsV1 {
                voice: prompts.voice.text.clone(),
                runner: prompts.runner.text.clone(),
            })
        })
        .unwrap_or(Err(Error::Input))
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
    let home = path.parent().and_then(Path::parent).ok_or("unreadable")?;
    let kind = path.file_stem().and_then(|name| name.to_str()).ok_or("unreadable")?;
    storage::read(home, storage::Kind::parse(kind)?).map_err(|code| match code {
        "symlink" | "io" => "unreadable",
        other => other,
    })
}

/// Read the effective validated base prompt, without an admission clock suffix.
pub(crate) fn read_base(home: &Path, which: &str) -> Result<String, &'static str> {
    let kind = storage::Kind::parse(which)?;
    storage::read(home, kind)
        .map(|text| text.unwrap_or_else(|| Prompt::default_text(which).into()))
        .map_err(prompt_error)
}

/// The shared tool/settings mutation path. No active snapshot is modified.
pub(crate) fn write_base(home: &Path, which: &str, content: &str) -> Result<(), &'static str> {
    storage::write(home, storage::Kind::parse(which)?, content).map_err(prompt_error)
}

fn prompt_error(code: &'static str) -> &'static str {
    match code {
        "empty" | "oversized" | "invalid_utf8" => "invalid_prompt",
        other => other,
    }
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
