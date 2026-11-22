use ring::{aead, digest, hkdf};

use super::{InvalidNoise, KEY_BYTES, MAX_MESSAGE, Result, check_size};

const PROTOCOL: &[u8] = b"Noise_XX_25519_AESGCM_SHA256";
// The reference stops at the maximum interoperable integer in JavaScript.
const NONCE_LIMIT: u64 = (1 << 53) - 1;

pub(crate) struct Cipher {
    key: aead::LessSafeKey,
    counter: u64,
}

impl Cipher {
    fn new(key: &[u8]) -> Result<Self> {
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| InvalidNoise)?;
        Ok(Self {
            key: aead::LessSafeKey::new(key),
            counter: 0,
        })
    }

    fn nonce(&mut self) -> Result<aead::Nonce> {
        if self.counter >= NONCE_LIMIT {
            return Err(InvalidNoise);
        }
        let mut iv = [0; 12];
        iv[4..].copy_from_slice(&self.counter.to_be_bytes());
        self.counter += 1;
        Ok(aead::Nonce::assume_unique_for_key(iv))
    }

    pub(crate) fn encrypt(&mut self, data: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        if data.len() > MAX_MESSAGE - self.key.algorithm().tag_len() {
            return Err(InvalidNoise);
        }
        let mut result = data.to_vec();
        let nonce = self.nonce()?;
        self.key
            .seal_in_place_append_tag(nonce, aead::Aad::from(aad), &mut result)
            .map_err(|_| InvalidNoise)?;
        Ok(result)
    }

    pub(crate) fn decrypt(&mut self, mut data: Vec<u8>, aad: &[u8]) -> Result<Vec<u8>> {
        check_size(&data)?;
        let nonce = self.nonce()?;
        let size = self
            .key
            .open_in_place(nonce, aead::Aad::from(aad), &mut data)
            .map_err(|_| InvalidNoise)?
            .len();
        data.truncate(size);
        Ok(data)
    }
}

pub(super) struct Symmetric {
    chaining_key: [u8; KEY_BYTES],
    pub(super) hash: [u8; KEY_BYTES],
    cipher: Option<Cipher>,
}

impl Symmetric {
    pub(super) fn new(prologue: &[u8]) -> Self {
        let mut name = [0; KEY_BYTES];
        name[..PROTOCOL.len()].copy_from_slice(PROTOCOL);
        let mut state = Self {
            chaining_key: name,
            hash: name,
            cipher: None,
        };
        state.mix_hash(prologue);
        state
    }

    pub(super) fn mix_hash(&mut self, data: &[u8]) {
        let mut hash = digest::Context::new(&digest::SHA256);
        hash.update(&self.hash);
        hash.update(data);
        self.hash.copy_from_slice(hash.finish().as_ref());
    }

    pub(super) fn mix_key(&mut self, input: &[u8]) -> Result<()> {
        let keys = hkdf(&self.chaining_key, input)?;
        self.chaining_key.copy_from_slice(&keys[..KEY_BYTES]);
        self.cipher = Some(Cipher::new(&keys[KEY_BYTES..])?);
        Ok(())
    }

    pub(super) fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let encrypted = match &mut self.cipher {
            Some(cipher) => cipher.encrypt(plaintext, &self.hash)?,
            None => plaintext.to_vec(),
        };
        self.mix_hash(&encrypted);
        Ok(encrypted)
    }

    pub(super) fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let plaintext = match &mut self.cipher {
            Some(cipher) => cipher.decrypt(ciphertext.to_vec(), &self.hash)?,
            None => ciphertext.to_vec(),
        };
        self.mix_hash(ciphertext);
        Ok(plaintext)
    }

    pub(super) fn split(&self) -> Result<(Cipher, Cipher)> {
        let keys = hkdf(&self.chaining_key, b"")?;
        Ok((Cipher::new(&keys[..KEY_BYTES])?, Cipher::new(&keys[KEY_BYTES..])?))
    }
}

struct TwoKeys;

impl hkdf::KeyType for TwoKeys {
    fn len(&self) -> usize {
        2 * KEY_BYTES
    }
}

fn hkdf(salt: &[u8], input: &[u8]) -> Result<[u8; 2 * KEY_BYTES]> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(input);
    let mut output = [0; 2 * KEY_BYTES];
    prk.expand(&[], TwoKeys)
        .map_err(|_| InvalidNoise)?
        .fill(&mut output)
        .map_err(|_| InvalidNoise)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t2_noise_nonce_counter_never_wraps() {
        let mut cipher = Cipher::new(&[7; KEY_BYTES]).unwrap();
        cipher.counter = NONCE_LIMIT - 1;
        assert_eq!(cipher.encrypt(&[], &[]).unwrap().len(), 16);
        assert_eq!(cipher.encrypt(&[], &[]), Err(InvalidNoise));
        assert_eq!(cipher.decrypt(vec![0; 16], &[]), Err(InvalidNoise));
        assert_eq!(cipher.counter, NONCE_LIMIT);
    }
}
