use std::{fs::File, os::unix::fs::MetadataExt, path::Path, time::Duration};

use echo_boot::{Result, layout};
use echo_core_api::{Clock, SystemClock};
use rustix::fs::{self as unix, AtFlags, Mode, OFlags};

use super::super::files::{self, CORE, Files};

fn private(files: &Files<'_>) -> Result<File> {
    let root = files.protected(&files.path(layout::ROOT))?;
    let dir = files::source_dir(&root, Path::new(layout::PRIVATE))?;
    check_dir(&dir)?;
    Ok(dir)
}

fn run_dir(files: &Files<'_>) -> Result<File> {
    let dir = files::source_dir(&private(files)?, Path::new(layout::RUN))?;
    check_dir(&dir)?;
    Ok(dir)
}

fn check_dir(dir: &File) -> Result<()> {
    let meta = dir.metadata().map_err(|_| "inspect setup directory")?;
    if (meta.uid(), meta.gid(), files::mode(&meta)) != (CORE.0, CORE.1, 0o700) {
        return Err("unsafe setup directory");
    }
    Ok(())
}

pub(super) fn read(files: &Files<'_>, leaf: &str) -> Result<File> {
    let file = files::read_at(private(files)?, Path::new(leaf))?;
    check_file(&file)?;
    Ok(file)
}

fn check_file(file: &File) -> Result<()> {
    let meta = file.metadata().map_err(|_| "inspect setup file")?;
    if meta.uid() != CORE.0 || files::mode(&meta) != 0o600 {
        return Err("unsafe setup file");
    }
    Ok(())
}

pub(super) fn request_park(files: &Files<'_>) -> Result<()> {
    let run = run_dir(files)?;
    match unix::openat(
        &run,
        layout::STOP,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    ) {
        Ok(fd) => (files.set_owner)(&File::from(fd), CORE)?,
        Err(rustix::io::Errno::EXIST) => check_file(&files::read_at(&run, Path::new(layout::STOP))?)?,
        Err(_) => return Err("request supervisor park"),
    }
    Ok(())
}

pub(super) fn park(files: &Files<'_>) -> Result<()> {
    request_park(files)?;
    let deadline = SystemClock.now().monotonic + Duration::from_secs(8);
    loop {
        if parked(files).is_ok() {
            return Ok(());
        }
        if SystemClock.now().monotonic >= deadline {
            return Err("supervisor did not acknowledge park");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn parked(files: &Files<'_>) -> Result<()> {
    let run = run_dir(files)?;
    for leaf in [layout::STOP, layout::FINISHED] {
        check_file(&files::read_at(&run, Path::new(leaf))?)?;
    }
    if super::observations::core_running(files)? {
        return Err("core is still active");
    }
    Ok(())
}

pub(super) fn resume(files: &Files<'_>) -> Result<()> {
    let run = run_dir(files)?;
    match unix::unlinkat(run, layout::STOP, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(_) => Err("release supervisor park"),
    }
}

pub(super) fn replace(files: &Files<'_>, leaf: &str, bytes: &[u8]) -> Result<()> {
    parked(files)?;
    let dir = private(files)?;
    let work = files.path("/data/local/echo/bin/.install");
    let root = files.protected(&work)?;
    let temp = ".account-write";
    let temporary = work.join(temp);
    files::remove(&temporary)?;
    let result = files.write(&temporary, bytes, CORE, 0o600).and_then(|()| {
        unix::renameat(&root, temp, &dir, leaf).map_err(|_| "replace private file")?;
        dir.sync_all().map_err(|_| "sync private file")
    });
    files::remove(&temporary).and(result)
}

pub(super) fn replace_key(files: &Files<'_>, leaf: &str, bytes: &[u8]) -> Result<()> {
    if leaf.is_empty()
        || leaf.len() > 64
        || leaf.starts_with('.')
        || !leaf
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || matches!(
            leaf,
            "bridge.toml" | "bridge.log" | "bridge.trace" | "screen-pairing.txt" | "launch.sh"
        )
        || !bytes.iter().all(|byte| (b' '..=b'~').contains(byte))
        || bytes.first() == Some(&b' ')
        || bytes.last() == Some(&b' ')
    {
        return Err("invalid private credential");
    }
    let config = files::read_text(read(files, layout::CONFIG)?, 1024 * 1024)?;
    let config = echo_config::BridgeConfig::from_toml_str(&config).map_err(|_| "invalid setup config")?;
    if config.live.mode != echo_config::LiveMode::ApiKey
        || config.live.credential_file != Path::new(super::ROOT).join(layout::PRIVATE).join(leaf)
    {
        return Err("API key target differs from installed config");
    }
    replace(files, leaf, bytes)
}
