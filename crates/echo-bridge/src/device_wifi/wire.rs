use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

pub(super) const RESULT_LIMIT: u64 = 65_536;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Operation {
    Status,
    Scan,
}

impl Operation {
    #[cfg(target_os = "android")]
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Scan => "scan",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Error {
    #[serde(rename = "wifi-unavailable")]
    Unavailable,
    #[serde(rename = "wifi-unsupported")]
    Unsupported,
    #[serde(rename = "wifi-invalid-request")]
    Invalid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFrame {
    #[serde(rename = "type")]
    kind: String,
    data: RequestData,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestData {
    version: u32,
    op: Operation,
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
    Scan { networks: Vec<Network> },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Network {
    pub ssid: String,
    signal_dbm: i32,
    secured: bool,
    network_id: Option<u32>,
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

pub(super) fn valid_ssid(name: &str) -> bool {
    (1..=32).contains(&name.len()) && !name.contains('\0')
}

pub(super) fn request(bytes: &[u8]) -> Result<Operation, (Option<Operation>, Error)> {
    let request: RequestFrame = serde_json::from_slice(bytes).map_err(|_| {
        let op = serde_json::from_slice::<Value>(bytes).ok().and_then(|value| {
            if value["type"] != "wifi-request" {
                return None;
            }
            serde_json::from_value(value["data"]["op"].clone()).ok()
        });
        (op, Error::Invalid)
    })?;
    let op = request.data.op;
    if request.kind != "wifi-request" {
        return Err((None, Error::Invalid));
    }
    if request.data.version != 1 {
        return Err((Some(op), Error::Unsupported));
    }
    Ok(op)
}

pub(super) fn encode_request(op: Operation) -> Vec<u8> {
    json!({"type":"wifi-request","data":{"version":1,"op":op}})
        .to_string()
        .into_bytes()
}

pub(super) fn failure(op: Option<Operation>, error: Error) -> Vec<u8> {
    json!({"type":"wifi-result","data":{"version":1,"op":op,"ok":false,"error":error}})
        .to_string()
        .into_bytes()
}

pub(super) fn success(op: Operation, reading: Reading) -> Vec<u8> {
    let mut data = match (op, reading) {
        (Operation::Status, Reading::Status { ssid, reachable }) => {
            json!({"ssid":ssid,"reachable":reachable})
        }
        (Operation::Scan, Reading::Scan { networks }) => json!({"networks":networks}),
        _ => return failure(Some(op), Error::Unavailable),
    };
    data["version"] = json!(1);
    data["op"] = json!(op);
    data["ok"] = json!(true);
    json!({"type":"wifi-result","data":data}).to_string().into_bytes()
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
        assert_eq!(request(&encode_request(Operation::Status)).unwrap(), Operation::Status);
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
