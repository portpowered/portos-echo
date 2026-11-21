//! Settled pack reads share the bounded reader and accepted settings owner.
use super::Worker;
use crate::{
    packs::{catalog, view},
    screen_settings::SettingsDispatch,
};
use std::time::Duration;

#[derive(Default)]
struct Watch {
    settled: Option<[u8; 32]>,
    accepted: Option<[u8; 32]>,
    /// The stamp taken just before the scan that matched the accepted catalog.
    quiet: Option<[u8; 32]>,
    #[cfg(test)]
    scans: usize,
}

impl Worker {
    pub(super) async fn watch_active(&self) {
        let mut watch = Watch::default();
        let mut ticks = tokio::time::interval(Duration::from_millis(250));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                () = crate::cancelled(self.stop.clone()) => return,
                () = crate::draining(self.drain.clone()) => return,
                _ = ticks.tick() => {}
            }
            self.watch_step(&mut watch).await;
        }
    }

    async fn watch_step(&self, watch: &mut Watch) {
        // A scan reads and hashes every installed pack, which is seconds of
        // CPU on the Echo. Run it only when a name, size or file identity
        // under the root has changed since the accepted catalog.
        let root = self.root.clone();
        let stamp = tokio::task::spawn_blocking(move || catalog::stamp(&root)).await;
        let stamp = stamp.ok().and_then(Result::ok);
        if stamp.is_some() && stamp == watch.quiet {
            return;
        }
        let revision = self.core.lock().await.pack_revision;
        let root = self.root.clone();
        // This single serial loop owns one bounded scan. Background work
        // must not take the HTTP capacity permit or starve settings reads.
        let snapshot = tokio::task::spawn_blocking(move || view::scan(&root)).await;
        #[cfg(test)]
        {
            watch.scans += 1;
        }
        let Ok(Ok(catalog)) = snapshot else {
            watch.settled = None;
            watch.quiet = None;
            return;
        };
        let signature = catalog.signature();
        if watch.accepted == Some(signature) {
            watch.quiet = stamp;
        } else if watch.settled != Some(signature) {
            watch.settled = Some(signature);
        } else if self.observe(catalog, revision).await {
            watch.accepted = Some(signature);
            watch.quiet = stamp;
        }
    }

    async fn observe(&self, catalog: crate::PackCatalog, revision: u64) -> bool {
        let Some(host) = self
            .files
            .settings
            .get()
            .and_then(tokio::sync::mpsc::WeakSender::upgrade)
        else {
            return false;
        };
        SettingsDispatch {
            core: self.core.clone(),
            stop: self.stop.clone(),
            drain: self.drain.clone(),
            host: Some(host),
        }
        .watched(catalog, revision)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SettingsOperation, SettingsReply,
        screen_core::{Core, PackObservation},
    };
    use echo_contracts::secret::Secret;
    use std::sync::Arc;
    use tokio::sync::{Mutex, mpsc, watch};

    fn catalog(root: &std::path::Path) -> crate::PackCatalog {
        view::scan(root).unwrap().with_selection(None)
    }

    #[tokio::test]
    async fn pack_watch_install_race_is_one_reload_in_both_commit_orders() {
        for watch_first in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let core = Arc::new(Mutex::new(Core::new(&Secret::new("synthetic-pair".into()))));
            // A prior empty observation establishes the built-in baseline.
            core.lock().await.pack_identity = PackObservation::Current(None);
            let pack = home.path().join("template");
            std::fs::create_dir(&pack).unwrap();
            std::fs::write(
                pack.join("pack.json"),
                r#"{"format":1,"name":"template","version":"1.0.0","character":{"rest":"rest.png"}}"#,
            )
            .unwrap();
            std::fs::write(pack.join("rest.png"), "generated-rest").unwrap();
            let (host, mut inbox) = mpsc::channel::<crate::SettingsRequest>(1);
            let (_stop, stop) = watch::channel(false);
            let (_drain, drain) = watch::channel(None);
            let dispatch = SettingsDispatch {
                core: core.clone(),
                stop,
                drain,
                host: Some(host),
            };
            let owner = tokio::spawn(async move {
                while let Some(request) = inbox.recv().await {
                    request
                        .commit(|operation| match operation {
                            SettingsOperation::PackCatalog(catalog) | SettingsOperation::PackWatch(catalog, _) => {
                                SettingsReply::Packs(catalog.with_selection(None))
                            }
                            _ => SettingsReply::InvalidPackSelection,
                        })
                        .await;
                }
            });
            if watch_first {
                assert!(dispatch.clone().watched(catalog(home.path()), 0).await);
                dispatch.clone().installed(catalog(home.path())).await;
            } else {
                dispatch.clone().installed(catalog(home.path())).await;
                assert!(!dispatch.clone().watched(catalog(home.path()), 0).await);
                assert!(dispatch.clone().watched(catalog(home.path()), 1).await);
            }
            assert_eq!(core.lock().await.pack_revision, 1);
            drop(dispatch);
            owner.await.unwrap();
        }
    }

    fn write_pack(root: &std::path::Path, rest: &str) {
        let pack = root.join("template");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("pack.json"),
            r#"{"format":1,"name":"template","version":"1.0.0","character":{"rest":"rest.png"}}"#,
        )
        .unwrap();
        std::fs::write(pack.join("rest.png"), rest).unwrap();
    }

    // T2: the watcher reads pack files only while the tree is changing.
    #[tokio::test]
    async fn pack_watch_scans_only_when_the_pack_tree_changes() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("screens");
        write_pack(&root, "generated-rest");
        let core = Arc::new(Mutex::new(Core::new(&Secret::new("synthetic-pair".into()))));
        core.lock().await.pack_identity = PackObservation::Current(None);
        let files = Arc::new(crate::packs::Files::default());
        let (host, mut inbox) = mpsc::channel::<crate::SettingsRequest>(1);
        assert!(files.settings.set(host.downgrade()).is_ok());
        let (_stop, stop) = watch::channel(false);
        let (_drain, drain) = watch::channel(None);
        let worker = Worker {
            root: root.clone(),
            client: Err(crate::packs::Error::Ca),
            core,
            stop,
            drain,
            files,
        };
        let owner = tokio::spawn(async move {
            let mut watched = 0;
            while let Some(request) = inbox.recv().await {
                watched += 1;
                request
                    .commit(|operation| match operation {
                        SettingsOperation::PackWatch(catalog, _) => SettingsReply::Packs(catalog.with_selection(None)),
                        _ => SettingsReply::InvalidPackSelection,
                    })
                    .await;
            }
            watched
        });
        let mut state = Watch::default();
        let before = catalog::stamp(&root).unwrap();
        for _ in 0..6 {
            worker.watch_step(&mut state).await;
        }
        // One scan to see the tree, one to see it settled; then only stamps.
        assert_eq!(state.scans, 2);
        assert_eq!(state.quiet, Some(before));
        assert_eq!(catalog::stamp(&root).unwrap(), before);

        write_pack(&root, "a longer replacement rest");
        assert_ne!(catalog::stamp(&root).unwrap(), before);
        for _ in 0..6 {
            worker.watch_step(&mut state).await;
        }
        assert_eq!(state.scans, 4);

        std::fs::write(root.join("template/extra.png"), "added").unwrap();
        for _ in 0..6 {
            worker.watch_step(&mut state).await;
        }
        assert_eq!(state.scans, 6);
        drop(host);
        assert_eq!(owner.await.unwrap(), 3);
    }

    #[test]
    fn pack_stamp_is_stable_for_absent_and_unchanged_roots_and_bounded() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("screens");
        let absent = catalog::stamp(&root).unwrap();
        assert_eq!(catalog::stamp(&root).unwrap(), absent);
        write_pack(&root, "generated-rest");
        let present = catalog::stamp(&root).unwrap();
        assert_ne!(present, absent);
        assert_eq!(catalog::stamp(&root).unwrap(), present);
        std::fs::remove_file(root.join("template/rest.png")).unwrap();
        assert_ne!(catalog::stamp(&root).unwrap(), present);
        for index in 0..4097 {
            std::fs::write(root.join(format!("file-{index}")), "").unwrap();
        }
        assert!(matches!(catalog::stamp(&root), Err(crate::packs::Error::Count)));
    }
}
