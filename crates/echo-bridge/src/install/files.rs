use std::{
    fs::{self, DirBuilder, File, Metadata},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
};

use echo_boot::Result;
use rustix::fs::{self as unix, CWD, Mode, OFlags};
use sha2::{Digest as _, Sha256};

const OPEN: OFlags = OFlags::NOFOLLOW.union(OFlags::NONBLOCK).union(OFlags::CLOEXEC);

pub(super) const ROOT: Owner = Owner(0, 0);
pub(super) const CORE: Owner = Owner(9999, 9999);
pub(super) const DEVICE: Owner = Owner(1000, 1000);

#[derive(Clone, Copy)]
pub(super) struct Owner(pub u32, pub u32);

pub(super) struct Files<'a> {
    pub root: &'a Path,
    pub root_uid: u32,
    pub set_owner: fn(&File, Owner) -> Result<()>,
}

impl Files<'_> {
    pub(super) fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative.trim_start_matches('/'))
    }

    pub(super) fn protected(&self, path: &Path) -> Result<File> {
        let relative = path.strip_prefix(self.root).map_err(|_| "path outside device root")?;
        let mut dir = open_dir(self.root)?;
        self.check_root_dir(&dir)?;
        for segment in relative.components() {
            let segment = match segment {
                std::path::Component::Normal(segment) => segment,
                _ => return Err("untrusted directory"),
            };
            dir = unix::openat(&dir, segment, OPEN | OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
                .map(File::from)
                .map_err(|_| "untrusted directory")?;
            self.check_root_dir(&dir)?;
        }
        Ok(dir)
    }

    fn check_root_dir(&self, file: &File) -> Result<()> {
        let meta = file.metadata().map_err(|_| "inspect directory")?;
        if meta.uid() == self.root_uid && meta.mode() & 0o022 == 0 {
            Ok(())
        } else {
            Err("untrusted directory")
        }
    }

    pub(super) fn trusted_file(&self, path: &Path) -> Result<File> {
        let parent = self.protected(path.parent().ok_or("file parent missing")?)?;
        let name = path.file_name().ok_or("file name missing")?;
        let file = read_at(&parent, Path::new(name))?;
        let meta = file.metadata().map_err(|_| "inspect trusted file")?;
        if meta.uid() != self.root_uid || meta.mode() & 0o7022 != 0 {
            return Err("untrusted file");
        }
        Ok(file)
    }

    pub(super) fn directory(&self, path: &Path, owner: Owner, mode: u32) -> Result<()> {
        DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| "create install directory")?;
        self.metadata(&open_dir(path)?, owner, mode)?;
        sync_parent(path)
    }

    pub(super) fn copy(&self, mut source: File, path: &Path, owner: Owner, mode: u32) -> Result<()> {
        let mut target = create(path)?;
        io::copy(&mut source, &mut target).map_err(|_| "copy install file")?;
        self.publish_file(&target, path, owner, mode)
    }

    pub(super) fn write(&self, path: &Path, bytes: &[u8], owner: Owner, mode: u32) -> Result<()> {
        let mut target = create(path)?;
        target.write_all(bytes).map_err(|_| "write install file")?;
        self.publish_file(&target, path, owner, mode)
    }

    fn publish_file(&self, file: &File, path: &Path, owner: Owner, mode: u32) -> Result<()> {
        self.metadata(file, owner, mode)?;
        file.sync_all().map_err(|_| "sync install file")?;
        sync_parent(path)
    }

    fn metadata(&self, file: &File, owner: Owner, mode: u32) -> Result<()> {
        (self.set_owner)(file, owner)?;
        unix::fchmod(file, Mode::from_bits_truncate(mode)).map_err(|_| "set install permissions")
    }
}

pub(super) fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("inspect install path"),
    }
}

pub(super) fn open_dir(path: &Path) -> Result<File> {
    unix::open(path, OPEN | OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
        .map(File::from)
        .map_err(|_| "open install directory")
}

pub(super) fn read_at(dir: impl std::os::fd::AsFd, path: &Path) -> Result<File> {
    let file = unix::openat(dir, path, OPEN | OFlags::RDONLY, Mode::empty())
        .map(File::from)
        .map_err(|_| "open install file")?;
    let meta = file.metadata().map_err(|_| "inspect install file")?;
    if meta.is_file() && meta.nlink() == 1 {
        Ok(file)
    } else {
        Err("unsafe install file")
    }
}

pub(super) fn read_text(file: File, limit: u64) -> Result<String> {
    let mut text = String::new();
    file.take(limit + 1)
        .read_to_string(&mut text)
        .map_err(|_| "read install data")?;
    if text.len() as u64 > limit {
        return Err("install data exceeds limit");
    }
    Ok(text)
}

pub(super) fn digest(mut file: File) -> Result<String> {
    use std::fmt::Write as _;
    let mut digest = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let count = file.read(&mut buffer).map_err(|_| "hash install file")?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let mut hex = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(hex, "{byte:02x}").map_err(|_| "encode install digest")?;
    }
    Ok(hex)
}

pub(super) fn source_dir(parent: &File, name: &Path) -> Result<File> {
    unix::openat(parent, name, OPEN | OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
        .map(File::from)
        .map_err(|_| "unsafe source directory")
}

pub(super) fn copy_tree(
    files: &Files<'_>,
    source: &File,
    destination: &Path,
    owner: Owner,
    mode: u32,
    depth: u8,
) -> Result<()> {
    if depth == 0 {
        return Err("source tree too deep");
    }
    files.directory(destination, owner, mode)?;
    let entries = unix::Dir::read_from(source).map_err(|_| "read source directory")?;
    for entry in entries {
        let entry = entry.map_err(|_| "read source entry")?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        let name = Path::new(std::os::unix::ffi::OsStrExt::from_bytes(name));
        let target = destination.join(name);
        let meta = unix::statat(source, name, unix::AtFlags::SYMLINK_NOFOLLOW).map_err(|_| "inspect source entry")?;
        if unix::FileType::from_raw_mode(meta.st_mode) == unix::FileType::Directory {
            copy_tree(files, &source_dir(source, name)?, &target, owner, mode, depth - 1)?;
        } else {
            files.copy(read_at(source, name)?, &target, owner, mode & 0o666)?;
        }
    }
    Ok(())
}

pub(super) fn rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).map_err(|_| "rename install path")?;
    sync_parent(from)?;
    sync_parent(to)
}

pub(super) fn remove(path: &Path) -> Result<()> {
    if exists(path)? {
        let meta = fs::symlink_metadata(path).map_err(|_| "inspect removal path")?;
        let result = if meta.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        result.map_err(|_| "remove install path")?;
        sync_parent(path)?;
    }
    Ok(())
}

pub(super) fn mode(meta: &Metadata) -> u32 {
    meta.mode() & 0o7777
}

fn create(path: &Path) -> Result<File> {
    unix::openat(
        CWD,
        path,
        OPEN | OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
    .map_err(|_| "create install file")
}

fn sync_parent(path: &Path) -> Result<()> {
    open_dir(path.parent().ok_or("install path parent missing")?)?
        .sync_all()
        .map_err(|_| "sync install directory")
}
