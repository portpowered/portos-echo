use std::{
    fmt::Write as _,
    fs::{self, DirBuilder, File, Metadata},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
};

use echo_boot::{Digest, Result, Selection, Slot, layout};
use rustix::fs::{Mode, OFlags, open};
use sha2::{Digest as _, Sha256};

const LOCK: &str = "supervisor.lock";
const TEMP: &str = "selection.new";

pub(super) struct Layout {
    state: PathBuf,
    // Inherited by the role and its descendants. An abrupt supervisor death
    // cannot admit a second instance until the old group releases the lock.
    _lock: File,
}

impl Layout {
    pub(super) fn open() -> Result<Self> {
        let root = Path::new(layout::ROOT);
        let bin = root.join(layout::BIN);
        check_directory(root)?;
        check_directory(&bin)?;
        let state = bin.join(layout::BOOT_STATE);
        match DirBuilder::new().mode(0o700).create(&state) {
            Ok(()) => sync_directory(&bin)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err("create boot state"),
        }
        check_directory(&state)?;
        if metadata(&state)?.mode() & 0o077 != 0 {
            return Err("boot state must be root-only");
        }
        let lock = state_file(&state.join(LOCK), OFlags::RDWR | OFlags::CREATE).map_err(|_| "open supervisor lock")?;
        check_state_file(&lock)?;
        lock.try_lock()
            .map_err(|_| "supervisor already active or cleanup pending")?;
        rustix::io::fcntl_setfd(&lock, rustix::io::FdFlags::empty()).map_err(|_| "inherit supervisor lock")?;
        Ok(Self { state, _lock: lock })
    }

    pub(super) fn load(&self) -> Result<Option<Selection>> {
        let file = match state_file(&self.state.join(layout::SELECTION), OFlags::RDONLY) {
            Ok(file) => file,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err("open selection journal"),
        };
        check_state_file(&file)?;
        let mut bytes = Vec::new();
        file.take(1025)
            .read_to_end(&mut bytes)
            .map_err(|_| "read selection journal")?;
        if bytes.len() > 1024 {
            return Err("invalid selection journal");
        }
        Selection::decode(&bytes).map(Some)
    }

    pub(super) fn save(&self, selection: &Selection) -> Result<()> {
        let temp = self.state.join(TEMP);
        remove(&temp).map_err(|_| "clear transient selection journal")?;
        let mut file = state_file(&temp, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL)
            .map_err(|_| "create selection journal")?;
        file.write_all(&selection.encode()?)
            .and_then(|()| file.sync_all())
            .map_err(|_| "write selection journal")?;
        fs::rename(temp, self.state.join(layout::SELECTION)).map_err(|_| "publish selection journal")?;
        sync_directory(&self.state)
    }
}

fn state_file(path: &Path, flags: OFlags) -> rustix::io::Result<File> {
    open(
        path,
        flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
}

fn check_state_file(file: &File) -> Result<()> {
    let meta = file.metadata().map_err(|_| "inspect boot state")?;
    if meta.is_file() && meta.uid() == 0 && meta.mode() & 0o7077 == 0 && meta.nlink() == 1 {
        Ok(())
    } else {
        Err("unsafe boot state file")
    }
}

fn sync_directory(directory: &Path) -> Result<()> {
    File::open(directory)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| "sync boot state directory")
}

fn metadata(path: &Path) -> Result<Metadata> {
    fs::symlink_metadata(path).map_err(|_| "inspect install layout")
}

fn check_directory(directory: &Path) -> Result<()> {
    let meta = metadata(directory)?;
    if meta.is_dir() && meta.uid() == 0 && meta.mode() & 0o022 == 0 {
        Ok(())
    } else {
        Err("unsafe install directory")
    }
}

pub(super) fn candidate(slot: Slot) -> Result<(PathBuf, File)> {
    let bin = Path::new(layout::ROOT).join(layout::BIN);
    let path = bin.join(slot.name());
    let file = state_file(&path, OFlags::RDONLY).map_err(|_| "open selected binary")?;
    let meta = file.metadata().map_err(|_| "inspect selected binary")?;
    if meta.is_file()
        && meta.uid() == 0
        && meta.nlink() == 1
        && meta.mode() & 0o7022 == 0
        && meta.mode() & 0o111 == 0o111
    {
        Ok((path, file))
    } else {
        Err("unsafe selected binary")
    }
}

pub(super) fn digest(slot: Slot) -> Result<Digest> {
    let (_, mut file) = candidate(slot)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let read = file.read(&mut buffer).map_err(|_| "hash selected binary")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    Digest::parse(hex)
}

pub(super) fn marker(name: &str) -> PathBuf {
    Path::new(layout::ROOT)
        .join(layout::PRIVATE)
        .join(layout::RUN)
        .join(name)
}

pub(super) fn stopped() -> Result<bool> {
    match fs::symlink_metadata(marker(layout::STOP)) {
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => Err("unsafe stop marker"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("inspect stop marker"),
    }
}

// Called only by the credential-dropped marker helper.
pub(super) fn acknowledge(finished: bool) -> Result<()> {
    let path = marker(layout::FINISHED);
    if !finished {
        return remove(&path).map_err(|_| "clear finished marker");
    }
    let temp = path.with_extension("new");
    remove(&temp).map_err(|_| "clear transient finished marker")?;
    state_file(&temp, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL)
        .and_then(|file| rustix::fs::fchmod(file, Mode::RUSR | Mode::WUSR))
        .map_err(|_| "create finished marker")?;
    fs::rename(temp, path).map_err(|_| "publish finished marker")
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
