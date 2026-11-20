//! Installation publishes through the same accepted settings owner as selection.
use super::Worker;
use crate::{packs::view, screen_settings::SettingsDispatch};

impl Worker {
    pub(super) async fn notify(&self) {
        let Some(host) = self.files.settings.get().cloned() else {
            return;
        };
        let root = self.root.clone();
        let Ok(Ok(catalog)) = tokio::task::spawn_blocking(move || view::scan(&root)).await else {
            return;
        };
        SettingsDispatch {
            core: self.core.clone(),
            stop: self.stop.clone(),
            drain: self.drain.clone(),
            host: Some(host),
        }
        .installed(catalog)
        .await;
    }
}
