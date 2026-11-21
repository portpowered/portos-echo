//! Port OS Echo endpoint install and message vocabulary.

use serde::{Deserialize, Serialize};

pub const ECHO_PLUGIN: &str = "port1/systems/zero/plugins/echo-show";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRequest<'a> {
    principal_id: &'static str,
    plugin_id: &'static str,
    plugin_authentication_data: DeviceIdentity<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_link_id: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceIdentity<'a> {
    device_id: &'a str,
    device_name: &'a str,
    model: &'static str,
}

impl<'a> InstallRequest<'a> {
    /// Build an install or a rename of the supplied installation.
    #[must_use]
    pub fn new(device_id: &'a str, device_name: &'a str, auth_link_id: Option<&'a str>) -> Self {
        Self {
            principal_id: "~self",
            plugin_id: ECHO_PLUGIN,
            plugin_authentication_data: DeviceIdentity {
                device_id,
                device_name,
                model: "Echo Show",
            },
            auth_link_id,
        }
    }

    #[must_use]
    pub fn auth_link_id(&self) -> Option<&str> {
        self.auth_link_id
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InstallResult {
    pub auth_link_id: String,
    pub id: String,
    pub etag: String,
    #[serde(rename = "status")]
    _status: InstallStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum InstallStatus {
    Succeeded,
}
