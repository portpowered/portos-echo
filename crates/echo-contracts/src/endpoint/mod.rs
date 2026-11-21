//! Port OS Echo endpoint install and message vocabulary.

use serde::{Deserialize, Serialize};

mod session;
pub use session::{EndSession, EndSessionOutcome, EndedReason, SessionEnded, StartSession, StartSessionOutcome};

pub const ECHO_PLUGIN: &str = "port1/systems/zero/plugins/echo-show";
pub const VERSION: &str = "1.0";

#[derive(Deserialize)]
pub struct SetVolume {
    pub volume: u8,
}

#[derive(Deserialize)]
pub struct AdjustVolume {
    pub delta: i64,
}

#[derive(Deserialize)]
pub struct SetOutputMute {
    pub mute: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSnapshot {
    #[serde(rename = "responseFormat")]
    pub format: SnapshotFormat,
}

#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SnapshotFormat {
    Base64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub image: SnapshotImage,
    pub time_of_sample: String,
}

#[derive(Serialize)]
pub struct SnapshotImage {
    #[serde(rename = "mimeType")]
    mime_type: &'static str,
    data: String,
    width: u32,
    height: u32,
}

impl SnapshotImage {
    #[must_use]
    pub fn jpeg(data: String, width: u32, height: u32) -> Self {
        Self {
            mime_type: "image/jpeg",
            data,
            width,
            height,
        }
    }
}

#[derive(Deserialize)]
pub struct Downlink {
    pub header: CommandHeader,
    pub target: CommandTarget,
    pub payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandHeader {
    pub namespace: String,
    pub name: String,
    pub version: String,
    pub message_id: String,
}

#[derive(Deserialize)]
pub struct CommandTarget {
    #[serde(rename = "type")]
    _kind: RouteType,
    pub value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum RouteType {
    Route,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReply<'a, T> {
    pub request_id: &'a str,
    pub outcome: T,
}

#[derive(Serialize)]
pub struct Uplink<'a, T> {
    header: UplinkHeader<'a>,
    target: Route<'a>,
    payload: T,
}

#[derive(Serialize)]
struct UplinkHeader<'a> {
    namespace: &'static str,
    name: &'a str,
    version: &'static str,
}

#[derive(Serialize)]
struct Route<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    value: &'a str,
}

impl<'a, T> Uplink<'a, T> {
    #[must_use]
    pub fn new(name: &'a str, route_id: &'a str, payload: T) -> Self {
        Self {
            header: UplinkHeader {
                namespace: ECHO_PLUGIN,
                name,
                version: VERSION,
            },
            target: Route {
                kind: "ROUTE",
                value: route_id,
            },
            payload,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportState<'a> {
    pub state: AssistantState,
    pub session_id: Option<&'a str>,
    pub origin: Option<SessionOrigin>,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantState {
    Idle,
    Listening,
    Thinking,
    Speaking,
    Muted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    Wake,
    Remote,
}

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
