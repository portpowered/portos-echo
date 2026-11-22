use std::time::Duration;

use tokio::time::Instant;

use super::*;

#[test]
fn t2_daemon_envelope_reference_bytes() {
    let request = Frame {
        id: 1,
        body: Body::Request(Request {
            verb: "POST".into(),
            path: "/link-control".into(),
            ..Request::default()
        }),
    };
    let reference = hex::decode("1219080112150a04504f5354120d2f6c696e6b2d636f6e74726f6c").unwrap();
    assert_eq!(encode_request(&request), reference);
    assert_eq!(decode_request(&reference), Ok(request));
    let response = Frame {
        id: 1,
        body: Body::Response(Response {
            status: 200,
            ..Response::default()
        }),
    };
    let reference = hex::decode("0a0708011a0308c801").unwrap();
    assert_eq!(decode_response(&reference), Ok(response));
    let reset = Frame {
        id: 7,
        body: Body::Reset {
            code: 1,
            reason: "x".into(),
        },
    };
    assert_eq!(encode_response(&reset), hex::decode("0a0908072a050801120178").unwrap());
}

#[test]
fn t2_chunk_limit_default_and_malformed_input() {
    let now = Instant::now();
    let mut assembler = Assembler::default();
    let mut writer = Packetizer::default();
    assert_eq!(writer.split(&[1, 2]).unwrap(), [[8, 1, 24, 1, 34, 2, 1, 2]]);
    assert_eq!(assembler.push(&[8, 1, 34, 2, 1, 2], now), Ok(Some(vec![1, 2])));
    assert_eq!(writer.split(&vec![3; 65_489]).unwrap().len(), 1);
    let data = vec![3; 65_490];
    let parts = writer.split(&data).unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(assembler.push(&parts[1], now), Ok(None));
    assert_eq!(assembler.push(&parts[0], now), Ok(Some(data)));
    let mut overflow = proto::Writer::default();
    overflow.bytes(4, &vec![0; 65_490]);
    assert_eq!(assembler.push(&overflow.finish(), now), Err(InvalidWire));
    for malformed in [b"\x18\x00".as_slice(), b"\x10\x01", b"\x08\xff", b"\x22\x02x", b"\0"] {
        assert_eq!(assembler.push(malformed, now), Err(InvalidWire));
    }
    assert_eq!(decode_response(&[0x0a, 0x03, 0x08]), Err(InvalidWire));
    assert_eq!(decode_request(&[0x08, 0x01]), Err(InvalidWire));
}

#[test]
fn t2_chunk_expiry_and_count_bound() {
    let now = Instant::now();
    let mut assembler = Assembler::default();
    for id in 1..=16 {
        assert_eq!(assembler.push(&[8, id, 24, 2, 34, 1, b'a'], now), Ok(None));
    }
    let extra = [8, 17, 24, 2, 34, 1, b'b'];
    assert_eq!(assembler.push(&extra, now), Err(InvalidWire));
    let later = now + Duration::from_secs(61);
    assert_eq!(assembler.push(&extra, later), Ok(None));
    assert_eq!(
        assembler.push(&[8, 17, 16, 1, 24, 2, 34, 1, b'c'], later),
        Ok(Some(b"bc".to_vec()))
    );
}

#[test]
fn t2_daemon_reset_isolated_from_other_streams() {
    let mut mux = StreamMux::default();
    let a = mux.open(Request::default()).unwrap().id;
    let b = mux.open(Request::default()).unwrap().id;
    let response = |id| Frame {
        id,
        body: Body::Response(Response {
            status: 200,
            ..Response::default()
        }),
    };
    let reset = Body::Reset {
        code: 1,
        reason: "done".into(),
    };
    assert_eq!(
        mux.accept(Frame { id: a, body: reset }),
        Some((
            a,
            Ok(Body::Reset {
                code: 1,
                reason: "done".into()
            })
        ))
    );
    assert_eq!(mux.accept(response(a)), None);
    assert!(!mux.forget(a));
    assert_eq!(mux.accept(response(b)), Some((b, Ok(response(b).body))));
    let final_body = || Body::Chunk {
        data: b"complete".to_vec(),
        end_body: true,
    };
    assert_eq!(
        mux.accept(Frame {
            id: b,
            body: final_body()
        }),
        Some((b, Ok(final_body())))
    );
    assert_eq!(mux.accept(response(b)), None);
}
