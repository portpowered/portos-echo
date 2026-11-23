use crate::{Result, layout};
use serde::{Deserialize, Serialize};

const LIMIT: u8 = 3;
const VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Current,
    Previous,
}

impl Slot {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Current => layout::CURRENT,
            Self::Previous => layout::PREVIOUS,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// # Errors
    /// Rejects values other than lowercase SHA-256 hex.
    pub fn parse(hex: String) -> Result<Self> {
        let digest = Self(hex);
        if digest.valid() {
            Ok(digest)
        } else {
            Err("invalid binary digest")
        }
    }

    fn valid(&self) -> bool {
        self.0.len() == 64
            && self
                .0
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    version: u8,
    current_sha256: Digest,
    selected: Slot,
    failed_tries: u8,
    attempt_in_progress: bool,
}

impl Selection {
    pub(crate) fn new(current: Digest) -> Self {
        Self {
            version: VERSION,
            current_sha256: current,
            selected: Slot::Current,
            failed_tries: 0,
            attempt_in_progress: false,
        }
    }

    /// # Errors
    /// Rejects malformed or unsupported journals before selecting a slot.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = serde_json::from_slice(bytes).map_err(|_| "invalid selection journal")?;
        let valid_attempt =
            !value.attempt_in_progress || (value.selected == Slot::Current && value.failed_tries < LIMIT);
        let valid_previous = value.selected != Slot::Previous || [0, LIMIT].contains(&value.failed_tries);
        if value.version == VERSION
            && value.current_sha256.valid()
            && value.failed_tries <= LIMIT
            && valid_attempt
            && valid_previous
        {
            Ok(value)
        } else {
            Err("invalid selection journal")
        }
    }

    /// # Errors
    /// Returns an error if the journal cannot be encoded.
    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|_| "encode selection journal")
    }

    pub(crate) fn current(&self) -> &Digest {
        &self.current_sha256
    }

    #[must_use]
    pub const fn slot(&self) -> Slot {
        self.selected
    }

    #[must_use]
    pub const fn failed_tries(&self) -> u8 {
        self.failed_tries
    }

    pub(crate) const fn needs_fallback(&self) -> bool {
        matches!(self.selected, Slot::Current) && self.failed_tries == LIMIT
    }

    pub(crate) fn fallback(&mut self) {
        self.selected = Slot::Previous;
    }

    pub(crate) fn attempt(&mut self) {
        self.attempt_in_progress = self.selected == Slot::Current;
    }

    pub(crate) fn fail(&mut self) {
        if self.attempt_in_progress {
            self.failed_tries += 1;
            self.cancel();
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.attempt_in_progress = false;
    }

    pub(crate) fn stabilize(&mut self) {
        self.cancel();
        self.failed_tries = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_validates_version_digest_and_attempt() {
        let digest = Digest::parse("a".repeat(64)).unwrap();
        let mut selection = Selection::new(digest);
        selection.attempt();
        let bytes = selection.encode().unwrap();
        assert_eq!(Selection::decode(&bytes), Ok(selection.clone()));
        for _ in 0..2 {
            selection.fail();
            selection.fail();
            selection.attempt();
        }
        selection.fail();
        assert_eq!(selection.failed_tries(), 3);
        assert!(selection.needs_fallback());
        for patch in [
            ("version", 2.into()),
            ("failed_tries", 4.into()),
            ("attempt_in_progress", true.into()),
            ("current_sha256", "A".repeat(64).into()),
        ] {
            let mut invalid = serde_json::to_value(&selection).unwrap();
            invalid[patch.0] = patch.1;
            assert_eq!(
                Selection::decode(&serde_json::to_vec(&invalid).unwrap()),
                Err("invalid selection journal")
            );
        }
    }
}
