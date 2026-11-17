use super::*;
use serde_json::json;

#[test]
fn exact_byte_limit_and_fragmented_utf8_decode() -> Result<(), RpcError> {
    let frame = br#"{"method":"future/notification","params":{"value":7}}"#;
    let mut decoder = Decoder::new(frame.len())?;
    for byte in frame {
        assert!(decoder.push(*byte)?.is_none());
    }
    let Some(Message::Notification { method, params, .. }) = decoder.push(b'\n')? else {
        return Err(RpcError::Malformed);
    };
    assert_eq!(method, "future/notification");
    assert_eq!(params, json!({"value": 7}));
    let value = json!({"method": "synthetic", "params": "é"});
    let encoded = encode(&value, 1024)?;
    for byte in &encoded[..encoded.len() - 1] {
        assert!(decoder.push(*byte)?.is_none());
    }
    let Some(Message::Notification { params, .. }) = decoder.push(b'\n')? else {
        return Err(RpcError::Malformed);
    };
    assert_eq!(params, json!("é"));
    Ok(())
}

#[test]
fn oversize_and_invalid_lines_close_decoder_without_payload_diagnostics() -> Result<(), RpcError> {
    let mut decoder = Decoder::new(2)?;
    decoder.push(b'{')?;
    decoder.push(b'}')?;
    assert!(matches!(decoder.push(b' '), Err(RpcError::LineTooLong)));
    assert!(matches!(decoder.push(b'\n'), Err(RpcError::Disconnected)));
    let mut decoder = Decoder::new(64)?;
    for byte in b"synthetic-private-content" {
        decoder.push(*byte)?;
    }
    let error = decoder.push(b'\n').err().ok_or(RpcError::Malformed)?;
    assert_eq!(error.to_string(), "Codex RPC: Malformed");
    assert!(matches!(decoder.push(b'\n'), Err(RpcError::Disconnected)));
    Ok(())
}

#[test]
fn partial_eof_is_terminal_and_newlines_are_not_optional() -> Result<(), RpcError> {
    let mut decoder = Decoder::new(64)?;
    for byte in b"{\"id\":1,\"result\":null}" {
        assert!(decoder.push(*byte)?.is_none());
    }
    assert_eq!(decoder.eof(), RpcError::Disconnected);
    assert!(matches!(decoder.push(b'\n'), Err(RpcError::Disconnected)));
    Ok(())
}

#[test]
fn encoder_counts_escaped_bytes_and_excludes_only_lf() -> Result<(), RpcError> {
    let value = json!({"result": "\n\"é"});
    let expected = serde_json::to_vec(&value).map_err(|_| RpcError::Malformed)?;
    let encoded = encode(&value, expected.len())?;
    assert_eq!(&encoded[..expected.len()], expected);
    assert_eq!(encoded.last(), Some(&b'\n'));
    assert_eq!(encode(&value, expected.len() - 1), Err(RpcError::LineTooLong));
    assert_eq!(encode(&value, 0), Err(RpcError::InvalidLimits));
    assert!(matches!(Decoder::new(0), Err(RpcError::InvalidLimits)));
    Ok(())
}

#[test]
fn ambiguous_and_wrongly_typed_envelopes_are_rejected() {
    for frame in [
        "[]",
        "{}",
        r#"{"method":7}"#,
        r#"{"method":"a","result":3}"#,
        r#"{"id":null,"result":3}"#,
        r#"{"id":1.5,"result":3}"#,
        r#"{"id":1,"result":3,"error":{"code":1,"message":"x"}}"#,
        r#"{"id":1,"error":{"code":"x","message":"x"}}"#,
        r#"{"id":1,"error":{"code":3}}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":3}"#,
    ] {
        assert!(matches!(decode(frame.as_bytes()), Err(RpcError::Malformed)), "{frame}");
    }
}

struct InvalidValue;
impl Serialize for InvalidValue {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("synthetic-private-content"))
    }
}

#[test]
fn serialization_failure_exposes_no_partial_frame_or_custom_error_text() {
    assert_eq!(encode(&InvalidValue, 128), Err(RpcError::Malformed));
}
