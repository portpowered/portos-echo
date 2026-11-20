//! HTTPS-only downloads, with a literal loopback exception for controlled tests.
use crate::integrity::{digest, selected_checksum};
use crate::{Failure, Result};
use reqwest::{Client, Response, StatusCode, Url};
use std::time::Duration;

pub struct Release {
    pub bytes: Vec<u8>,
    pub digest: String,
    pub screen_pack: Option<ScreenPack>,
}

pub struct ScreenPack {
    pub asset: String,
    pub name: &'static str,
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

async fn request(client: &Client, mut url: Url, local: bool) -> Result<Response> {
    for _ in 0..=10 {
        if !allowed(&url, local) {
            return Err(Failure("https_required"));
        }
        let response = client
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
        return Ok(response);
    }
    Err(Failure("redirect_limit"))
}

async fn download(mut response: Response, limit: usize) -> Result<Vec<u8>> {
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
    Ok(bytes)
}

fn verify(bytes: &[u8], expected: &str) -> Result<()> {
    if digest(bytes) != expected {
        return Err(Failure("checksum_mismatch"));
    }
    Ok(())
}

pub async fn fetch(base: &str, version: &str, install_pack: bool) -> Result<Release> {
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
    let sums = download(request(&client, url("SHA256SUMS")?, local).await?, 1024 * 1024).await?;
    let sums = std::str::from_utf8(&sums).map_err(|_| Failure("invalid_checksum"))?;
    let expected = selected_checksum(sums, &asset)?;
    let bytes = download(request(&client, url(&asset)?, local).await?, 512 * 1024 * 1024).await?;
    verify(&bytes, &expected)?;
    let screen_pack = if install_pack {
        Some(fetch_pack(&client, url, local, &version, sums).await?)
    } else {
        None
    };
    Ok(Release {
        bytes,
        digest: expected,
        screen_pack,
    })
}

async fn fetch_pack(
    client: &Client,
    url: impl Fn(&str) -> Result<Url>,
    local: bool,
    version: &str,
    sums: &str,
) -> Result<ScreenPack> {
    let mut name = "default";
    let mut asset = format!("{name}-{version}.screenpack.tar.gz");
    let mut response = request(client, url(&asset)?, local).await?;
    let expected = if response.status() == StatusCode::NOT_FOUND && legacy_eligible(version) {
        name = "template";
        asset = format!("{name}-{version}.screenpack.tar.gz");
        let expected = selected_checksum(sums, &asset)?;
        response = request(client, url(&asset)?, local).await?;
        expected
    } else {
        if response.status() != StatusCode::OK {
            return Err(Failure("download_failed"));
        }
        selected_checksum(sums, &asset)?
    };
    if response.status() != StatusCode::OK {
        return Err(Failure("download_failed"));
    }
    let bytes = download(response, 65 * 1024 * 1024).await?;
    verify(&bytes, &expected)?;
    Ok(ScreenPack {
        asset,
        name,
        bytes,
        digest: expected,
    })
}

fn legacy_eligible(normalized: &str) -> bool {
    normalized
        .strip_prefix("0.")
        .and_then(|rest| rest.split_once('.'))
        .is_some_and(|(minor, _)| matches!(minor, "0" | "1" | "2"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_core_selects_legacy_era() -> Result<()> {
        for version in ["v0.0.0", "0.1.9", "v0.2.0-rc.1", "0.2.99999999999999999999999999"] {
            assert!(legacy_eligible(&crate::cli::version(version)?));
        }
        for version in [
            "v0.3.0-alpha",
            "0.3.0",
            "0.10.1",
            "1.0.0",
            "99999999999999999999999.0.0",
        ] {
            assert!(!legacy_eligible(&crate::cli::version(version)?));
        }
        Ok(())
    }
}
