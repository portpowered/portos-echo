//! Offline grant configuration checks precede discovery and store creation.
use crate::{Error, bridge_input::loopback};
use echo_config::{BridgeConfig, McpAuth};
use echo_mcp_auth::AuthOptions;
use std::{net::SocketAddr, path::Path, time::Duration};

pub struct Settings {
    pub options: AuthOptions,
    pub listen: SocketAddr,
    pub pairing_file: std::path::PathBuf,
}

impl Settings {
    pub(crate) fn configured(config: &BridgeConfig) -> Result<Option<Self>, Error> {
        let Some(server) = config
            .mcp
            .server
            .first()
            .filter(|server| server.auth == McpAuth::PortosDeviceCode)
        else {
            return Ok(None);
        };
        let mcp = &config.mcp;
        let directory = mcp.state_dir.as_ref().ok_or(Error::Input)?;
        outside_repository(directory)?;
        if mcp.client_id.trim().is_empty() || mcp.scopes.is_empty() || mcp.scopes.iter().any(|s| s.trim().is_empty()) {
            return Err(Error::Input);
        }
        Ok(Some(Self {
            options: AuthOptions {
                resource_url: server.url.clone(),
                client_id: mcp.client_id.clone(),
                scopes: mcp.scopes.clone(),
                storage_dir: directory.clone(),
                request_timeout: Duration::from_secs(5),
            },
            listen: loopback(&mcp.operator_listen.to_string(), true)?,
            pairing_file: config.screen.pairing_token_file.clone(),
        }))
    }
}

fn outside_repository(path: &Path) -> Result<(), Error> {
    if path.as_os_str().is_empty() || path.components().any(|c| c == std::path::Component::ParentDir) {
        return Err(Error::Input);
    }
    // Resolve existing ancestors so symlinks cannot place a new child in a checkout.
    let absolute = std::path::absolute(path).map_err(|_| Error::Input)?;
    let existing = absolute.ancestors().find(|p| p.exists()).ok_or(Error::Input)?;
    let resolved = existing.canonicalize().map_err(|_| Error::Input)?;
    if resolved.ancestors().any(|p| p.join(".git").exists()) {
        return Err(Error::Input);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_link_store_rejects_checkout_and_traversal_without_creating_files() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        std::fs::write(checkout.join(".git"), "gitdir: inert").unwrap();
        assert_eq!(outside_repository(&checkout.join("new/state")), Err(Error::Input));
        assert_eq!(outside_repository(&temp.path().join("new/../state")), Err(Error::Input));
        assert_eq!(outside_repository(Path::new("")), Err(Error::Input));
        assert_eq!(outside_repository(&temp.path().join("external/state")), Ok(()));
        assert!(!temp.path().join("external").exists());
        assert!(!checkout.join("new").exists());
    }
}
