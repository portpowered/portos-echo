//! Private, single-attempt HTTPS transport. Never expose reqwest diagnostics.
use crate::Error;
use echo_config::MuseConfig;
use echo_contracts::secret::Secret;
use echo_screen_contract::settings_v2::MuseProbeReasonV2;
use reqwest::{Client, Method, StatusCode, Url, header};
use std::{io::Read, path::Path, time::Duration};

pub(super) const MAX_BODY: usize = 1_048_576;
const OPERATION: Duration = Duration::from_secs(5);

pub(crate) struct Http {
    client: Client,
    origin: Url,
    credential: Secret<header::HeaderValue>,
}

impl Http {
    pub fn open(config: &MuseConfig, local: bool) -> Result<Self, Error> {
        Self::from_inputs(
            Self::origin(&config.url, local)?,
            &Self::read_ca(&config.ca_file)?,
            &Self::read_credential(&config.credential_file)?,
        )
    }

    pub(crate) fn origin(text: &str, local: bool) -> Result<Url, Error> {
        origin(text, local)
    }

    pub(crate) fn read_ca(path: &Path) -> Result<Secret<Vec<u8>>, Error> {
        read(path, 65536).map(Secret::new)
    }

    pub(crate) fn read_credential(path: &Path) -> Result<Secret<Vec<u8>>, Error> {
        read(path, 257).map(Secret::new)
    }

    pub(crate) fn validate_credential(private: &Secret<Vec<u8>>) -> Result<(), Error> {
        credential(private.expose().clone()).map(|_| ())
    }

    pub(crate) fn from_inputs(origin: Url, ca: &Secret<Vec<u8>>, private: &Secret<Vec<u8>>) -> Result<Self, Error> {
        let certificate = reqwest::Certificate::from_der(ca.expose()).map_err(|_| Error::Input)?;
        let credential = credential(private.expose().clone())?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .timeout(OPERATION)
            .build()
            .map_err(|_| Error::Input)?;
        Ok(Self {
            client,
            origin,
            credential,
        })
    }

    pub async fn request(&self, method: Method, path: &str, body: Option<Vec<u8>>) -> Result<(u16, Vec<u8>), Error> {
        let mut request = self.prepare(method, path)?;
        if let Some(body) = body {
            request = request.header(header::CONTENT_TYPE, "application/json").body(body);
        }
        let response = request.send().await.map_err(|_| Error::Unavailable)?;
        let status = response.status();
        if !matches!(status, StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT) {
            return Err(Error::Unavailable);
        }
        Ok((status.as_u16(), response_bytes(response, MAX_BODY).await?))
    }

    fn prepare(&self, method: Method, path: &str) -> Result<reqwest::RequestBuilder, Error> {
        let url = self.origin.join(path).map_err(|_| Error::Input)?;
        Ok(self
            .client
            .request(method, url)
            .header(header::AUTHORIZATION, self.credential.expose().clone()))
    }

    pub(crate) async fn probe(&self) -> Result<(), MuseProbeReasonV2> {
        use MuseProbeReasonV2::{InvalidConfiguration, Unauthorized, Unavailable, UnexpectedResponse};
        let response = self
            .prepare(Method::GET, "/v1/tasks")
            .map_err(|_| InvalidConfiguration)?
            .send()
            .await
            .map_err(|_| Unavailable)?;
        match response.status() {
            StatusCode::UNAUTHORIZED => Err(Unauthorized),
            status if status.is_server_error() => Err(Unavailable),
            StatusCode::METHOD_NOT_ALLOWED => {
                let bytes = response_bytes(response, 1024).await.map_err(|error| {
                    if error == Error::Unavailable {
                        Unavailable
                    } else {
                        UnexpectedResponse
                    }
                })?;
                let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| UnexpectedResponse)?;
                (value.get("error").and_then(serde_json::Value::as_str) == Some("method_not_allowed"))
                    .then_some(())
                    .ok_or(UnexpectedResponse)
            }
            _ => Err(UnexpectedResponse),
        }
    }
}

async fn response_bytes(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, Error> {
    if response.content_length().is_some_and(|size| size > limit as u64) {
        return Err(Error::Decode);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
        if chunk.len() > limit - bytes.len() {
            return Err(Error::Decode);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn origin(text: &str, local: bool) -> Result<Url, Error> {
    let authority = text.strip_prefix("https://").ok_or(Error::Input)?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    let url = Url::parse(text).map_err(|_| Error::Input)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.port() == Some(0)
        || text.trim() != text
        || authority.contains(['@', '/', '\\', '?', '#'])
        || (local
            && (url.host_str() != Some("127.0.0.1")
                || !(authority == "127.0.0.1" || authority.starts_with("127.0.0.1:"))))
    {
        return Err(Error::Input);
    }
    Ok(url)
}

fn credential(mut bytes: Vec<u8>) -> Result<Secret<header::HeaderValue>, Error> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if !(32..=256).contains(&bytes.len()) || !bytes.iter().all(u8::is_ascii_graphic) {
        return Err(Error::Input);
    }
    let mut value = b"Bearer ".to_vec();
    value.extend_from_slice(&bytes);
    let mut value = header::HeaderValue::from_bytes(&value).map_err(|_| Error::Input)?;
    value.set_sensitive(true);
    Ok(Secret::new(value))
}

fn read(path: &Path, max: u64) -> Result<Vec<u8>, Error> {
    let file = std::fs::File::open(path).map_err(|_| Error::Input)?;
    let meta = file.metadata().map_err(|_| Error::Input)?;
    if !meta.is_file() || meta.len() > max {
        return Err(Error::Input);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = std::fs::metadata("/proc/self").map_err(|_| Error::Input)?.uid();
        if meta.mode() & 0o077 != 0 || meta.uid() != owner {
            return Err(Error::Input);
        }
    }
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes).map_err(|_| Error::Input)?;
    if bytes.len() as u64 > max {
        return Err(Error::Input);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
