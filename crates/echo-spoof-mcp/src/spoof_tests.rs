use crate::spoof_wire::{catalog, valid_args, valid_envelope};
use serde_json::json;

#[test]
fn t2_catalog_is_exact_with_closed_argument_schemas() {
    assert_eq!(
        catalog(),
        json!([
            {"name":"test.echo","description":"Echo text","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}},
            {"name":"test.wait","description":"Wait for milliseconds","inputSchema":{"type":"object","properties":{"ms":{"type":"integer","minimum":0}},"required":["ms"],"additionalProperties":false}},
            {"name":"test.requires_approval","description":"Require approval","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
            {"name":"ui.show_emotion","description":"Show emotion","inputSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false}}
        ])
    );
}

#[test]
fn t1_tool_validation_preserves_unsigned_waits_and_closed_arguments() {
    for (name, args) in [
        ("test.echo", json!({"text":""})),
        ("test.wait", json!({"ms":0})),
        ("test.wait", json!({"ms":60_000})),
        ("test.requires_approval", json!({})),
        ("ui.show_emotion", json!({"name":"happy"})),
    ] {
        assert!(valid_args(name, &args));
    }
    assert!(valid_args("test.wait", &json!({"ms":u64::MAX})));
    for name in [
        "test.echo",
        "test.wait",
        "test.requires_approval",
        "ui.show_emotion",
        "test.missing",
    ] {
        for args in [
            json!(null),
            json!([]),
            json!({"extra":true}),
            json!({"text":1}),
            json!({"text":"yes","extra":0}),
        ] {
            assert!(!valid_args(name, &args));
        }
    }
    for ms in [json!(-1), json!(1.5), json!("7")] {
        assert!(!valid_args("test.wait", &json!({"ms":ms})));
    }
}

#[test]
fn t2_envelope_validation_accepts_only_supported_stateless_requests() {
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"client","version":"1"}}});
    assert!(valid_envelope(&init));
    for path in ["protocolVersion", "capabilities", "clientInfo"] {
        let mut invalid = init.clone();
        invalid["params"][path] = json!(null);
        assert!(!valid_envelope(&invalid));
    }
    assert!(valid_envelope(
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    ));
    assert!(!valid_envelope(
        &json!({"jsonrpc":"2.0","id":1,"method":"notifications/initialized"})
    ));
    for params in [json!({}), json!({"cursor":"x"})] {
        assert!(valid_envelope(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":params})
        ));
    }
    for params in [json!(null), json!({"cursor":1}), json!({"cursor":"x","extra":0})] {
        assert!(!valid_envelope(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":params})
        ));
    }
    for method in ["tools/call", "unknown"] {
        assert!(!valid_envelope(
            &json!({"jsonrpc":"2.0","id":1,"method":method,"params":{}})
        ));
    }
    for id in [json!(0), json!(-1), json!("1"), json!(null)] {
        let mut invalid = init.clone();
        invalid["id"] = id;
        assert!(!valid_envelope(&invalid));
    }
    let mut invalid = init;
    invalid["jsonrpc"] = json!("1.0");
    assert!(!valid_envelope(&invalid));
}
