//! Validated catalog data, resolved by the bridge's existing settings owner.
use super::{Error, catalog, manifest};
use crate::{
    SettingsOperation,
    screen_http::{HttpState, authorized},
};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, path::Path};

#[derive(Serialize)]
struct Installed {
    id: String,
    name: String,
    version: String,
    manifest_url: String,
    rest_url: Option<String>,
}

/// Safe, versioned catalog. Entries can only be created by validating local data.
#[derive(Serialize)]
pub struct PackCatalog {
    version: u8,
    revision: u64,
    active: Option<String>,
    installed: Vec<Installed>,
    jobs: Vec<super::jobs::Job>,
    warning: Option<&'static str>,
}

impl PackCatalog {
    /// Compare resolved looks, so saving an alias for the same pack is a no-op.
    #[must_use]
    pub fn selection_changes(&self, selection: &str) -> bool {
        self.active.as_deref() != self.resolve(Some(selection)).map(|pack| pack.id.as_str())
    }

    pub(crate) fn active(&self) -> Option<String> {
        self.active.clone()
    }

    pub(crate) fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
    }

    /// Resolve the last accepted sparse overlay without reading settings again.
    #[must_use]
    pub fn with_selection(mut self, selection: Option<&str>) -> Self {
        self.active = self.resolve(selection).map(|pack| pack.id.clone());
        self.warning = selection
            .filter(|value| !self.accepts(value))
            .map(|_| "invalid_selection");
        self
    }

    /// A save may disable packs, choose an exact directory, or resolve template.
    /// No caller-supplied path or arbitrary TOML string is accepted.
    #[must_use]
    pub fn accepts(&self, selection: &str) -> bool {
        selection.is_empty() || self.resolve(Some(selection)).is_some()
    }

    fn resolve(&self, selection: Option<&str>) -> Option<&Installed> {
        match selection.unwrap_or("template") {
            "" => None,
            "template" => self
                .installed
                .iter()
                .filter(|pack| pack.name == "template")
                .max_by(|a, b| version_cmp(&a.version, &b.version).then_with(|| a.id.cmp(&b.id))),
            id => self.installed.iter().find(|pack| pack.id == id),
        }
    }
}

fn number_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn version_cmp(a: &str, b: &str) -> Ordering {
    let (a, ap) = a.split_once('-').map_or((a, None), |(v, p)| (v, Some(p)));
    let (b, bp) = b.split_once('-').map_or((b, None), |(v, p)| (v, Some(p)));
    let release = a
        .split('.')
        .zip(b.split('.'))
        .map(|(a, b)| number_cmp(a, b))
        .find(|o| !o.is_eq());
    if let Some(order) = release {
        return order;
    }
    match (ap, bp) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => prerelease_cmp(a, b),
    }
}

fn prerelease_cmp(a: &str, b: &str) -> Ordering {
    for (a, b) in a.split('.').zip(b.split('.')) {
        let numeric = |s: &str| s.bytes().all(|byte| byte.is_ascii_digit());
        let order = match (numeric(a), numeric(b)) {
            (true, true) => number_cmp(a, b),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => a.cmp(b),
        };
        if !order.is_eq() {
            return order;
        }
    }
    a.split('.').count().cmp(&b.split('.').count())
}

fn scan(root: &Path) -> Result<PackCatalog, Error> {
    let mut installed = Vec::new();
    match std::fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            for entry in std::fs::read_dir(root).map_err(|_| Error::Storage)? {
                let entry = entry.map_err(|_| Error::Storage)?;
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if id.starts_with('.') {
                    continue;
                }
                if let Ok(files) = catalog::read(root, &id) {
                    let pack = manifest::parse(&files)?;
                    let base = format!("/screen-packs/v1/{id}/");
                    installed.push(Installed {
                        id,
                        name: pack.name,
                        version: pack.version,
                        manifest_url: format!("{base}pack.json"),
                        rest_url: pack.character.map(|character| format!("{base}{}", character.rest)),
                    });
                }
            }
        }
        _ => return Err(Error::Storage),
    }
    installed.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(PackCatalog {
        version: 1,
        revision: 0,
        active: None,
        installed,
        jobs: vec![],
        warning: None,
    })
}

async fn read(state: &HttpState) -> Result<PackCatalog, StatusCode> {
    let permit = state
        .packs
        .busy
        .clone()
        .try_lock_owned()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let root = state.packs.root.get().cloned().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    // Observe completion before scanning: an installed job must never accompany
    // a catalog captured before that job's atomic directory publication.
    let jobs = state.packs.jobs.lock().await.snapshot();
    let mut catalog = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        scan(&root)
    })
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    catalog.jobs = jobs;
    Ok(catalog)
}

