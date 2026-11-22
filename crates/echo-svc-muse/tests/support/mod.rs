mod fake;
mod task_peer;

use std::{
    os::unix::fs::PermissionsExt as _,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use echo_agent_api::Task;
use echo_contracts::ids::DelegationId;
use serde_json::{Value, json};

pub(crate) use fake::{Fake, Pki, Vm};
pub(crate) use task_peer::*;

pub(crate) const SAVED_AT: u64 = 1_700_000_000;

pub(crate) fn epoch_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

pub(crate) fn pairing_record() -> Value {
    json!({
        "version": 1,
        "node_id": "gadget-fixture",
        "access_token": "synthetic-access",
        "refresh_token": "synthetic-refresh",
        "access_token_saved_at": SAVED_AT,
        "noise_static_key": STANDARD.encode([7; 32]),
    })
}

pub(crate) fn write_pairing(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

pub(crate) fn assert_catalogue(register: &Value) {
    assert_eq!(register["type"], "req");
    assert_eq!(register["method"], "link.register");
    let params = &register["params"];
    assert_eq!(params["node_id"], "gadget-fixture");
    assert_eq!(params["device_family"], "gadget");
    assert_eq!(params["platform"], "linux");
    assert_eq!(params["model_id"], "portos-echo");
    assert_eq!(params["is_wakeup_supported"], false);
    let catalogue = params["commands_v2"].as_object().unwrap();
    assert_eq!(catalogue.keys().collect::<Vec<_>>(), ["echo.report.result"]);
    let report = &catalogue["echo.report.result"];
    assert_eq!(report["timeout_ms"], 15000);
    assert_eq!(report["optional"], json!({}));
    let fields = report["required"].as_object().unwrap();
    assert_eq!(fields.keys().collect::<Vec<_>>(), ["status", "summary", "task_id"]);
    for field in fields.values() {
        assert_eq!(field["type"], "string");
    }
}

pub(crate) fn demo_task() -> Task {
    Task {
        delegation_id: DelegationId::new("synthetic-delegation").unwrap(),
        instructions: "Read the fixture".into(),
        context: json!({}),
        deadline_seconds: 2,
        max_tool_calls: 1,
    }
}

pub(crate) fn age_pairing(fake: &Fake, already_prefixed: bool) -> Vec<u8> {
    let mut record: Value = serde_json::from_slice(&std::fs::read(&fake.pairing).unwrap()).unwrap();
    record["access_token_saved_at"] = json!(SAVED_AT);
    if already_prefixed {
        record["refresh_token"] = json!("hatch_refresh:synthetic-refresh");
    }
    write_pairing(&fake.pairing, &record);
    std::fs::read(&fake.pairing).unwrap()
}

pub(crate) fn fresh_lease(id: &str, token: &str) -> Value {
    json!([{"vm_ws_url": "wss://127.0.0.1:1", "vm_auth_token": token, "vm_id": id}])
}

pub(crate) fn encode_control(message: &Value) -> Vec<u8> {
    let json = serde_json::to_vec(message).unwrap();
    let mut bytes = u32::try_from(json.len()).unwrap().to_le_bytes().to_vec();
    bytes.extend(json);
    bytes
}
