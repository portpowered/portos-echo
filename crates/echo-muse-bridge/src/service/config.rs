use std::io::Read;
use std::{
    net::SocketAddr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use echo_contracts::secret::Secret;
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub listen: SocketAddr,
    pub tls_certificate_der: PathBuf,
    pub tls_private_key_der: PathBuf,
    pub client_credential_file: PathBuf,
    pub store: PathBuf,
    pub report_socket: PathBuf,
    pub gadget_program: PathBuf,
    #[serde(default)]
    pub clock: ClockConfig,
}

#[derive(Default, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ClockConfig {
    #[default]
    System,
    ControlledStdin {
        initial_unix_seconds: u64,
    },
}

pub(super) fn private_file(path: &Path, max: u64) -> Result<Vec<u8>, &'static str> {
    private_path(path, false)?;
    let file = std::fs::File::open(path).map_err(|_| "private_file_unavailable")?;
    let meta = file.metadata().map_err(|_| "private_file_unavailable")?;
    if meta.len() > max {
        return Err("private_file_too_large");
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "private_file_unavailable")?;
    if bytes.len() as u64 > max {
        return Err("private_file_too_large");
    }
    Ok(bytes)
}

pub(super) fn private_path(path: &Path, directory: bool) -> Result<(), &'static str> {
    if !path.is_absolute() || path.components().any(|p| matches!(p, std::path::Component::ParentDir)) {
        return Err("unsafe_private_path");
    }
    // Refuse symlinks in every component, including parent aliases.
    let mut prefix = PathBuf::new();
    for part in path.components() {
        prefix.push(part);
        if prefix.join(".git").exists() {
            return Err("private_path_in_repository");
        }
        let meta = std::fs::symlink_metadata(&prefix).map_err(|_| "private_path_unavailable")?;
        if meta.file_type().is_symlink() {
            return Err("unsafe_private_path");
        }
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| "private_path_unavailable")?;
    if meta.mode() & 0o077 != 0 || meta.is_dir() != directory || (!directory && !meta.is_file()) {
        return Err("unsafe_private_permissions");
    }
    // Ownership must match the current process's private executable configuration.
    let owner = std::fs::metadata("/proc/self").map_err(|_| "owner_unavailable")?.uid();
    if meta.uid() != owner {
        return Err("unsafe_private_owner");
    }
    Ok(())
}

impl Config {
    pub fn load(path: &Path, control: bool) -> Result<Self, &'static str> {
        let config: Self = serde_json::from_slice(&private_file(path, 65536)?).map_err(|_| "invalid_config")?;
        if matches!(config.clock, ClockConfig::ControlledStdin { .. })
            && (!control || config.listen.ip() != std::net::Ipv4Addr::LOCALHOST)
        {
            return Err("invalid_clock_config");
        }
        if !config.gadget_program.is_absolute() || !config.gadget_program.is_file() {
            return Err("invalid_gadget_program");
        }
        for path in [&config.store, &config.report_socket] {
            private_path(path.parent().ok_or("invalid_config")?, true)?;
            if !path.is_absolute() {
                return Err("invalid_config");
            }
        }
        Ok(config)
    }

    pub fn credential(&self) -> Result<Secret<Vec<u8>>, &'static str> {
        let mut bytes = private_file(&self.client_credential_file, 257)?;
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        if !(32..=256).contains(&bytes.len()) || !bytes.iter().all(u8::is_ascii_graphic) {
            return Err("invalid_credential");
        }
        Ok(Secret::new(bytes))
    }

    pub fn tls(&self) -> Result<Arc<ServerConfig>, &'static str> {
        let certificate = CertificateDer::from(private_file(&self.tls_certificate_der, 65536)?);
        let key = Secret::new(private_file(&self.tls_private_key_der, 65536)?);
        let provider = rustls::crypto::ring::default_provider();
        let config = ServerConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .map_err(|_| "invalid_tls")?
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.expose().clone())),
            )
            .map_err(|_| "invalid_tls")?;
        Ok(Arc::new(config))
    }
}
