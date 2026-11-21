//! Private, single-attempt HTTPS transport. Never expose reqwest diagnostics.
use crate::Error;
use echo_config::MuseConfig;
use echo_contracts::secret::Secret;
use reqwest::{Client, Method, StatusCode, Url, header};
use std::{io::Read, path::Path, time::Duration};

pub(super) const MAX_BODY: usize = 1_048_576;
const OPERATION: Duration = Duration::from_secs(5);

pub(super) struct Http {
    client: Client,
    origin: Url,
    credential: Secret<header::HeaderValue>,
}

impl Http {
    pub fn open(config: &MuseConfig, local: bool) -> Result<Self, Error> {
        let origin = origin(&config.url, local)?;
        let certificate = reqwest::Certificate::from_der(&read(&config.ca_file, 65536)?).map_err(|_| Error::Input)?;
        let credential = credential(read(&config.credential_file, 257)?)?;
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
        let url = self.origin.join(path).map_err(|_| Error::Input)?;
        let mut request = self
            .client
            .request(method, url)
            .header(header::AUTHORIZATION, self.credential.expose().clone());
        if let Some(body) = body {
            request = request.header(header::CONTENT_TYPE, "application/json").body(body);
        }
        let mut response = request.send().await.map_err(|_| Error::Unavailable)?;
        let status = response.status();
        if !matches!(status, StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT) {
            return Err(Error::Unavailable);
        }
        if response.content_length().is_some_and(|size| size > MAX_BODY as u64) {
            return Err(Error::Decode);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
            if chunk.len() > MAX_BODY - bytes.len() {
                return Err(Error::Decode);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((status.as_u16(), bytes))
    }
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
