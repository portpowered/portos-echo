//! Public in-memory observations contain no header values.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct CallRecord {
    pub name: String,
    pub args: Value,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestRecord {
    pub method: String,
    pub id: Option<u64>,
    pub authorization_matches: Option<bool>,
    pub session_header: bool,
    pub dpop_header: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub requests: Vec<RequestRecord>,
    pub calls: Vec<CallRecord>,
}
