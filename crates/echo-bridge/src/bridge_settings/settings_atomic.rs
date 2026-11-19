use super::SettingsError;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    Create,
    Write,
    FileSync,
    Rename,
    DirectorySync,
}

struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Create beside the destination, sync bytes, rename once, then sync directory.
/// The guard owns only a successfully created new file, never a collision.
pub(super) fn replace(
    directory: &Path,
    filename: &str,
    bytes: &[u8],
    mut check: impl FnMut(Stage) -> std::io::Result<()>,
) -> Result<(), SettingsError> {
    let parent = File::open(directory).map_err(|_| SettingsError::Persistence)?;
    let path = directory.join(format!(
        ".screen-save-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    check(Stage::Create).map_err(|_| SettingsError::Persistence)?;
    let mut file = crate::bridge_private::file(OpenOptions::new().write(true).create_new(true))
        .open(&path)
        .map_err(|_| SettingsError::Persistence)?;
    let temporary = Temporary(path);
    let before_rename = (|| -> std::io::Result<()> {
        check(Stage::Write)?;
        file.write_all(bytes)?;
        check(Stage::FileSync)?;
        file.sync_all()?;
        check(Stage::Rename)?;
        std::fs::rename(&temporary.0, directory.join(filename))
    })();
    before_rename.map_err(|_| SettingsError::Persistence)?;
    check(Stage::DirectorySync)
        .and_then(|()| parent.sync_all())
        .map_err(|_| SettingsError::Durability)
}