pub(super) async fn guard(state: &HttpState, uri: &Uri, headers: &HeaderMap) -> Result<(), StatusCode> {
    let core = state.core.lock().await;
    if !authorized(&core, uri) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if headers
        .get("origin")
        .is_some_and(|origin| origin != state.origin.as_str())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    if !core.accepting() || *state.stop.borrow() || state.drain.borrow().is_some() || state.packs.root.get().is_none() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(())
}

pub(crate) async fn snapshot(State(state): State<HttpState>, request: Request) -> Response {
    if let Err(status) = guard(&state, request.uri(), request.headers()).await {
        return status.into_response();
    }
    let catalog = match read(&state).await {
        Ok(catalog) => catalog,
        Err(status) => return status.into_response(),
    };
    state
        .settings_dispatch()
        .send(request.uri().clone(), SettingsOperation::PackCatalog(catalog))
        .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u8,
    screen_pack: String,
}

pub(crate) async fn selection(State(state): State<HttpState>, request: Request) -> Response {
    if let Err(status) = guard(&state, request.uri(), request.headers()).await {
        return status.into_response();
    }
    let uri = request.uri().clone();
    let body: Selection = match input(&state, request).await {
        Ok(body) => body,
        Err(status) => return status.into_response(),
    };
    if body.version != 1 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let catalog = match read(&state).await {
        Ok(catalog) => catalog,
        Err(status) => return status.into_response(),
    };
    state
        .settings_dispatch()
        .send(uri, SettingsOperation::PackSelection(catalog, body.screen_pack))
        .await
}

pub(super) async fn input<T: serde::de::DeserializeOwned>(
    state: &HttpState,
    request: Request,
) -> Result<T, StatusCode> {
    let json = request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
        });
    if !json {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let bytes = tokio::select! {
        biased;
        () = crate::cancelled(state.stop.clone()) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        () = crate::draining(state.drain.clone()) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        result = to_bytes(request.into_body(), 65_536) => result,
    }
    .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_template_version_order_is_numeric_stable_and_overflow_free() {
        for (less, greater) in [
            ("2.0.0", "10.0.0"),
            ("1.2.9", "1.3.0"),
            ("1.0.0-beta.2", "1.0.0-beta.10"),
            ("1.0.0-beta.10", "1.0.0"),
            ("1.0.0-9", "1.0.0-alpha"),
            ("1.0.0-alpha", "1.0.0-alpha.1"),
            ("1.0.0-alpha", "1.0.0-beta"),
            ("999999999999999999999999.0.0", "1000000000000000000000000.0.0"),
        ] {
            assert_eq!(version_cmp(less, greater), Ordering::Less);
            assert_eq!(version_cmp(greater, less), Ordering::Greater);
        }
        assert_eq!(version_cmp("01.002.000", "1.2.0"), Ordering::Equal);
        assert_eq!(version_cmp("0.0.0-000", "0.0.0-0"), Ordering::Equal);
    }

    #[test]
    fn pack_catalog_missing_disabled_layout_only_and_alias_selection() {
        let home = tempfile::tempdir().unwrap();
        assert!(
            scan(&home.path().join("missing"))
                .unwrap()
                .with_selection(None)
                .active
                .is_none()
        );
        let root = home.path();
        for id in ["template", "template@1.0.0"] {
            let pack = root.join(id);
            std::fs::create_dir(&pack).unwrap();
            std::fs::write(
                pack.join("pack.json"),
                r##"{
                "format":1,"name":"template","version":"1.0.0","layout":{
                    "safe_area":{"x":0,"y":0,"width":100,"height":100},"slots":{},
                    "stage":{"anchor":{"x":50,"y":50},"scale":1,"background":"#000000"}
                }}"##,
            )
            .unwrap();
        }
        let value = scan(root).unwrap().with_selection(None);
        assert_eq!(value.active.as_deref(), Some("template@1.0.0"));
        assert!(value.installed.iter().all(|pack| pack.rest_url.is_none()));
        assert!(value.accepts("template"));
        assert!(value.accepts("template@1.0.0"));
        assert!(!value.accepts("template@9.0.0"));
        let disabled = scan(root).unwrap().with_selection(Some(""));
        assert_eq!(disabled.active, None);
        assert_eq!(disabled.warning, None);
        let bad = scan(root).unwrap().with_selection(Some("missing"));
        assert_eq!(bad.active, None);
        assert_eq!(bad.warning, Some("invalid_selection"));
    }
}
