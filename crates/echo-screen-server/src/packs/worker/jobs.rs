//! Bounded serial installer admission and payload-free public status.
use super::super::{Error, download, view};
use crate::screen_http::{HttpState, authorized};
use axum::{
    Json,
    extract::{Request as HttpRequest, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub(crate) struct Request {
    pub id: String,
    pub url: String,
    pub digest: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct Job {
    id: String,
    status: &'static str,
    reason: Option<&'static str>,
}

#[derive(Default)]
pub(crate) struct Jobs {
    next: u64,
    entries: VecDeque<Job>,
}

impl Jobs {
    pub(in crate::packs) fn snapshot(&self) -> Vec<Job> {
        self.entries.iter().cloned().collect()
    }

    pub(in crate::packs) fn admit(&mut self) -> Option<String> {
        if self
            .entries
            .iter()
            .filter(|j| matches!(j.status, "queued" | "downloading"))
            .count()
            >= 4
        {
            return None;
        }
        self.configured()
    }

    pub(in crate::packs) fn configured(&mut self) -> Option<String> {
        self.next = self.next.checked_add(1)?;
        if self.entries.len() == 32 {
            let index = self
                .entries
                .iter()
                .position(|j| matches!(j.status, "installed" | "failed"))?;
            self.entries.remove(index);
        }
        let id = self.next.to_string();
        self.entries.push_back(Job {
            id: id.clone(),
            status: "queued",
            reason: None,
        });
        Some(id)
    }

    pub(in crate::packs) fn update(&mut self, id: &str, status: &'static str, error: Option<Error>) {
        if let Some(job) = self.entries.iter_mut().find(|job| job.id == id) {
            job.status = status;
            job.reason = error.map(reason);
        }
    }
}

fn reason(error: Error) -> &'static str {
    match error {
        Error::Ca => "tls_failed",
        Error::Url => "invalid_url",
        Error::Download => "download_failed",
        Error::Deadline => "download_timeout",
        Error::Digest => "checksum_mismatch",
        Error::Path => "unsafe_path",
        Error::Link => "link_entry",
        Error::Entry => "special_entry",
        Error::Duplicate => "duplicate_entry",
        Error::Count => "file_count_limit",
        Error::Size | Error::Transfer => "expanded_size_limit",
        Error::Clip => "clip_size_limit",
        Error::Manifest | Error::Reference | Error::Type | Error::Archive => "invalid_manifest",
        Error::Css => "unsafe_css",
        Error::Storage | Error::Conflict | Error::Cancelled => "install_failed",
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    version: u8,
    url: String,
    sha256: String,
}

fn validate(input: &Input) -> Result<(), &'static str> {
    if download::url(&input.url).is_err() {
        return Err("invalid_url");
    }
    if input.sha256.len() != 64 || !input.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid_sha256");
    }
    Ok(())
}

pub(crate) async fn install(State(state): State<HttpState>, request: HttpRequest) -> Response {
    if let Err(status) = view::guard(&state, request.uri(), request.headers()).await {
        return status.into_response();
    }
    let uri = request.uri().clone();
    let input: Input = match view::input(&state, request).await {
        Ok(input) => input,
        Err(status) => return status.into_response(),
    };
    if input.version != 1 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if let Err(reason) = validate(&input) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"version":1,"reason":reason})),
        )
            .into_response();
    }
    // Reserve queue capacity before creating a public job; serialize the final
    // admission with revoke/drain, as for the settings writer's commit boundary.
    let Some(sender) = state.packs.requests.get() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(permit) = sender.try_reserve() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let core = state.core.lock().await;
    if !authorized(&core, &uri) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut jobs = state.packs.jobs.lock().await;
    if !core.accepting() || *state.stop.borrow() || state.drain.borrow().is_some() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(id) = jobs.admit() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    permit.send(Request {
        id: id.clone(),
        url: input.url,
        digest: input.sha256,
    });
    (StatusCode::ACCEPTED, Json(serde_json::json!({"version":1,"job":id}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_bound_pending_and_history_without_evicting_live_work() {
        let mut jobs = Jobs::default();
        let first = jobs.admit().unwrap();
        jobs.update(&first, "downloading", None);
        for _ in 0..3 {
            assert!(jobs.admit().is_some());
        }
        assert_eq!(jobs.admit(), None);
        for _ in 0..40 {
            let id = jobs.configured().unwrap();
            jobs.update(&id, "failed", Some(Error::Digest));
        }
        assert_eq!(jobs.snapshot().len(), 32);
        assert_eq!(jobs.snapshot()[0].id, first);
        assert_eq!(jobs.snapshot()[0].status, "downloading");
        assert_eq!(jobs.snapshot()[31].reason, Some("checksum_mismatch"));
        jobs.update(&first, "installed", None);
        assert!(jobs.admit().is_some());
        assert_eq!(jobs.admit(), None);
    }
}
