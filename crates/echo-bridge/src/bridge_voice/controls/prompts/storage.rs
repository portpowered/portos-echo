//! Canonical prompt IO for tools and settings callers. One serialized commit
//! archives the old disk version before publishing, then bounds shared history.
use crate::bridge_notes::notes_files as files;
use rustix::{
    fd::AsFd,
    fs::{self, AtFlags, Dir},
};
use std::{path::Path, sync::Mutex, time::SystemTime};

static WRITER: Mutex<()> = Mutex::new(());
type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Voice,
    Runner,
}

impl Kind {
    pub(super) fn parse(which: &str) -> Result<Self> {
        match which {
            "voice" => Ok(Self::Voice),
            "runner" => Ok(Self::Runner),
            _ => Err("invalid_arguments"),
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Runner => "runner",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Voice => "voice.md",
            Self::Runner => "runner.md",
        }
    }
}

pub(super) fn validate(text: &str) -> Result<()> {
    if text.len() > 32_768 {
        Err("oversized")
    } else if text.trim().is_empty() {
        Err("empty")
    } else {
        Ok(())
    }
}

pub(super) fn read(home: &Path, kind: Kind) -> Result<Option<String>> {
    let _guard = WRITER.lock().map_err(|_| "io")?;
    let Some(dir) = files::root_named(home, "prompts", false)? else {
        return Ok(None);
    };
    read_current(&dir, kind)
}

fn read_current(dir: &impl AsFd, kind: Kind) -> Result<Option<String>> {
    if files::existing(dir, kind.file())?.is_none() {
        return Ok(None);
    }
    let bytes = files::read_limited(dir, kind.file(), 32_768).map_err(|code| match code {
        "note_too_large" => "oversized",
        other => other,
    })?;
    let text = String::from_utf8(bytes).map_err(|_| "invalid_utf8")?;
    validate(&text)?;
    Ok(Some(text))
}

pub(super) fn write(home: &Path, kind: Kind, content: &str) -> Result<()> {
    commit(home, kind, content, SystemTime::now(), Fault::None)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    #[cfg(test)]
    Archive,
    #[cfg(test)]
    Publish,
    #[cfg(test)]
    Prune,
}

fn commit(home: &Path, kind: Kind, content: &str, now: SystemTime, fault: Fault) -> Result<()> {
    validate(content).map_err(|_| "invalid_prompt")?;
    let _guard = WRITER.lock().map_err(|_| "io")?;
    let dir = files::root_named(home, "prompts", true)?.ok_or("io")?;
    let old = read_current(&dir, kind)?;
    // Check history even on a first write; never follow a replaced directory.
    let history = files::directory(&dir, std::ffi::OsStr::new(".history"), old.is_some())?;
    if let Some(history) = &history {
        let entries = history_entries(history)?;
        if let Some(old) = old {
            archive(history, kind, &old, now, &entries, fault)?;
        }
    }
    let fail = publish_fault(fault);
    files::replace(&dir, kind.file(), content.as_bytes(), fail)?;
    if let Some(history) = history {
        #[cfg(test)]
        if fault == Fault::Prune {
            return Err("history");
        }
        prune(&history)?;
    }
    Ok(())
}

fn archive(
    dir: &impl AsFd,
    kind: Kind,
    old: &str,
    now: SystemTime,
    entries: &[(u128, String)],
    fault: Fault,
) -> Result<()> {
    let current = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "history")?
        .as_nanos();
    let last = entries.iter().map(|(stamp, _)| *stamp).max().unwrap_or(0);
    let stamp = current.max(last.checked_add(1).ok_or("history")?);
    let name = format!("{}-{stamp}.md", kind.name());
    let fail = archive_fault(fault);
    files::replace(dir, &name, old.as_bytes(), fail).map_err(|_| "history")
}

fn history_entries(dir: &impl AsFd) -> Result<Vec<(u128, String)>> {
    let mut entries = Vec::new();
    for entry in Dir::read_from(dir).map_err(|_| "history")? {
        let entry = entry.map_err(|_| "history")?;
        let name = entry.file_name().to_str().map_err(|_| "history")?;
        if matches!(name, "." | "..") {
            continue;
        }
        let stamp = name
            .strip_prefix("voice-")
            .or_else(|| name.strip_prefix("runner-"))
            .and_then(|name| name.strip_suffix(".md"))
            .filter(|stamp| !stamp.is_empty() && stamp.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|stamp| stamp.parse().ok())
            .ok_or("history")?;
        let stat = files::existing(dir, name)?.ok_or("history")?;
        if files::regular(&stat)? > 32_768 {
            return Err("history");
        }
        entries.push((stamp, name.into()));
    }
    Ok(entries)
}

fn prune(dir: &impl AsFd) -> Result<()> {
    let mut entries = history_entries(dir)?;
    entries.sort();
    let remove = entries.len().saturating_sub(20);
    for (_, name) in entries.into_iter().take(remove) {
        fs::unlinkat(dir, name.as_str(), AtFlags::empty()).map_err(|_| "history")?;
    }
    fs::fsync(dir).map_err(|_| "history")
}

fn publish_fault(fault: Fault) -> bool {
    #[cfg(test)]
    {
        fault == Fault::Publish
    }
    #[cfg(not(test))]
    {
        let _ = fault;
        false
    }
}

fn archive_fault(fault: Fault) -> bool {
    #[cfg(test)]
    {
        fault == Fault::Archive
    }
    #[cfg(not(test))]
    {
        let _ = fault;
        false
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
