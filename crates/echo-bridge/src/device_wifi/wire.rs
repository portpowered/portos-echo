use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

pub(super) const RESULT_LIMIT: u64 = 65_536;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Operation {
    Status,
    Scan,
    Join,
    Forget,
}

pub(super) enum Request {
    Status,
    Scan,
    Join { ssid: String, password: Secret<String> },
    Forget { network_id: i32 },
}

impl Request {
    pub(super) fn operation(&self) -> Operation {
        match self {
            Self::Status => Operation::Status,
            Self::Scan => Operation::Scan,
            Self::Join { .. } => Operation::Join,
            Self::Forget { .. } => Operation::Forget,
        }
    }

    pub(super) fn data(&self) -> Value {
        let mut data = json!({"version":1,"op":self.operation()});
        match self {
            Self::Join { ssid, password } => {
                data["ssid"] = json!(ssid);
                data["password"] = json!(password.expose());
            }
            Self::Forget { network_id } => data["networkId"] = json!(network_id),
            Self::Status | Self::Scan => {}
        }
        data
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) enum Error {
    #[serde(rename = "wifi-unavailable")]
    Unavailable,
    #[serde(rename = "wifi-unsupported")]
    Unsupported,
    #[serde(rename = "wifi-invalid-request")]
    Invalid,
    #[serde(rename = "wifi-busy")]
    Busy,
    #[serde(rename = "wifi-not-saved")]
    NotSaved,
    #[serde(rename = "wifi-join-failed")]
    JoinFailed,
    #[serde(rename = "wifi-rollback-failed")]
    RollbackFailed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFrame {
    #[serde(rename = "type")]
    kind: String,
    data: RequestData,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
enum RequestData {
    Status {
        version: u32,
    },
    Scan {
        version: u32,
    },
    Join {
        version: u32,
        ssid: String,
        password: Secret<String>,
    },
    Forget {
        version: u32,
        #[serde(rename = "networkId")]
        network_id: i32,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResultFrame {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: ResultData,
}

#[derive(Deserialize)]
pub(super) struct ResultData {
    pub version: u32,
    pub op: Operation,
    pub ok: bool,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub(super) enum Reading {
    Status { ssid: Option<String>, reachable: bool },
    Scan { networks: Vec<ScanEntry> },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ScanEntry {
    pub ssid: String,
    pub signal_dbm: i32,
    pub secured: bool,
    pub network_id: Option<i32>,
}

impl Reading {
    pub(super) fn project(self) -> Self {
        match self {
            Self::Status { ssid, reachable } => {
                let ssid = ssid.filter(|name| valid_ssid(name));
                let reachable = ssid.is_some() && reachable;
                Reading::Status { ssid, reachable }
            }
            Self::Scan { mut networks } => {
                networks.retain(|net| valid_ssid(&net.ssid));
                networks.sort_by(|left, right| {
                    right
                        .signal_dbm
                        .cmp(&left.signal_dbm)
                        .then_with(|| left.ssid.cmp(&right.ssid))
                });
                let mut seen = HashSet::new();
                networks.retain(|net| seen.insert(net.ssid.clone()));
                networks.truncate(192);
                Reading::Scan { networks }
            }
        }
    }
}

impl From<Status> for Reading {
    fn from(value: Status) -> Self {
        Self::Status {
            ssid: value.ssid,
            reachable: value.reachable,
        }
    }
}

pub(super) fn valid_ssid(name: &str) -> bool {
    (1..=32).contains(&name.len()) && !name.contains('\0')
}

pub(super) fn request(bytes: &[u8]) -> Result<Request, (Option<Operation>, Error)> {
    let request: RequestFrame = serde_json::from_slice(bytes).map_err(|_| {
        let op = serde_json::from_slice::<Value>(bytes).ok().and_then(|value| {
            if value["type"] != "wifi-request" {
                return None;
            }
            serde_json::from_value(value["data"]["op"].clone()).ok()
        });
        (op, Error::Invalid)
    })?;
    if request.kind != "wifi-request" {
        return Err((None, Error::Invalid));
    }
    let (version, request) = match request.data {
        RequestData::Status { version } => (version, Request::Status),
        RequestData::Scan { version } => (version, Request::Scan),
        RequestData::Join {
            version,
            ssid,
            password,
        } => (version, Request::Join { ssid, password }),
        RequestData::Forget { version, network_id } => (version, Request::Forget { network_id }),
    };
    let op = Some(request.operation());
    if version != 1 {
        return Err((op, Error::Unsupported));
    }
    let valid = match &request {
        Request::Join { ssid, password } => {
            valid_ssid(ssid) && password.expose().len() <= 64 && !password.expose().contains('\0')
        }
        Request::Forget { network_id } => *network_id >= 0,
        Request::Status | Request::Scan => true,
    };
    valid.then_some(request).ok_or((op, Error::Invalid))
}

pub(super) fn encode_request(request: &Request) -> Vec<u8> {
    json!({"type":"wifi-request","data":request.data()})
        .to_string()
        .into_bytes()
}

pub(super) fn failure(op: Option<Operation>, error: Error) -> Vec<u8> {
    result(op, json!({"error":error}), false)
}

pub(super) fn success(op: Operation, reading: Reading) -> Vec<u8> {
    let data = match (op, reading) {
        (Operation::Status | Operation::Join | Operation::Forget, Reading::Status { ssid, reachable }) => {
            json!({"ssid":ssid,"reachable":reachable})
        }
        (Operation::Scan, Reading::Scan { networks }) => json!({"networks":networks}),
        _ => return failure(Some(op), Error::Unavailable),
    };
    result(Some(op), data, true)
}

fn result(op: Option<Operation>, mut data: Value, ok: bool) -> Vec<u8> {
    data["version"] = json!(1);
    data["op"] = json!(op);
    data["ok"] = json!(ok);
    json!({"type":"wifi-result","data":data}).to_string().into_bytes()
}

pub(super) fn join_result(outcome: Outcome) -> Vec<u8> {
    let op = Operation::Join;
    let (error, status) = match outcome {
        Outcome::Unavailable => return failure(Some(op), Error::Unavailable),
        Outcome::Connected(status) => return success(op, Reading::from(status).project()),
        Outcome::Rejected(status) => (Error::JoinFailed, status),
        Outcome::RestoreFailed(status) => (
            Error::RollbackFailed,
            status.unwrap_or(Status {
                ssid: None,
                reachable: false,
            }),
        ),
    };
    let Reading::Status { ssid, reachable } = Reading::from(status).project() else {
        return failure(Some(op), Error::Unavailable);
    };
    result(
        Some(op),
        json!({"error":error,"ssid":ssid,"reachable":reachable}),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_v1_projects_sorted_distinct_networks_and_disconnected_status() {
        let radio = serde_json::from_value::<Reading>(json!({
            "networks":[
                {"ssid":"Fixture A","signalDbm":-64,"secured":true,"networkId":7},
                {"ssid":"Fixture B","signalDbm":-42,"secured":true,"networkId":null},
                {"ssid":"Fixture B","signalDbm":-80,"secured":true,"networkId":null},
                {"ssid":"","signalDbm":-20,"secured":false,"networkId":null}
            ]
        }))
        .unwrap();
        let response: Value = serde_json::from_slice(&success(Operation::Scan, radio.project())).unwrap();
        assert_eq!(
            response,
            json!({"type":"wifi-result","data":{"version":1,"op":"scan","ok":true,"networks":[
                {"ssid":"Fixture B","signalDbm":-42,"secured":true,"networkId":null},
                {"ssid":"Fixture A","signalDbm":-64,"secured":true,"networkId":7}
            ]}})
        );
        let reading = Reading::Status {
            ssid: None,
            reachable: true,
        };
        let response: Value = serde_json::from_slice(&success(Operation::Status, reading.project())).unwrap();
        assert_eq!(
            response,
            json!({"type":"wifi-result","data":{
                "version":1,"op":"status","ok":true,"ssid":null,"reachable":false
            }})
        );
    }

    #[test]
    fn wifi_v1_rejects_unknown_version_wrong_kind_and_extra_fields() {
        assert_eq!(
            request(&encode_request(&Request::Status)).unwrap().operation(),
            Operation::Status
        );
        assert!(matches!(
            request(br#"{"type":"wifi-request","data":{"version":2,"op":"status"}}"#),
            Err((Some(Operation::Status), Error::Unsupported))
        ));
        assert!(matches!(
            request(br#"{"type":"wifi-request","data":{"version":1,"op":"scan","extra":null}}"#),
            Err((Some(Operation::Scan), Error::Invalid))
        ));
        assert!(matches!(request(br#"{"type":"ping"}"#), Err((None, Error::Invalid))));
    }
}
use echo_contracts::secret::Secret;
use echo_device::wifi::{Outcome, Status};
