//! Noise XX initiator. TLS authenticates the VM identity before these messages.

mod symmetric;

#[cfg(test)]
pub(crate) mod responder;
#[cfg(test)]
mod vectors;

pub(crate) use symmetric::Cipher;
use symmetric::Symmetric;
use x25519_dalek::{PublicKey, StaticSecret};

const KEY_BYTES: usize = 32;
const ENCRYPTED_KEY_BYTES: usize = KEY_BYTES + 16;
pub(crate) const MAX_MESSAGE: usize = 65_535;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InvalidNoise;

type Result<T> = std::result::Result<T, InvalidNoise>;

pub(crate) struct Initiator<'a> {
    state: Symmetric,
    static_key: &'a StaticSecret,
    ephemeral: StaticSecret,
}

impl<'a> Initiator<'a> {
    pub(crate) fn start(
        static_key: &'a StaticSecret,
        ephemeral: StaticSecret,
        prologue: &[u8],
        payload: &[u8],
    ) -> Result<(Self, Vec<u8>)> {
        let mut state = Symmetric::new(prologue);
        let mut message = PublicKey::from(&ephemeral).to_bytes().to_vec();
        state.mix_hash(&message);
        message.extend(state.seal(payload)?);
        check_size(&message)?;
        Ok((
            Self {
                state,
                static_key,
                ephemeral,
            },
            message,
        ))
    }

    pub(crate) fn finish(mut self, message: &[u8], payload: &[u8]) -> Result<(Vec<u8>, Vec<u8>, Session)> {
        check_size(message)?;
        let (remote, rest) = read_key(message)?;
        self.state.mix_hash(remote.as_bytes());
        self.state.mix_key(&dh(&self.ephemeral, &remote)?)?;
        let (encrypted_static, encrypted_payload) = rest.split_at_checked(ENCRYPTED_KEY_BYTES).ok_or(InvalidNoise)?;
        let static_bytes = self.state.open(encrypted_static)?;
        let (remote_static, _) = read_key(&static_bytes)?;
        self.state.mix_key(&dh(&self.ephemeral, &remote_static)?)?;
        let peer_payload = self.state.open(encrypted_payload)?;

        let mut reply = self.state.seal(PublicKey::from(self.static_key).as_bytes())?;
        self.state.mix_key(&dh(self.static_key, &remote)?)?;
        reply.extend(self.state.seal(payload)?);
        check_size(&reply)?;
        let (send, receive) = self.state.split()?;
        Ok((
            peer_payload,
            reply,
            Session {
                send,
                receive,
                #[cfg(test)]
                hash: self.state.hash,
            },
        ))
    }
}

pub(crate) struct Session {
    pub(crate) send: Cipher,
    pub(crate) receive: Cipher,
    #[cfg(test)]
    hash: [u8; KEY_BYTES],
}

fn read_key(message: &[u8]) -> Result<(PublicKey, &[u8])> {
    let (key, rest) = message.split_first_chunk::<KEY_BYTES>().ok_or(InvalidNoise)?;
    Ok((PublicKey::from(*key), rest))
}

fn dh(key: &StaticSecret, peer: &PublicKey) -> Result<[u8; KEY_BYTES]> {
    let shared = key.diffie_hellman(peer);
    if !shared.was_contributory() {
        return Err(InvalidNoise);
    }
    Ok(*shared.as_bytes())
}

fn check_size(message: &[u8]) -> Result<()> {
    if message.len() > MAX_MESSAGE {
        return Err(InvalidNoise);
    }
    Ok(())
}
