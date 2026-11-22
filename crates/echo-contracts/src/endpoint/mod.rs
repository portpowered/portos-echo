//! Port OS Echo endpoint install and message vocabulary (live contract 2026-11-21).
//!
//! Commands arrive from the device gateway as [`Frame`]s. Everything the bridge
//! says goes to `POST /messages` as one `report-state` [`Uplink`] whose payload
//! is a strict [`Report`]: Port OS refuses unknown fields with 400.

use serde::{Deserialize, Serialize};

mod session;
pub use session::{EndSession, EndSessionReply, EndedReason, SessionEnded, StartSession, StartSessionReply};

/// The `pluginId` of the published echo-show plugin, used by the pairing exchange.
pub const ECHO_PLUGIN_ID: &str = "port1/systems/zero/plugins/15badf8f-fe9b-4bf0-be33-2d7eb287f89f";
/// The header namespace of every bridge report. It is not the plugin id.
pub const REPORT_NAMESPACE: &str = "port1/systems/zero/plugins/echo-show";
/// The only report name the plugin accepts.
pub const REPORT_STATE: &str = "report-state";
/// Capability interface namespaces carry this prefix in command frames.
pub const CAPABILITY_PREFIX: &str = "port1/systems/zero/capability-interfaces/";
pub const VERSION: &str = "1.0";
/// Snapshot replies carry at most 4 MiB of JPEG.
pub const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
/// Session ids are at most 128 characters on the wire.
pub const MAX_SESSION_ID: usize = 128;
/// The REST scopes the endpoint needs: pairing, route read-back and reports.
pub const ENDPOINT_SCOPES: [&str; 3] = ["message:send", "endpoint:write", "endpoint:read"];

#[derive(Deserialize)]
pub struct SetVolume {
    pub volume: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdjustVolume {
    pub volume_delta: f64,
}

#[derive(Deserialize)]
pub struct SetOutputMute {
    pub mute: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSnapshot {
    /// The host republishes or inlines the bytes as the caller asked; the
    /// bridge always replies with inline base64.
    #[serde(default)]
    pub response_format: Option<SnapshotFormat>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SnapshotFormat {
    Url,
    Base64,
}

/// One gateway command: one WebSocket text message.
#[derive(Deserialize)]
pub struct Frame {
    pub header: FrameHeader,
    #[serde(default)]
    pub body: serde_json::Value,
    pub context: FrameContext,
}

#[derive(Deserialize)]
pub struct FrameHeader {
    pub namespace: String,
    pub name: String,
    pub version: String,
    /// The command id. Replies carry it as `requestId`.
    pub id: String,
}

impl FrameHeader {
    /// The capability interface name, such as `snapshot`, when the namespace is canonical.
    #[must_use]
    pub fn interface(&self) -> Option<&str> {
        self.namespace.strip_prefix(CAPABILITY_PREFIX)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameContext {
    pub device_id: String,
    #[serde(default)]
    pub sent_at: Option<String>,
}

#[derive(Serialize)]
pub struct Uplink<'a, T> {
    header: UplinkHeader,
    target: Route<'a>,
    payload: T,
}

#[derive(Serialize)]
struct UplinkHeader {
    namespace: &'static str,
    name: &'static str,
    version: &'static str,
}

#[derive(Serialize)]
struct Route<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    value: &'a str,
}

impl<'a, T> Uplink<'a, T> {
    /// A `report-state` message for the paired route, whose value is the bridge's `deviceId`.
    #[must_use]
    pub fn report(device_id: &'a str, payload: T) -> Self {
        Self {
            header: UplinkHeader {
                namespace: REPORT_NAMESPACE,
                name: REPORT_STATE,
                version: VERSION,
            },
            target: Route {
                kind: "ROUTE",
                value: device_id,
            },
            payload,
        }
    }
}

/// The strict `report-state` payload. Every part is optional; at least one is present.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampled_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<SpeakerReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant: Option<AssistantReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub reply: Option<Reply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_ended: Option<SessionEnded>,
}

impl Report {
    /// One reply to the command whose header id is `request_id`.
    #[must_use]
    pub fn reply(request_id: &str, reply: Reply) -> Self {
        Self {
            request_id: Some(request_id.into()),
            reply: Some(reply),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SpeakerReport {
    pub volume: u8,
    pub muted: bool,
}

/// Session id and origin appear only with an active state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantReport {
    pub state: AssistantState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOrigin>,
}

impl AssistantReport {
    #[must_use]
    pub fn new(state: AssistantState, session: Option<(&str, SessionOrigin)>) -> Self {
        let session = session.filter(|_| state.active());
        Self {
            state,
            session_id: session.map(|(id, _)| id.into()),
            origin: session.map(|(_, origin)| origin),
        }
    }
}

/// At most one reply per report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Reply {
    Snapshot(SnapshotReply),
    StartSession(StartSessionReply),
    EndSession(EndSessionReply),
    Error(ErrorReply),
}

impl Reply {
    /// A reason-code refusal without free text; the plugin writes the message.
    #[must_use]
    pub fn refused(code: &'static str) -> Self {
        Self::Error(ErrorReply {
            code,
            session_id: None,
            origin: None,
        })
    }

