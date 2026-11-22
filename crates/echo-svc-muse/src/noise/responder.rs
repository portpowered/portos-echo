use super::{ENCRYPTED_KEY_BYTES, InvalidNoise, Result, Session, Symmetric, check_size, dh, read_key};
use x25519_dalek::{PublicKey, StaticSecret};

pub(crate) struct Responder {
    state: Symmetric,
    ephemeral: StaticSecret,
}

impl Responder {
    pub(crate) fn reply(
        static_key: &StaticSecret,
        ephemeral: StaticSecret,
        prologue: &[u8],
        first: &[u8],
        payload: &[u8],
    ) -> Result<(Self, Vec<u8>, Vec<u8>)> {
        check_size(first)?;
        let mut state = Symmetric::new(prologue);
        let (remote, first_payload) = read_key(first)?;
        state.mix_hash(remote.as_bytes());
        let first_payload = state.open(first_payload)?;

        let mut second = PublicKey::from(&ephemeral).to_bytes().to_vec();
        state.mix_hash(&second);
        state.mix_key(&dh(&ephemeral, &remote)?)?;
        second.extend(state.seal(PublicKey::from(static_key).as_bytes())?);
        state.mix_key(&dh(static_key, &remote)?)?;
        second.extend(state.seal(payload)?);
        check_size(&second)?;
        Ok((Self { state, ephemeral }, first_payload, second))
    }

    pub(crate) fn finish(mut self, third: &[u8]) -> Result<(Vec<u8>, Session)> {
        check_size(third)?;
        let (encrypted_static, payload) = third.split_at_checked(ENCRYPTED_KEY_BYTES).ok_or(InvalidNoise)?;
        let static_bytes = self.state.open(encrypted_static)?;
        let (remote_static, _) = read_key(&static_bytes)?;
        self.state.mix_key(&dh(&self.ephemeral, &remote_static)?)?;
        let payload = self.state.open(payload)?;
        let (receive, send) = self.state.split()?;
        Ok((
            payload,
            Session {
                send,
                receive,
                hash: self.state.hash,
            },
        ))
    }
}
