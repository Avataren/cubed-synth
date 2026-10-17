//! A seed source for the crate's random number generators that needs no OS
//! randomness: a splitmix64 stream the host seeds.
//!
//! Native builds seed their RNGs from the OS and only fall back to this stream
//! if that fails. wasm32 builds without the `wasm` feature (a host's own
//! AudioWorklet module, whose global scope has no `crypto`) always use it, so
//! the host should call [`set_seed`] once at startup with entropy from its main
//! thread. Unseeded, the stream starts from a fixed value: deterministic, never
//! an error.

use std::sync::atomic::{AtomicU64, Ordering};

const GOLDEN_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;

static STATE: AtomicU64 = AtomicU64::new(GOLDEN_GAMMA);

/// Restarts the stream from `seed`.
pub fn set_seed(seed: u64) {
    STATE.store(seed, Ordering::Relaxed);
}

/// Fills `bytes` with the stream's next values.
pub fn fill(bytes: &mut [u8]) {
    for chunk in bytes.chunks_mut(8) {
        let state = STATE
            .fetch_add(GOLDEN_GAMMA, Ordering::Relaxed)
            .wrapping_add(GOLDEN_GAMMA);
        let word = mix(state).to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
}

/// splitmix64's output function.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_gives_same_bytes_and_calls_differ() {
        let mut first = [0u8; 20];
        let mut second = [0u8; 20];
        set_seed(42);
        fill(&mut first);
        fill(&mut second);
        assert_ne!(first, second);

        let mut again = [0u8; 20];
        set_seed(42);
        fill(&mut again);
        assert_eq!(first, again);
    }
}
