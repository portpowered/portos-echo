//! Safe descriptor-relative Linux IO, reused by all notes operations.
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{self, AtFlags, FileType, Mode, OFlags, Stat},
    io::Errno,
};
use std::{
    io::{Read, Write},
    path::{Component, Path},
    sync::atomic::{AtomicU64, Ordering},
};

type Result<T> = std::result::Result<T, &'static str>;
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn error(error: Errno) -> &'static str {
    match error {
        Errno::LOOP => "symlink",
        Errno::NOENT => "not_found",
        _ => "io",
    }
}

/// Walk even home ancestors without following a link. Never create anything
/// until a validated write needs it; reads/list of missing notes stay inert.
pub(super) fn root(home: &Path, create: bool) -> Result<Option<OwnedFd>> {
    root_named(home, "notes", create)
}

pub(crate) fn root_named(home: &Path, child: &str, create: bool) -> Result<Option<OwnedFd>> {
    if !home.is_absolute() {
        return Err("io");
    }
    let mut fd = fs::open("/", DIRECTORY, Mode::empty()).map_err(error)?;
    for part in home.components() {
        match part {
            Component::RootDir => (),
            Component::Normal(name) => match directory(&fd, name, create)? {
                Some(child) => fd = child,
                None => return Ok(None),
            },
            _ => return Err("io"),
        }
    }
    directory(&fd, std::ffi::OsStr::new(child), create)
}

pub(crate) fn directory(fd: &impl AsFd, name: &std::ffi::OsStr, create: bool) -> Result<Option<OwnedFd>> {
    match fs::statat(fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
            FileType::Symlink => return Err("symlink"),
            FileType::Directory => (),
            _ => return Err("not_regular"),
        },
        Err(Errno::NOENT) if create => match fs::mkdirat(fd, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
            Ok(()) | Err(Errno::EXIST) => (),
            Err(failure) => return Err(error(failure)),
        },
        Err(Errno::NOENT) => return Ok(None),
        Err(failure) => return Err(error(failure)),
    }
    fs::openat(fd, name, DIRECTORY, Mode::empty()).map(Some).map_err(error)
}

pub(crate) fn regular(stat: &Stat) -> Result<u64> {
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::Symlink => Err("symlink"),
        FileType::RegularFile if stat.st_nlink == 1 => u64::try_from(stat.st_size).map_err(|_| "io"),
        _ => Err("not_regular"),
    }
}

pub(crate) fn existing(fd: &impl AsFd, name: &str) -> Result<Option<Stat>> {
    match fs::statat(fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            regular(&stat)?;
            Ok(Some(stat))
        }
        Err(Errno::NOENT) => Ok(None),
        Err(failure) => Err(error(failure)),
    }
}

pub(crate) fn read(fd: &impl AsFd, name: &str) -> Result<String> {
    String::from_utf8(read_limited(fd, name, super::NOTE_BYTES)?).map_err(|_| "io")
}

pub(crate) fn read_limited(fd: &impl AsFd, name: &str, limit: u64) -> Result<Vec<u8>> {
    existing(fd, name)?.ok_or("not_found")?;
    let file = fs::openat(
        fd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(error)?;
    if regular(&fs::fstat(&file).map_err(error)?)? > limit {
        return Err("note_too_large");
    }
    let mut bytes = Vec::new();
    std::fs::File::from(file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "io")?;
    if bytes.len() as u64 > limit {
        return Err("note_too_large");
    }
    Ok(bytes)
}

/// The home has one process owner; callers hold the directory's writer lock.
/// Reconcile only our reserved staging grammar and private, singly linked files.
/// Lookalikes and unsafe entries remain untouched and fail closed.
pub(crate) fn recover_staging(fd: &impl AsFd, name: &str) -> Result<bool> {
    let Some(suffix) = name.strip_prefix(".note-tmp-") else {
        return Ok(false);
    };
    let Some((pid, sequence)) = suffix.split_once('-') else {
        return Ok(false);
    };
    let valid_pid = pid
        .parse::<u32>()
        .is_ok_and(|value| value > 0 && value.to_string() == pid);
    let valid_sequence = sequence.parse::<u64>().is_ok_and(|value| value.to_string() == sequence);
    if !valid_pid || !valid_sequence {
        return Ok(false);
    }
    let stat = fs::statat(fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
    let owner = fs::fstat(fd).map_err(error)?.st_uid;
    if regular(&stat)? > super::NOTE_BYTES || stat.st_uid != owner || stat.st_mode & 0o7777 != 0o600 {
        return Err("io");
    }
    fs::unlinkat(fd, name, AtFlags::empty()).map_err(error)?;
    fs::fsync(fd).map_err(error)?;
    Ok(true)
}

pub(crate) fn recover_directory(fd: &impl AsFd) -> Result<()> {
    for entry in fs::Dir::read_from(fd).map_err(error)? {
        let entry = entry.map_err(error)?;
        if let Ok(name) = entry.file_name().to_str() {
            recover_staging(fd, name)?;
        }
    }
    Ok(())
}

/// Exclusive staging never opens a pre-existing file, even under a hostile
/// umask or competing writer. Dropping before rename cleans only our temp.
pub(crate) fn replace(fd: &impl AsFd, name: &str, content: &[u8], fail: bool) -> Result<()> {
    let staging = format!(
        ".note-tmp-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let file = fs::openat(
        fd,
        staging.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(error)?;
    let staged = Staged {
        parent: fd,
        name: &staging,
    };
    fs::fchmod(&file, Mode::RUSR | Mode::WUSR).map_err(error)?;
    let mut file = std::fs::File::from(file);
    file.write_all(content)
        .and_then(|()| file.sync_all())
        .map_err(|_| "io")?;
    if fail {
        return Err("io");
    }
    existing(fd, name)?;
    fs::renameat(fd, staging.as_str(), fd, name).map_err(error)?;
    drop(staged);
    fs::fsync(fd).map_err(error)
}

struct Staged<'a, F: AsFd> {
    parent: &'a F,
    name: &'a str,
}
impl<F: AsFd> Drop for Staged<'_, F> {
    fn drop(&mut self) {
        let _ = fs::unlinkat(self.parent, self.name, AtFlags::empty());
    }
}
