use serde::Deserialize;
use x25519_dalek::StaticSecret;

use super::{Initiator, InvalidNoise, responder::Responder};

#[derive(Deserialize)]
struct Vector {
    protocol_name: String,
    init_prologue: String,
    init_static: String,
    init_ephemeral: String,
    resp_prologue: String,
    resp_static: String,
    resp_ephemeral: String,
    handshake_hash: String,
    messages: [Message; 6],
}

#[derive(Deserialize)]
struct Message {
    payload: String,
    ciphertext: String,
}

fn secret(text: &str) -> StaticSecret {
    StaticSecret::from(<[u8; 32]>::try_from(hex::decode(text).unwrap()).unwrap())
}

#[test]
fn t2_noise_vectors() {
    let vector: Vector = serde_json::from_str(include_str!("../../tests/fixtures/noise-xx.json")).unwrap();
    assert_eq!(vector.protocol_name, "Noise_XX_25519_AESGCM_SHA256");
    let initiator_static = secret(&vector.init_static);
    let [one, two, three, four, five, six] = vector.messages.map(|message| {
        (
            hex::decode(message.payload).unwrap(),
            hex::decode(message.ciphertext).unwrap(),
        )
    });
    let (initiator, first) = Initiator::start(
        &initiator_static,
        secret(&vector.init_ephemeral),
        &hex::decode(vector.init_prologue).unwrap(),
        &one.0,
    )
    .unwrap();
    assert_eq!(first, one.1);
    let (responder, payload, second) = Responder::reply(
        &secret(&vector.resp_static),
        secret(&vector.resp_ephemeral),
        &hex::decode(vector.resp_prologue).unwrap(),
        &first,
        &two.0,
    )
    .unwrap();
    assert_eq!((payload, &second), (one.0, &two.1));
    let (payload, third, mut client) = initiator.finish(&second, &three.0).unwrap();
    assert_eq!((payload, &third), (two.0, &three.1));
    let (payload, mut peer) = responder.finish(&third).unwrap();
    assert_eq!(payload, three.0);
    assert_eq!(hex::encode(client.hash), vector.handshake_hash);
    assert_eq!(hex::encode(peer.hash), vector.handshake_hash);
    for (sender, receiver, (plaintext, ciphertext)) in [
        (&mut peer.send, &mut client.receive, four),
        (&mut client.send, &mut peer.receive, five),
    ] {
        assert_eq!(sender.encrypt(&plaintext, &[]), Ok(ciphertext.clone()));
        assert_eq!(receiver.decrypt(ciphertext, &[]), Ok(plaintext));
    }
    assert_eq!(peer.send.encrypt(&six.0, &[]), Ok(six.1.clone()));
    assert_eq!(client.receive.decrypt(six.1, &[]), Ok(six.0));
    assert_eq!(client.receive.decrypt(vec![0; 16], &[]), Err(InvalidNoise));
    println!("{} vectors: 1 passed (6 messages)", vector.protocol_name);
}

#[test]
fn t2_noise_rejects_low_order_and_tampered_handshakes() {
    let key = StaticSecret::from([1; 32]);
    let (initiator, _) = Initiator::start(&key, StaticSecret::from([2; 32]), &[], &[]).unwrap();
    assert!(matches!(initiator.finish(&[0; 96], &[]), Err(InvalidNoise)));
    let (initiator, first) = Initiator::start(&key, StaticSecret::from([2; 32]), &[], &[]).unwrap();
    let (_, _, mut second) = Responder::reply(&key, StaticSecret::from([3; 32]), &[], &first, &[]).unwrap();
    *second.last_mut().unwrap() ^= 1;
    assert!(matches!(initiator.finish(&second, &[]), Err(InvalidNoise)));
}
