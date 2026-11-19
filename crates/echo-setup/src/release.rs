//! HTTPS-only downloads, with a literal loopback exception for controlled tests.
use crate::integrity::{digest, selected_checksum};
use crate::{Failure, Result};
use reqwest::{Client, Url};
use std::time::Duration;

pub struct Release {
    pub bytes: Vec<u8>,
    pub digest: String,
}

pub fn client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Failure("download_failed"))
}

fn allowed(url: &Url, local: bool) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && (url.scheme() == "https" || (local && url.scheme() == "http" && url.host_str() == Some("127.0.0.1")))
}

async fn download(client: &Client, mut url: Url, local: bool, limit: usize) -> Result<Vec<u8>> {
    for _ in 0..=10 {
        if !allowed(&url, local) {
            return Err(Failure("https_required"));
        }
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|_| Failure("download_failed"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(Failure("download_failed"))?;
            let next = url.join(location).map_err(|_| Failure("download_failed"))?;
            if url.scheme() == "https" && next.scheme() != "https" {
                return Err(Failure("https_required"));
            }
            url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(Failure("download_failed"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure("download_failed"))? {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(Failure("download_limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(bytes);
    }
    Err(Failure("redirect_limit"))
}

pub async fn fetch(base: &str, version: &str) -> Result<Release> {
    let version = crate::cli::version(version)?;
    let literal_loopback = base.starts_with("http://127.0.0.1:");
    let base = Url::parse(base).map_err(|_| Failure("https_required"))?;
    // Url normalizes alternative IP spellings. Require the explicit literal too.
    let local = literal_loopback
        && base.scheme() == "http"
        && base.host_str() == Some("127.0.0.1")
        && base.as_str().starts_with("http://127.0.0.1:")
        && base.port().is_some();
    if !allowed(&base, local) || base.query().is_some() {
        return Err(Failure("https_required"));
    }
    let client = client()?;
    let asset = format!("echo-{version}-armv7.tar.gz");
    let directory = format!("{}/v{version}/", base.as_str().trim_end_matches('/'));
    let url = |name: &str| Url::parse(&format!("{directory}{name}")).map_err(|_| Failure("download_failed"));
    let sums = download(&client, url("SHA256SUMS")?, local, 1024 * 1024).await?;
    let sums = std::str::from_utf8(&sums).map_err(|_| Failure("invalid_checksum"))?;
    let expected = selected_checksum(sums, &asset)?;
    let bytes = download(&client, url(&asset)?, local, 512 * 1024 * 1024).await?;
    if digest(&bytes) != expected {
        return Err(Failure("checksum_mismatch"));
    }
    Ok(Release {
        bytes,
        digest: expected,
    })
}
