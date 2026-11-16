use super::*;
use serde_json::json;

fn trace(records: &[Value]) -> Trace {
    let mut trace = Trace::default();
    trace.append(&wire(records)).unwrap();
    trace
}

fn wire(records: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn journey() -> Vec<Value> {
    vec![
        json!({"event":"screen.listening","elapsed_ms":1,"address":"127.0.0.1:49152"}),
        json!({"event":"session.delegation.created","elapsed_ms":2,"run_id":"r-a","delegation_id":"d-a"}),
        json!({"event":"tools/call","elapsed_ms":3,"run_id":"r-a","delegation_id":"d-a","call_id":"c-a","name":"test.echo","count":1}),
        json!({"event":"session.commentary.append","elapsed_ms":4,"run_id":"r-a","delegation_id":"d-a"}),
        json!({"event":"timing","elapsed_ms":5,"run_id":"r-a","duration_ms":1499,"name":"first_audio_after_endpoint_ms"}),
    ]
}

fn recorded(records: &[Value]) -> Vec<u8> {
    format!("{}\n", normalize(&trace(records)).unwrap().join("\n")).into_bytes()
}

#[test]
fn normalizes_only_incidental_values_and_keeps_measurements_and_relationships() {
    let mut records = journey();
    records.push(
        json!({"event":"next","elapsed_ms":8,"run_id":"r-b","delegation_id":"d-b",
        "nested":[{"run_id":"r-a","call_id":"c-a","elapsed_ms":123}],
        "health_addr":"127.0.0.1:49153","address":"127.0.0.1:49152"}),
    );
    let actual = normalize(&trace(&records)).unwrap();
    let values = actual
        .iter()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values[0]["address"], "127.0.0.1:port-1");
    assert_eq!(values[2]["call_id"], "call_id-1");
    assert_eq!(values[4]["duration_ms"], 1499);
    assert_eq!(
        values[5],
        json!({"event":"next","elapsed_ms":0,"run_id":"run_id-2",
        "delegation_id":"delegation_id-2","nested":[{"run_id":"run_id-1","call_id":"call_id-1","elapsed_ms":123}],
        "health_addr":"127.0.0.1:port-2","address":"127.0.0.1:port-1"})
    );
    let expected = recorded(&records);
    for (index, record) in records.iter_mut().enumerate() {
        record["elapsed_ms"] = json!(index + 100);
        for key in ["run_id", "delegation_id", "call_id"] {
            if let Some(text) = record[key].as_str() {
                record[key] = json!(format!("renamed-{text}"));
            }
        }
    }
    records[0]["address"] = json!("127.0.0.1:51000");
    records[5]["address"] = json!("127.0.0.1:51000");
    records[5]["health_addr"] = json!("127.0.0.1:51001");
    records[5]["nested"][0]["run_id"] = json!("renamed-r-a");
    records[5]["nested"][0]["call_id"] = json!("renamed-c-a");
    assert_eq!(compare(&trace(&records), &expected), Ok(()));
}

#[test]
fn any_semantic_change_fails_with_expected_actual_and_line_number() {
    let original = journey();
    let expected = recorded(&original);
    for (key, value) in [
        ("count", json!(2)),
        ("name", json!("test.wait")),
        ("event", json!("tools/other")),
        ("run_id", json!("other-run")),
        ("delegation_id", json!("other-delegation")),
    ] {
        let mut altered = original.clone();
        altered[2][key] = value;
        let error = compare(&trace(&altered), &expected).unwrap_err();
        assert!(error.contains("line 3\n- "), "{error}");
        assert!(error.contains("\n+ "), "{error}");
    }
    let mut altered = original.clone();
    altered[4]["duration_ms"] = json!(1501);
    assert!(
        compare(&trace(&altered), &expected)
            .unwrap_err()
            .contains("\"duration_ms\":1501")
    );
    let mut altered = original.clone();
    // Keep timestamps legal so this specifically proves record-order checking.
    altered[1]["elapsed_ms"] = json!(3);
    altered.swap(1, 2);
    assert!(compare(&trace(&altered), &expected).unwrap_err().contains("line 2"));
    for altered in [&original[..4], &original[..0]] {
        assert!(compare(&trace(altered), &expected).is_err());
    }
    let mut altered = original.clone();
    altered.push(original[4].clone());
    let error = compare(&trace(&altered), &expected).unwrap_err();
    assert!(error.contains("line 6\n- <missing>\n+ "), "{error}");
    assert!(
        compare(&trace(&original), &recorded(&original[..4]))
            .unwrap_err()
            .contains("line 5")
    );
}

#[test]
fn missing_partial_malformed_and_noncanonical_goldens_cannot_pass() {
    let original = trace(&journey());
    for expected in [b"".as_slice(), b"{}\n", b"not-json\n", b"{\"event\":"] {
        assert!(compare(&original, expected).is_err());
    }
    assert!(
        compare(&original, &wire(&journey()))
            .unwrap_err()
            .contains("golden: trace differs")
    );
    let mut partial = Trace::default();
    partial.append(b"{\"event\":").unwrap();
    assert_eq!(normalize(&partial).unwrap_err(), "trace: incomplete final JSON line");
    let mut invalid = Trace::default();
    assert!(invalid.append(b"{}\n").is_err());
    assert_eq!(normalize(&invalid).unwrap_err(), "trace: previously invalid trace");
}

#[test]
fn invalid_identity_and_non_loopback_or_unbound_endpoints_are_rejected() {
    for endpoint in ["bad", "localhost:1", "127.0.0.1:0", "[::1]:1"] {
        let mut records = journey();
        records[0]["address"] = json!(endpoint);
        assert!(normalize(&trace(&records)).is_err());
    }
    for identity in [json!(""), json!(7), json!({})] {
        let mut records = journey();
        records[1]["delegation_id"] = identity;
        assert_eq!(normalize(&trace(&records)).unwrap_err(), "golden: invalid identity");
    }
}

#[test]
fn endpoint_and_call_relationship_changes_and_new_fields_are_not_hidden() {
    let mut records = journey();
    records.push(json!({"event":"next","elapsed_ms":6,"run_id":"r-a","call_id":"c-a",
        "address":"127.0.0.1:49152"}));
    let expected = recorded(&records);
    for (key, value) in [
        ("call_id", json!("c-b")),
        ("address", json!("127.0.0.1:49153")),
        ("new_field", json!(true)),
    ] {
        let mut changed = records.clone();
        changed[5][key] = value;
        assert!(compare(&trace(&changed), &expected).unwrap_err().contains("line 6"));
    }
}
