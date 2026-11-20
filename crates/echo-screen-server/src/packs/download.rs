use super::{Error, TRANSFER_LIMIT};
use reqwest::{Client, Url};
use std::path::Path;
use tokio::io::AsyncWriteExt;

pub(super) fn url(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|_| Error::Url)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || value.split('/').nth(2).is_some_and(|authority| authority.contains('@'))
        || value.contains('\\')
        || value.chars().any(char::is_whitespace)
    {
        return Err(Error::Url);
    }
    Ok(url)
}

pub(super) async fn client(ca: Option<&Path>) -> Result<Client, Error> {
    let ca = ca.ok_or(Error::Ca)?;
    let file = tokio::fs::File::open(ca).await.map_err(|_| Error::Ca)?;
    let mut pem = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut tokio::io::AsyncReadExt::take(file, 1_048_577), &mut pem)
        .await
        .map_err(|_| Error::Ca)?;
    if pem.len() > 1_048_576 {
        return Err(Error::Ca);
    }
    let roots = reqwest::Certificate::from_pem_bundle(&pem).map_err(|_| Error::Ca)?;
    if roots.is_empty() {
        return Err(Error::Ca);
    }
    let mut builder = Client::builder()
        .no_proxy()
        .tls_built_in_root_certs(false)
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never());
    for root in roots {
        builder = builder.add_root_certificate(root);
    }
    builder.build().map_err(|_| Error::Ca)
}

pub(super) async fn fetch(client: &Client, initial: &str, destination: &Path) -> Result<(), Error> {
    let mut target = url(initial)?;
    for redirects in 0..=3 {
        let response = client.get(target.clone()).send().await.map_err(|_| Error::Download)?;
        if response.status().is_redirection() {
            if redirects == 3 {
                return Err(Error::Download);
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(Error::Download)?;
            let next = target.join(location).map_err(|_| Error::Url)?;
            target = url(next.as_str())?;
            continue;
        }
        if !response.status().is_success() {
            return Err(Error::Download);
        }
        return body(response, destination).await;
    }
    Err(Error::Download)
}

async fn body(mut response: reqwest::Response, destination: &Path) -> Result<(), Error> {
    if response.content_length().is_some_and(|n| n > TRANSFER_LIMIT) {
        return Err(Error::Transfer);
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(destination)
        .await
        .map_err(|_| Error::Storage)?;
    let mut count = 0_u64;
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Download)? {
        count = count.checked_add(chunk.len() as u64).ok_or(Error::Transfer)?;
        if count > TRANSFER_LIMIT {
            return Err(Error::Transfer);
        }
        file.write_all(&chunk).await.map_err(|_| Error::Storage)?;
    }
    file.flush().await.map_err(|_| Error::Storage)
}
