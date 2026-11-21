use crate::{AuthError, store, valid_text};
use echo_contracts::{endpoint::InstallRequest, secret::Secret};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::sync::Mutex;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EndpointInstall {
    pub auth_link_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    pub device_name: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PendingExchange {
    idempotency_key: String,
    device_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_link_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Record {
    version: u8,
    device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    installation: Option<EndpointInstall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_exchange: Option<PendingExchange>,
}

/// The private record for the endpoint installation and its pending exchange.
pub struct EndpointRegistration {
    path: PathBuf,
    pub(super) record: Mutex<Record>,
}

impl EndpointRegistration {
    /// Load or allocate a stable UUID under the bridge's private state home.
    ///
    /// # Errors
    /// Refuses unreadable, malformed or non-durable local state.
    pub fn open(home: &Path) -> Result<Self, AuthError> {
        let directory = home.join("portos-endpoint");
        fs::create_dir_all(&directory).map_err(|_| AuthError::Storage)?;
        store::restrict_directory(&directory)?;
        let path = directory.join("registration.json");
        let (record, fresh) = match fs::read(&path) {
            Ok(bytes) => {
                let bytes = Secret::new(bytes);
                (
                    serde_json::from_slice(bytes.expose()).map_err(|_| AuthError::Storage)?,
                    false,
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
                Record {
                    version: 1,
                    device_id: new_request_id()?,
                    installation: None,
                    pending_exchange: None,
                },
                true,
            ),
            Err(_) => return Err(AuthError::Storage),
        };
        record.validate()?;
        let registration = Self {
            path,
            record: Mutex::new(record),
        };
        if fresh {
            let guard = registration.record.try_lock().map_err(|_| AuthError::Storage)?;
            registration.save(&guard)?;
        }
        Ok(registration)
    }

    pub(super) fn save(&self, record: &Record) -> Result<(), AuthError> {
        let bytes = Secret::new(serde_json::to_vec(record).map_err(|_| AuthError::Storage)?);
        store::atomic_write(&self.path, bytes.expose())
    }

    /// Read the currently committed install without probing Port OS.
    pub async fn installation(&self) -> Option<EndpointInstall> {
        self.record.lock().await.installation.clone()
    }

    /// Persist the exact next exchange before sending its request. An existing
    /// pending exchange always takes precedence over a later name change.
    ///
    /// # Errors
    /// Returns a storage error before sending anything.
    pub async fn exchange(&self, desired_name: &str) -> Result<Option<EndpointInstallExchange>, AuthError> {
        let mut record = self.record.lock().await;
        let pending = if let Some(pending) = &record.pending_exchange {
            pending.clone()
        } else {
            if record
                .installation
                .as_ref()
                .is_some_and(|value| value.device_name == desired_name)
            {
                return Ok(None);
            }
            let pending = PendingExchange {
                idempotency_key: new_request_id()?,
                device_name: desired_name.into(),
                auth_link_id: record.installation.as_ref().map(|value| value.auth_link_id.clone()),
            };
            record.pending_exchange = Some(pending.clone());
            if let Err(error) = self.save(&record) {
                record.pending_exchange = None;
                return Err(error);
            }
            pending
        };
        Ok(Some(EndpointInstallExchange {
            device_id: record.device_id.clone(),
            pending,
        }))
    }

    /// Commit a successful exchange before asking the route resolver.
    ///
    /// # Errors
    /// Refuses an obsolete receipt or failed persistence.
    pub async fn complete_exchange(
        &self,
        exchange: EndpointInstallExchange,
        auth_link_id: String,
    ) -> Result<(), AuthError> {
        let mut record = self.record.lock().await;
        if !valid_text(&auth_link_id)
            || record.pending_exchange.as_ref().is_none_or(|pending| {
                pending.idempotency_key != exchange.pending.idempotency_key
                    || pending
                        .auth_link_id
                        .as_ref()
                        .is_some_and(|prior| prior != &auth_link_id)
            })
        {
            return Err(AuthError::Protocol);
        }
        let route_id = record
            .installation
            .as_ref()
            .and_then(|install| install.route_id.clone());
        let previous = record.installation.replace(EndpointInstall {
            auth_link_id,
            route_id,
            device_name: exchange.pending.device_name.clone(),
        });
        record.pending_exchange = None;
        if let Err(error) = self.save(&record) {
            record.installation = previous;
            record.pending_exchange = Some(exchange.pending);
            return Err(error);
        }
        Ok(())
    }

    /// Save only a nonblank, separately resolved route for this installation.
    ///
    /// # Errors
    /// Refuses an invalid route, obsolete install or failed persistence.
    pub async fn resolve(&self, auth_link_id: &str, route_id: String) -> Result<(), AuthError> {
        let mut record = self.record.lock().await;
        if !valid_text(&route_id) || route_id == record.device_id {
            return Err(AuthError::Protocol);
        }
        let mut install = record
            .installation
            .clone()
            .filter(|value| value.auth_link_id == auth_link_id)
            .ok_or(AuthError::Protocol)?;
        install.route_id = Some(route_id);
        let previous = record.installation.replace(install);
        if let Err(error) = self.save(&record) {
            record.installation = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Remove the installation while retaining the stable UUID.
    ///
    /// # Errors
    /// Reports failed local persistence.
    pub async fn unlink(&self) -> Result<(), AuthError> {
        let mut record = self.record.lock().await;
        record.installation = None;
        record.pending_exchange = None;
        self.save(&record)
    }
}

impl Record {
    fn validate(&self) -> Result<(), AuthError> {
        if self.version != 1 || !valid_id(&self.device_id) {
            return Err(AuthError::Storage);
        }
        if let Some(install) = &self.installation
            && (!valid_text(&install.auth_link_id)
                || !valid_text(&install.device_name)
                || install
                    .route_id
                    .as_ref()
                    .is_some_and(|route| !valid_text(route) || route == &self.device_id))
        {
            return Err(AuthError::Storage);
        }
        if let Some(pending) = &self.pending_exchange
            && (!valid_id(&pending.idempotency_key)
                || !valid_text(&pending.device_name)
                || pending.auth_link_id.as_deref()
                    != self.installation.as_ref().map(|value| value.auth_link_id.as_str()))
        {
            return Err(AuthError::Storage);
        }
        Ok(())
    }
}

/// Exact durable exchange input and its idempotency key.
pub struct EndpointInstallExchange {
    device_id: String,
    pending: PendingExchange,
}

impl EndpointInstallExchange {
    #[must_use]
    pub fn key(&self) -> &str {
        &self.pending.idempotency_key
    }

    #[must_use]
    pub fn request(&self) -> InstallRequest<'_> {
        InstallRequest::new(
            &self.device_id,
            &self.pending.device_name,
            self.pending.auth_link_id.as_deref(),
        )
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Allocate a random version 4 UUID for an install or logical request.
///
/// # Errors
/// Returns a safe storage error if OS randomness is unavailable.
pub fn new_request_id() -> Result<String, AuthError> {
    let mut bytes = [0_u8; 16];
    OsRng.try_fill_bytes(&mut bytes).map_err(|_| AuthError::Storage)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]),
        u16::from_be_bytes([bytes[8], bytes[9]]),
        u64::from_be_bytes([0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]])
    ))
}
