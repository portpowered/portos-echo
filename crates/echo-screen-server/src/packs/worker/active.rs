//! Settled pack reads share the bounded reader and accepted settings owner.
use super::Worker;
use crate::{packs::view, screen_settings::SettingsDispatch};
use std::time::Duration;

impl Worker {
    pub(super) async fn watch_active(&self) {
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
            let Ok(permit) = self.files.busy.clone().try_lock_owned() else {
                continue;
            };
            let revision = self.core.lock().await.pack_revision;
            let root = self.root.clone();
            let snapshot = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                view::scan(&root)
            })
            .await;
            let Ok(Ok(catalog)) = snapshot else {
                settled = None;
                continue;
            };
            let signature = catalog.signature();
            if accepted.as_ref() == Some(&signature) {
                continue;
            }
            if settled.as_ref() == Some(&signature) {
                if self.observe(catalog, revision).await {
                    accepted = Some(signature);
                }
            } else {
                settled = Some(signature);
            }
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
}
