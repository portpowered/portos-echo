use crate::{AuthError, AuthOptions};
use reqwest::Client;
use serde::Deserialize;
use url::Url;

#[derive(Deserialize)]
pub(crate) struct Metadata {
    pub issuer: String,
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
    pub revocation_endpoint: String,
    pub dpop_signing_alg_values_supported: Vec<String>,
}

#[derive(Deserialize)]
struct Resource {
    resource: String,
    authorization_servers: Vec<String>,
}

pub(crate) fn safe_url(value: &str) -> Result<Url, AuthError> {
    let url = Url::parse(value).map_err(|_| AuthError::Discovery)?;
    let secure = url.scheme() == "https";
    let local = url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if (!secure && !local)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AuthError::Discovery);
    }
    Ok(url)
}

pub(crate) async fn discover(client: &Client, options: &AuthOptions) -> Result<Metadata, AuthError> {
    let mut resource_url = safe_url(&options.resource_url)?;
    resource_url.set_path("/.well-known/oauth-protected-resource");
    resource_url.set_query(None);
    let resource: Resource = get(client, resource_url.as_str()).await?;
    if resource.resource != options.resource_url || resource.authorization_servers.len() != 1 {
        return Err(AuthError::Discovery);
    }
    let issuer = safe_url(&resource.authorization_servers[0])?;
    let metadata_url = format!(
        "{}/.well-known/oauth-authorization-server",
        issuer.as_str().trim_end_matches('/')
    );
    let metadata: Metadata = get(client, &metadata_url).await?;
    if metadata.issuer != resource.authorization_servers[0]
        || !metadata
            .dpop_signing_alg_values_supported
            .iter()
            .any(|alg| alg == "ES256")
    {
        return Err(AuthError::Discovery);
    }
    for endpoint in [
        &metadata.device_authorization_endpoint,
        &metadata.token_endpoint,
        &metadata.revocation_endpoint,
    ] {
        let url = safe_url(endpoint)?;
        if url.origin() != issuer.origin() {
            return Err(AuthError::Discovery);
        }
    }
    Ok(metadata)
}

async fn get<T: serde::de::DeserializeOwned>(client: &Client, url: &str) -> Result<T, AuthError> {
    let reply = client.get(url).send().await.map_err(|_| AuthError::Transport)?;
    if !reply.status().is_success() {
        return Err(AuthError::Transport);
    }
    reply.json().await.map_err(|_| AuthError::Discovery)
}
