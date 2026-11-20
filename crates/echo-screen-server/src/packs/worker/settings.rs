//! Settled, bounded settings reads off the runtime owner; no second writer.
use super::Worker;
use crate::{packs::view, screen_settings::SettingsDispatch};
use std::{fs, io::Read, path::Path, time::Duration};

/// A fixed-home settings snapshot. Submitted HTTP data cannot construct one.
#[derive(Clone)]
pub struct PackSettings {
    text: String,
    metadata: Option<fs::Metadata>,
}

fn same_snapshot(before: &PackSettings, after: &PackSettings) -> bool {
    before.text == after.text
        && match (&before.metadata, &after.metadata) {
            (Some(before), Some(after)) => same_file(before, after),
            (None, None) => true,
            _ => false,
        }
}

impl PackSettings {
    /// Bounded UTF-8 overlay bytes, read outside the settings transaction.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Reject a stale read if another save/replacement won before admission.
    #[must_use]
    pub fn is_current(&self, home: &Path) -> bool {
        match (self.metadata.as_ref(), fs::symlink_metadata(home.join("settings.toml"))) {
            (None, Err(error)) => error.kind() == std::io::ErrorKind::NotFound,
            (Some(before), Ok(after)) => same_file(before, &after),
            _ => false,
        }
    }
}

fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return false;
        }
    }
    after.is_file() && before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

fn read(home: &Path) -> Option<PackSettings> {
    let path = home.join("settings.toml");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(PackSettings {
                text: String::new(),
                metadata: None,
            });
        }
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
        _ => return None,
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0002_0000 | 0x0000_0800);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 65_536 {
        return None;
    }
    let mut text = String::new();
    file.take(65_537).read_to_string(&mut text).ok()?;
    let snapshot = PackSettings {
        text,
        metadata: Some(metadata),
    };
    (snapshot.text.len() <= 65_536 && snapshot.is_current(home)).then_some(snapshot)
}

impl Worker {
    pub(super) async fn watch_settings(&self) {
        let Some(home) = self.root.parent() else { return };
        let mut settled = None;
        let mut accepted = None;
        let mut ticks = tokio::time::interval(Duration::from_millis(250));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                () = crate::cancelled(self.stop.clone()) => return,
                () = crate::draining(self.drain.clone()) => return,
                _ = ticks.tick() => {}
            }
            let home = home.to_owned();
            let Ok(Some(snapshot)) = tokio::task::spawn_blocking(move || read(&home)).await else {
                settled = None;
                continue;
            };
            if accepted.as_ref().is_some_and(|before| same_snapshot(before, &snapshot)) {
                continue;
            }
            if settled.as_ref().is_some_and(|before| same_snapshot(before, &snapshot)) {
                if self.refresh_settings(snapshot.clone()).await {
                    accepted = Some(snapshot);
                }
            } else {
                settled = Some(snapshot);
            }
        }
    }

    async fn refresh_settings(&self, snapshot: PackSettings) -> bool {
        let Some(host) = self
            .files
            .settings
            .get()
            .and_then(tokio::sync::mpsc::WeakSender::upgrade)
        else {
            return false;
        };
        let Ok(_permit) = self.files.busy.try_lock() else {
            return false;
        };
        let root = self.root.clone();
        let Ok(Ok(catalog)) = tokio::task::spawn_blocking(move || view::scan(&root)).await else {
            return false;
        };
        drop(_permit);
        SettingsDispatch {
            core: self.core.clone(),
            stop: self.stop.clone(),
            drain: self.drain.clone(),
            host: Some(host),
        }
        .refresh(catalog, snapshot)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_snapshot_bounds_regular_utf8_and_stale_replacement() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("settings.toml");
        let absent = read(home.path()).unwrap();
        assert_eq!(absent.text(), "");
        assert!(absent.is_current(home.path()));
        fs::write(&path, "screen_pack='local'").unwrap();
        assert!(!absent.is_current(home.path()));
        let local = read(home.path()).unwrap();
        assert_eq!(local.text(), "screen_pack='local'");
        assert!(local.is_current(home.path()));
        let temporary = home.path().join("replacement");
        fs::write(&temporary, local.text()).unwrap();
        fs::rename(temporary, &path).unwrap();
        assert!(!local.is_current(home.path()));
        fs::write(&path, vec![b' '; 65_537]).unwrap();
        assert!(read(home.path()).is_none());
        fs::write(&path, [0xff]).unwrap();
        assert!(read(home.path()).is_none());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(read(home.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn settings_snapshot_refuses_symlinks_and_fifos_without_blocking() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("target");
        let path = home.path().join("settings.toml");
        fs::write(&target, "screen_pack='local'").unwrap();
        symlink(&target, &path).unwrap();
        assert!(read(home.path()).is_none());
        fs::remove_file(&path).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(read(home.path()).is_none());
    }
}
