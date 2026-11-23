//! Selected wire types adapted from rust-v0.160.0's experimental JSON schema.
// OpenAI Codex, Copyright 2025 OpenAI; Apache-2.0 (see ../NOTICE).
use echo_contracts::secret::Secret;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// IDs are signed integers or strings in the pinned schema.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    Text(String),
}

/// Inbound messages retain unknown notification parameters without projecting them.
/// Payload-bearing types deliberately do not implement Debug or Display.
pub enum Message {
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
        raw: Value,
    },
    Response {
        id: RequestId,
        result: Value,
    },
    Failure {
        id: RequestId,
        code: i64,
        retryable: bool,
    },
}

/// Decode an envelope without retaining untrusted error text in diagnostics.
///
/// # Errors
/// Returns a payload-free error for malformed or ambiguous envelopes.
pub fn decode(bytes: &[u8]) -> Result<Message, super::codex_rpc::RpcError> {
    use super::codex_rpc::RpcError;
    let value: Value = serde_json::from_slice(bytes).map_err(|_| RpcError::Malformed)?;
    let object = value.as_object().ok_or(RpcError::Malformed)?;
    if object.contains_key("jsonrpc") {
        return Err(RpcError::Malformed);
    }
    let id = object
        .get("id")
        .map(|id| serde_json::from_value(id.clone()))
        .transpose()
        .map_err(|_| RpcError::Malformed)?;
    let result = object.get("result");
    let failure = object.get("error");
    if let Some(method) = object.get("method") {
        if result.is_some() || failure.is_some() {
            return Err(RpcError::Malformed);
        }
        let method = method.as_str().ok_or(RpcError::Malformed)?.to_owned();
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        return Ok(match id {
            Some(id) => Message::Request { id, method, params },
            None => Message::Notification {
                method,
                params,
                raw: value,
            },
        });
    }
    let id = id.ok_or(RpcError::Malformed)?;
    match (result, failure) {
        (Some(result), None) => Ok(Message::Response {
            id,
            result: result.clone(),
        }),
        (None, Some(error)) => {
            let code = error.get("code").and_then(Value::as_i64).ok_or(RpcError::Malformed)?;
            if !error.get("message").is_some_and(Value::is_string) {
                return Err(RpcError::Malformed);
            }
            let retryable = transient_discovery(code, &error["message"]);
            Ok(Message::Failure { id, code, retryable })
        }
        _ => Err(RpcError::Malformed),
    }
}

// rust-v0.160.0's fixed transport categories; no raw text/data is retained.
fn transient_discovery(code: i64, message: &Value) -> bool {
    code == -32603
        && matches!(
            message.as_str(),
            Some("workspace routing discovery failed" | "workspace routing discovery timed out")
        )
}

/// Minimal initialization always opts into experimental methods.
#[must_use]
pub fn initialize_params() -> Value {
    serde_json::json!({
        "clientInfo": {"name": "portos_echo", "title": "Port OS Echo", "version": "0.1.0"},
        "capabilities": {"experimentalApi": true}
    })
}

/// The selected WebRTC-only realtime start surface. V3 is always explicit.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeStart<'a> {
    thread_id: &'a str,
    output_modality: &'static str,
    transport: Webrtc<'a>,
    version: &'static str,
    include_startup_context: bool,
    client_managed_handoffs: bool,
}

#[derive(Serialize)]
struct Webrtc<'a> {
    r#type: &'static str,
    sdp: &'a str,
}

impl<'a> RealtimeStart<'a> {
    #[must_use]
    pub fn new(thread_id: &'a str, sdp: &'a str) -> Self {
        Self {
            thread_id,
            output_modality: "audio",
            transport: Webrtc { r#type: "webrtc", sdp },
            version: "v3",
            include_startup_context: false,
            client_managed_handoffs: false,
        }
    }
}

/// Parameters used by realtime stop and thread-scoped notifications.
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadId {
    pub thread_id: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterrupt {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    pub namespace: Option<String>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolContent {
    InputText {
        text: String,
    },
    InputImage {
        #[serde(rename = "imageUrl")]
        image_url: String,
    },
    InputAudio {
        #[serde(rename = "audioUrl")]
        audio_url: String,
    },
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub content_items: Vec<ToolContent>,
    pub success: bool,
}

/// Only the two sign-in modes consumed by the bridge are projected.
/// Secret bytes are exposed solely to the bounded wire serializer.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LoginParams<'a> {
    ApiKey {
        #[serde(rename = "apiKey", serialize_with = "serialize_secret")]
        api_key: &'a Secret<String>,
    },
    ChatgptDeviceCode {},
}

fn serialize_secret<S: serde::Serializer>(value: &Secret<String>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.expose())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRead {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<bool>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LoginResult {
    ApiKey,
    ChatgptDeviceCode {
        #[serde(rename = "loginId")]
        login_id: String,
        #[serde(rename = "verificationUrl")]
        verification_url: String,
        #[serde(rename = "userCode")]
        user_code: Secret<String>,
    },
}

/// Selected account fields; routing/plan metadata stays available in the raw reply.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadResult {
    pub account: Option<Account>,
    pub requires_openai_auth: bool,
    pub workspace_routing: Option<Value>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Account {
    ApiKey,
    Chatgpt {
        email: Option<String>,
        #[serde(rename = "planType")]
        plan_type: String,
    },
}

/// Project the thread identifier needed for subsequent operations, and the
/// effective service tier when the app server reports one.
#[derive(Deserialize)]
pub struct ThreadStartResult {
    pub thread: Thread,
    /// The tier the server applied; an unsupported request is served as `default`.
    #[serde(default, rename = "serviceTier")]
    pub service_tier: Option<String>,
}

#[derive(Deserialize)]
pub struct Thread {
    pub id: String,
}

/// Known notifications are optional projections of a preserved raw envelope.
/// A future method remains an ordinary raw `Message::Notification`.
#[derive(Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum KnownNotification {
    #[serde(rename = "thread/realtime/started")]
    Started(RealtimeStarted),
    #[serde(rename = "thread/realtime/sdp")]
    Sdp(RealtimeSdp),
    #[serde(rename = "thread/realtime/itemAdded")]
    ItemAdded(RealtimeItem),
    #[serde(rename = "thread/realtime/transcript/delta")]
    TranscriptDelta(TranscriptDelta),
    #[serde(rename = "thread/realtime/transcript/done")]
    TranscriptDone(TranscriptDone),
    #[serde(rename = "thread/realtime/error")]
    Error(RealtimeError),
    #[serde(rename = "thread/realtime/closed")]
    Closed(RealtimeClosed),
    #[serde(rename = "account/login/completed")]
    LoginCompleted(LoginCompleted),
    #[serde(rename = "account/updated")]
    AccountUpdated(AccountUpdated),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeStarted {
    pub thread_id: String,
    pub version: String,
    pub realtime_session_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeSdp {
    pub thread_id: String,
    pub sdp: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeItem {
    pub thread_id: String,
    pub item: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptDelta {
    pub thread_id: String,
    pub role: String,
    pub delta: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptDone {
    pub thread_id: String,
    pub role: String,
    pub text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeError {
    pub thread_id: String,
    pub message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeClosed {
    pub thread_id: String,
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginCompleted {
    pub login_id: Option<String>,
    pub success: bool,
    pub error: Option<String>,
    pub onboarding_entrypoint: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUpdated {
    pub auth_mode: Option<String>,
    pub plan_type: Option<String>,
}
