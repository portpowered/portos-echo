use rand_core::RngCore;

#[derive(Debug, PartialEq, Eq)]
pub struct EntropyUnavailable;

pub trait Rng {
    /// # Errors
    /// The configured source could not fill the buffer.
    fn fill(&mut self, dest: &mut [u8]) -> Result<(), EntropyUnavailable>;
}

pub struct OsRng;

impl Rng for OsRng {
    fn fill(&mut self, dest: &mut [u8]) -> Result<(), EntropyUnavailable> {
        fill_from(&mut rand_core::OsRng, dest)
    }
}

fn fill_from(source: &mut impl RngCore, dest: &mut [u8]) -> Result<(), EntropyUnavailable> {
    source.try_fill_bytes(dest).map_err(|_| EntropyUnavailable)
}

/// Reproducible SplitMix64 samples for simulations.
pub struct SeededRng(u64);

impl SeededRng {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

impl Rng for SeededRng {
    fn fill(&mut self, dest: &mut [u8]) -> Result<(), EntropyUnavailable> {
        for chunk in dest.chunks_mut(8) {
            chunk.copy_from_slice(&self.next().to_le_bytes()[..chunk.len()]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_reproduce_samples() {
        let mut bytes = [0; 13];
        let mut same = [0; 13];
        SeededRng::new(101).fill(&mut bytes).unwrap();
        SeededRng::new(101).fill(&mut same).unwrap();
        assert_eq!(bytes, same);
        SeededRng::new(102).fill(&mut same).unwrap();
        assert_ne!(bytes, same);
    }

    #[test]
    fn os_entropy_adapter_propagates_source_failure() {
        struct Unavailable;
        impl RngCore for Unavailable {
            fn next_u32(&mut self) -> u32 {
                unreachable!()
            }
            fn next_u64(&mut self) -> u64 {
                unreachable!()
            }
            fn fill_bytes(&mut self, _: &mut [u8]) {
                unreachable!()
            }
            fn try_fill_bytes(&mut self, _: &mut [u8]) -> Result<(), rand_core::Error> {
                Err(rand_core::Error::from(std::num::NonZeroU32::MIN))
            }
        }
        assert_eq!(fill_from(&mut Unavailable, &mut [0]), Err(EntropyUnavailable));
    }
}
