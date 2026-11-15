use crate::AuthError;
use std::time::Duration;

const MAX_BODY: usize = 256 * 1024;
pub(crate) fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, AuthError> {
    serde_json::from_slice(body).map_err(|_| AuthError::InvalidCredential)
}
pub(crate) async fn request(
    request: reqwest::RequestBuilder,
    timeout: Duration,
) -> Result<(reqwest::StatusCode, Vec<u8>), AuthError> {
    tokio::time::timeout(timeout, async {
        let mut response = request.send().await.map_err(|_| AuthError::Transport)?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Transport)? {
            if chunk.len() > MAX_BODY.saturating_sub(bytes.len()) {
                return Err(AuthError::InvalidCredential);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((status, bytes))
    })
    .await
    .map_err(|_| AuthError::Timeout)?
}
