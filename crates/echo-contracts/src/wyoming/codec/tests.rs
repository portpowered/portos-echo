use super::*;
use proptest::prelude::*;
use serde_json::{Value, json};

fn event(inline: Option<Value>, data: Option<Value>, payload: Vec<u8>) -> Event {
    Event {
        header: Header {
            r#type: "probe".into(),
            data: inline,
            data_length: data.as_ref().map_or(0, |v| v.to_string().len()),
            payload_length: payload.len(),
        },
        data,
        payload,
    }
}

#[test]
fn every_split_and_bytewise_stream() {
    let expected = event(
        Some(json!({"inline": "\u{e9}"})),
        Some(json!(["\u{6c34}", null])),
        vec![0, 10, 255],
    );
    let wire = encode(&expected);
    for split in 0..=wire.len() {
        let mut decoder = Decoder::new();
        let mut got = decoder.feed(&wire[..split]).unwrap();
        got.extend(decoder.feed(&wire[split..]).unwrap());
        assert_eq!(got, std::slice::from_ref(&expected));
        decoder.finish().unwrap();
    }
    let mut decoder = Decoder::new();
    let got: Vec<_> = wire.iter().flat_map(|b| decoder.feed(&[*b]).unwrap()).collect();
    assert_eq!(got, [expected]);
    decoder.finish().unwrap();
}

#[test]
fn ordered_frames_and_trailing_partial() {
    let first = event(None, None, vec![]);
    let second = event(Some(Value::Null), Some(json!(true)), vec![10]);
    let wire = encode(&second);
    let mut stream = encode(&first);
    stream.extend_from_slice(&wire[..wire.len() - 1]);
    let mut decoder = Decoder::new();
    assert_eq!(decoder.feed(&stream).unwrap(), [first]);
    assert_eq!(decoder.feed(&[]).unwrap(), []);
    assert_eq!(decoder.feed(&wire[wire.len() - 1..]).unwrap(), [second]);
    decoder.finish().unwrap();
}

#[test]
fn missing_lengths_null_and_stale_lengths() {
    for (wire, data) in [
        ("{\"type\":\"pong\"}\n", None),
        ("{\"type\":\"pong\",\"data\":null}\n", Some(Value::Null)),
        ("{\"type\":\"pong\",\"data\":{}}\n", Some(json!({}))),
    ] {
        let got = Decoder::new().feed(wire.as_bytes()).unwrap();
        assert_eq!(
            got,
            [Event {
                header: Header {
                    r#type: "pong".into(),
                    data,
                    data_length: 0,
                    payload_length: 0
                },
                data: None,
                payload: vec![]
            }]
        );
    }
    let expected = event(None, Some(json!("\u{e9}\n")), vec![0, 10]);
    let mut stale = expected.clone();
    stale.header.data_length = 999;
    stale.header.payload_length = 999;
    assert_eq!(Decoder::new().feed(&encode(&stale)).unwrap(), [expected]);
}

#[test]
fn malformed_frames_poison() {
    for wire in [
        b"bad\n".to_vec(),
        b"{\"type\":3}\n".to_vec(),
        b"{\"type\":\"x\",\"payload_length\":-1}\n".to_vec(),
        b"{\"type\":\"x\",\"data_length\":1.5}\n".to_vec(),
        b"{\"type\":\"x\",\"payload_length\":18446744073709551616}\n".to_vec(),
    ] {
        let mut decoder = Decoder::new();
        let error = decoder.feed(&wire).unwrap_err();
        assert!(matches!(error, CodecError::Header(_)));
        assert!(error.to_string().starts_with("invalid header:"));
        assert!(matches!(decoder.feed(b""), Err(CodecError::Poisoned)));
        assert!(matches!(decoder.finish(), Err(CodecError::Poisoned)));
    }
}

#[test]
fn overflow_and_invalid_external_json() {
    for field in ["data_length", "payload_length"] {
        let wire = format!("{{\"type\":\"x\",\"{field}\":{}}}\n", usize::MAX);
        assert!(matches!(
            Decoder::new().feed(wire.as_bytes()),
            Err(CodecError::LengthOverflow)
        ));
    }
    let mut decoder = Decoder::new();
    let error = decoder.feed(b"{\"type\":\"x\",\"data_length\":1}\nx").unwrap_err();
    assert!(matches!(error, CodecError::Data(_)));
    assert!(error.to_string().starts_with("invalid external JSON data:"));
    assert_eq!(Decoder::new().feed(b"{\"type\":\"pong\"}\n").unwrap().len(), 1);
}

#[test]
fn truncated_header_data_and_payload() {
    let wire = encode(&event(None, Some(json!({"x":1})), vec![1, 2]));
    for end in 1..wire.len() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(&wire[..end]).unwrap(), []);
        assert!(matches!(decoder.finish(), Err(CodecError::Truncated)));
        assert!(matches!(decoder.feed(b""), Err(CodecError::Poisoned)));
    }
    assert_eq!(CodecError::Truncated.to_string(), "end of input in a partial frame");
    assert_eq!(
        CodecError::Poisoned.to_string(),
        "decoder is poisoned by an earlier error"
    );
    assert_eq!(
        CodecError::LengthOverflow.to_string(),
        "frame lengths overflow address space"
    );
}

fn json_values() -> BoxedStrategy<Value> {
    proptest::strategy::Union::new(vec![
        Just(Value::Null).boxed(),
        any::<bool>().prop_map(Value::from).boxed(),
        any::<i64>().prop_map(Value::from).boxed(),
        ".*".prop_map(Value::from).boxed(),
    ])
    .prop_recursive(3, 32, 5, |inner| {
        proptest::strategy::Union::new(vec![
            proptest::collection::vec(inner.clone(), 0..5)
                .prop_map(Value::Array)
                .boxed(),
            proptest::collection::btree_map(".*", inner, 0..5)
                .prop_map(|map| Value::Object(map.into_iter().collect()))
                .boxed(),
        ])
    })
    .boxed()
}

#[test]
fn arbitrary_frames() {
    let strategy = (
        ".*",
        proptest::option::of(json_values()),
        proptest::option::of(json_values()),
        proptest::collection::vec(any::<u8>(), 0..=65536),
    );
    proptest::test_runner::TestRunner::default()
        .run(&strategy, |(kind, inline, data, payload)| {
            let mut expected = event(inline, data, payload);
            expected.header.r#type = kind;
            let mut decoder = Decoder::new();
            assert_eq!(decoder.feed(&encode(&expected)).unwrap(), [expected]);
            decoder.finish().unwrap();
            Ok(())
        })
        .unwrap();
}

#[test]
fn explicit_payload_boundaries() {
    for size in [0, 1, 65536] {
        let expected = event(Some(Value::Null), Some(Value::Null), vec![10; size]);
        assert_eq!(Decoder::new().feed(&encode(&expected)).unwrap(), [expected]);
    }
}