    /// The endpoint is busy with the named session.
    #[must_use]
    pub fn busy(session_id: &str, origin: SessionOrigin) -> Self {
        Self::Error(ErrorReply {
            code: "BUSY",
            session_id: Some(session_id.into()),
            origin: Some(origin),
        })
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Error(error) => error.code,
            _ => "OK",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotReply {
    pub width: u32,
    pub height: u32,
    /// Base64 of a JPEG of at most [`MAX_SNAPSHOT_BYTES`].
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorReply {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOrigin>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AssistantState {
    Idle,
    Listening,
    Thinking,
    Speaking,
    Muted,
}

impl AssistantState {
    /// Listening, thinking and speaking describe an active session.
    #[must_use]
    pub fn active(self) -> bool {
        matches!(self, Self::Listening | Self::Thinking | Self::Speaking)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
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
    /// Build an install or a rename (renewal) of the supplied installation.
    #[must_use]
    pub fn new(device_id: &'a str, device_name: &'a str, auth_link_id: Option<&'a str>) -> Self {
        Self {
            principal_id: "~self",
            plugin_id: ECHO_PLUGIN_ID,
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

/// Unknown response fields are ignored: Port OS may add to its replies.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// One `GET /routes?owner=~caller` page (`EnumerateRoutesResponse`). Only the
/// fields the bridge reads. The cursor is nested:
/// `{"results":[..],"paginationContext":{"nextToken":".."}}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutePage {
    #[serde(default)]
    pub results: Vec<RouteRecord>,
    #[serde(default)]
    pagination_context: Option<PaginationContext>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaginationContext {
    #[serde(default)]
    next_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteRecord {
    #[serde(default)]
    pub associated_plugin_route_id: Option<String>,
    #[serde(default)]
    pub associated_endpoint_id: Option<String>,
    #[serde(default)]
    pub associated_auth_link_id: Option<String>,
}

impl RoutePage {
    /// The opaque `paginationContext.nextToken`, passed back unchanged as the
    /// `nextToken` query parameter. `None` (absent or empty) on the last page.
    #[must_use]
    pub fn next_token(&self) -> Option<&str> {
        self.pagination_context
            .as_ref()
            .and_then(|context| context.next_token.as_deref())
            .filter(|token| !token.is_empty())
    }

    /// The endpoint id of the route whose plugin route id is this device's
    /// `deviceId` and whose auth link is the current pairing's `authLinkId`,
    /// so a route an old pairing left for the same device never matches.
    /// Never derived.
    #[must_use]
    pub fn endpoint_for(&self, device_id: &str, auth_link_id: &str) -> Option<&str> {
        self.results
            .iter()
            .find(|route| {
                route.associated_plugin_route_id.as_deref() == Some(device_id)
                    && route.associated_auth_link_id.as_deref() == Some(auth_link_id)
            })
            .and_then(|route| route.associated_endpoint_id.as_deref())
            .filter(|id| !id.trim().is_empty())
    }
}
