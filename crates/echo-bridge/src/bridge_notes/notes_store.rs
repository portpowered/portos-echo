use super::{
    Receipt, TOTAL_BYTES, TOTAL_FILES, notes_files as files,
    notes_paths::{self, Request},
};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{self, AtFlags, Dir, FileType},
};
use serde_json::json;
use std::{path::Path, sync::Mutex};

// One process owns the agent home. Serial accounting covers reopened providers
// as well as cloned routers, and ends before any async suspension point.
static WRITER: Mutex<()> = Mutex::new(());

struct Entry {
    path: String,
    bytes: u64,
    modified: u64,
}

pub(super) fn execute(home: &Path, request: Request<'_>) -> Result<Receipt, &'static str> {
    let _guard = WRITER.lock().map_err(|_| "io")?;
    let write = matches!(request, Request::Write(..));
    let Some(root) = files::root(home, write)? else {
        return match request {
            Request::List(path) => Ok(listed(vec![], path)),
            _ => Err("not_found"),
        };
    };
    match request {
        Request::List(path) => {
            let mut entries = vec![];
            scan(&root, "", &mut entries)?;
            entries.retain(|entry| path.is_none_or(|prefix| entry.path.starts_with(&format!("{prefix}/"))));
            Ok(listed(entries, path))
        }
        Request::Read(path) => {
            let (parent, name) = parent(&root, path, false)?;
            let content = files::read(&parent, name)?;
            Ok(Receipt {
                bytes: content.len() as u64,
                path: Some(path.into()),
                content: json!({"path":path,"bytes":content.len(),"content":content}),
            })
        }
        Request::Write(path, content) => write_note(&root, path, content),
    }
}

fn parent<'a>(root: &OwnedFd, path: &'a str, create: bool) -> Result<(OwnedFd, &'a str), &'static str> {
    let mut fd = rustix::io::dup(root).map_err(files::error)?;
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return Ok((fd, part));
        }
        fd = files::directory(&fd, std::ffi::OsStr::new(part), create)?.ok_or("not_found")?;
    }
    Err("invalid_path")
}

fn write_note(root: &OwnedFd, path: &str, content: &str) -> Result<Receipt, &'static str> {
    let mut entries = vec![];
    scan(root, "", &mut entries)?;
    let previous = entries.iter().find(|entry| entry.path == path);
    if entries.len() + usize::from(previous.is_none()) > TOTAL_FILES {
        return Err("notes_files_limit");
    }
    let total: u64 = entries.iter().map(|entry| entry.bytes).sum();
    if total - previous.map_or(0, |entry| entry.bytes) + content.len() as u64 > TOTAL_BYTES {
        return Err("notes_bytes_limit");
    }
    let (parent, name) = parent(root, path, true)?;
    files::replace(&parent, name, content.as_bytes(), false)?;
    Ok(Receipt {
        bytes: content.len() as u64,
        path: Some(path.into()),
        content: json!({"path":path,"bytes":content.len()}),
    })
}

fn listed(mut entries: Vec<Entry>, directory: Option<&str>) -> Receipt {
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Receipt {
        bytes: entries.iter().map(|entry| entry.bytes).sum(),
        path: directory.map(str::to_owned),
        content: json!({"files":entries.iter().map(|entry| json!({"path":entry.path,"bytes":entry.bytes,"modified_unix_ms":entry.modified})).collect::<Vec<_>>()}),
    }
}

fn scan(fd: &impl AsFd, prefix: &str, entries: &mut Vec<Entry>) -> Result<(), &'static str> {
    let dir = Dir::read_from(fd).map_err(files::error)?;
    for entry in dir {
        let entry = entry.map_err(files::error)?;
        let name = entry.file_name().to_str().map_err(|_| "invalid_path")?;
        if matches!(name, "." | "..") || files::recover_staging(fd, name)? {
            continue;
        }
        let stat = fs::statat(fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(files::error)?;
        let path = format!("{prefix}{name}");
        if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
            notes_paths::validate(&path, false)?;
            let child = files::directory(fd, std::ffi::OsStr::new(name), false)?.ok_or("io")?;
            scan(&child, &format!("{path}/"), entries)?;
        } else {
            let bytes = files::regular(&stat)?;
            notes_paths::validate(&path, true)?;
            if bytes > super::NOTE_BYTES {
                return Err("note_too_large");
            }
            if entries.len() == TOTAL_FILES {
                return Err("notes_files_limit");
            }
            let nanos = u128::from(stat.st_mtime_nsec);
            let subsecond = u64::try_from(nanos / 1_000_000).map_err(|_| "io")?;
            let modified = u64::try_from(stat.st_mtime)
                .map_err(|_| "io")?
                .checked_mul(1000)
                .and_then(|ms| ms.checked_add(subsecond))
                .ok_or("io")?;
            entries.push(Entry { path, bytes, modified });
        }
    }
    if entries.iter().map(|entry| entry.bytes).sum::<u64>() > TOTAL_BYTES {
        return Err("notes_bytes_limit");
    }
    Ok(())
}
