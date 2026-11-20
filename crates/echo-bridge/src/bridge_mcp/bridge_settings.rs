//! Offline grant configuration checks precede discovery and store creation.
use crate::{Error, bridge_input::loopback};
use echo_config::{BridgeConfig, McpAuth};
use echo_mcp_auth::AuthOptions;
use std::{net::SocketAddr, path::Path, time::Duration};

pub struct Settings {
    pub options: AuthOptions,
    pub ca_pem: Option<Vec<u8>>,
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
            ca_pem: Self::ca_pem(config)?,
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

    pub(crate) fn ca_pem(config: &BridgeConfig) -> Result<Option<Vec<u8>>, Error> {
        config
            .codex
            .ca_file
            .as_ref()
            .map(|path| std::fs::read(path).map_err(|_| Error::Input))
            .transpose()
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
    fn mcp_echo_example_validates_private_portos_projection() {
        let config =
            BridgeConfig::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/bridge.echo.toml")).unwrap();
        assert_eq!(config.mcp.server.len(), 1);
        let server = &config.mcp.server[0];
        assert_eq!(server.name, "portos");
        assert_eq!(server.auth, McpAuth::PortosDeviceCode);
        assert_eq!(server.url, "https://api.portoperatingsystem.com/mcp"); // lint-rules: allow-literal offline example projection, never contacted
        assert_eq!(
            server.allow_tools,
            [
                "control",
                "endpoint_query",
                "endpoint_show",
                "endpoint_manage",
                "group_query",
                "group_manage",
                "guide",
                "subscription_list",
                "subscription_inspect",
                "subscription_cancel",
                "subscription_label_edit"
            ]
        );
        assert_eq!(
            config.mcp.scopes,
            [
                "endpoint:read",
                "endpoint:write",
                "message:send",
                "group:read",
                "group:manage",
                "subscription:read",
                "subscription:write"
            ]
        );
        let private = Path::new("/data/local/echo-bridge/private");
        assert_eq!(config.mcp.state_dir, Some(private.join("portos")));
        assert_eq!(config.screen.pairing_token_file, private.join("screen-pairing.txt"));
        assert_eq!(config.codex.home, Some(private.join("codex-home")));
    }

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
