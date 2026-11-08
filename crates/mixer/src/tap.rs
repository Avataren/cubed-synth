//! Taps: the last few thousand frames of a bus, for a scope or a spectrum.
//!
//! A [`Tap`] is a ring of the frames a bus rendered most recently (after its
//! fader, what the meter reads), written once per block on the audio thread and
//! read from any other thread. It is a single-writer ring of atomics: the writer
//! copies a block in and then publishes how many frames it has written; a reader
//! copies the latest frames out and checks the writer did not lap it meanwhile.
//! Neither side locks, waits or allocates.
//!
//! A bus has no tap until the host gives it one (`Command::SetTap`), so a bus
//! nobody looks at costs nothing. [`TapSet`] is the host's side of that: it makes
//! the taps (off the audio thread), keeps a handle on each, and reads them.
//! [`crate::Analysis`] is how a host reads them, natively or from a worklet's
//! messages.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::command::BusId;

/// Frames a tap keeps unless the host asks for another size.
pub const TAP_FRAMES: usize = 4096;

/// A ring of a bus's latest frames. See the module docs.
pub struct Tap {
    left: Box<[AtomicU32]>,
    right: Box<[AtomicU32]>,
    /// Frames written since the tap was made (never wraps in practice).
    written: AtomicU64,
}

impl Tap {
    /// A tap that keeps the last `frames` frames (at least 2). Make it off the
    /// audio thread.
    pub fn new(frames: usize) -> Arc<Tap> {
        let frames = frames.max(2);
        let ring = || (0..frames).map(|_| AtomicU32::new(0)).collect::<Box<[_]>>();
        Arc::new(Tap {
            left: ring(),
            right: ring(),
            written: AtomicU64::new(0),
        })
    }

    /// How many frames the ring keeps.
    pub fn capacity(&self) -> usize {
        self.left.len()
    }

    /// Frames written since the tap was made: it goes up by a block each time
    /// the bus renders one, so a reader can tell new frames from the ones it
    /// has seen.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// Appends a block (the audio thread's). `left` and `right` have the same
    /// length; a block longer than the ring leaves its last `capacity` frames.
    pub(crate) fn write(&self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        if n == 0 {
            return;
        }
        let cap = self.capacity();
        // Only the writer changes `written`, so a relaxed read of it is current.
        let start = self.written.load(Ordering::Relaxed);
        let skip = n.saturating_sub(cap);
        for i in skip..n {
            let at = (start + i as u64) as usize % cap;
            self.left[at].store(left[i].to_bits(), Ordering::Relaxed);
            self.right[at].store(right[i].to_bits(), Ordering::Relaxed);
        }
        self.written.store(start + n as u64, Ordering::Release);
    }

    /// Copies the latest frames, oldest first, into `left` and `right` (as many
    /// as fit in the shorter of the two, at most [`capacity`](Self::capacity))
    /// and returns how many it copied: fewer when the tap has not seen that many
    /// frames yet. The frames are consecutive; if the writer laps the read the
    /// oldest ones are dropped rather than returned torn.
    pub fn read(&self, left: &mut [f32], right: &mut [f32]) -> usize {
        let cap = self.capacity();
        let want = left.len().min(right.len()).min(cap);
        if want == 0 {
            return 0;
        }
        for attempt in 0..4 {
            let end = self.written.load(Ordering::Acquire);
            let have = (end.min(cap as u64) as usize).min(want);
            let first = end - have as u64;
            for i in 0..have {
                let at = (first + i as u64) as usize % cap;
                left[i] = f32::from_bits(self.left[at].load(Ordering::Relaxed));
                right[i] = f32::from_bits(self.right[at].load(Ordering::Relaxed));
            }
            std::sync::atomic::fence(Ordering::Acquire);
            let now = self.written.load(Ordering::Acquire);
            // The writer has overwritten frames `< now - cap`: any of ours that
            // old were read torn.
            let overwritten = (now.saturating_sub(cap as u64)).saturating_sub(first) as usize;
            if overwritten == 0 {
                return have;
            }
            if attempt == 3 || overwritten >= have {
                if overwritten >= have {
                    return 0;
                }
                left.copy_within(overwritten..have, 0);
                right.copy_within(overwritten..have, 0);
                return have - overwritten;
            }
        }
        0
    }
}

impl std::fmt::Debug for Tap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Tap({} frames, {} written)",
            self.capacity(),
            self.written()
        )
    }
}

/// The host's taps: one per bus slot, made and read off the audio thread.
#[derive(Debug, Default)]
pub struct TapSet {
    taps: Vec<Option<Arc<Tap>>>,
}

impl TapSet {
    pub fn new(max_buses: usize) -> TapSet {
        TapSet {
            taps: vec![None; max_buses],
        }
    }

    /// A new tap for `bus` of `frames` frames, replacing the host's handle on
    /// any it had (send the returned tap to the mixer with `Command::SetTap`).
    /// `None` for a bus slot beyond the mixer's.
    pub fn enable(&mut self, bus: BusId, frames: usize) -> Option<Arc<Tap>> {
        let slot = self.taps.get_mut(bus.0 as usize)?;
        let tap = Tap::new(frames);
        *slot = Some(tap.clone());
        Some(tap)
    }

    /// Lets go of the host's handle on `bus`'s tap; true if there was one.
    pub fn disable(&mut self, bus: BusId) -> bool {
        self.taps
            .get_mut(bus.0 as usize)
            .and_then(Option::take)
            .is_some()
    }

    pub fn get(&self, bus: BusId) -> Option<&Arc<Tap>> {
        self.taps.get(bus.0 as usize)?.as_ref()
    }

    /// The bus's latest frames (see [`Tap::read`]); 0 if it has no tap.
    pub fn scope(&self, bus: BusId, left: &mut [f32], right: &mut [f32]) -> usize {
        self.get(bus).map_or(0, |tap| tap.read(left, right))
    }

    /// Frames the bus's tap has seen (see [`Tap::written`]); 0 if it has none.
    pub fn position(&self, bus: BusId) -> u64 {
        self.get(bus).map_or(0, |tap| tap.written())
    }

    /// The buses that have a tap.
    pub fn enabled(&self) -> impl Iterator<Item = BusId> + '_ {
        self.taps
            .iter()
            .enumerate()
            .filter(|(_, tap)| tap.is_some())
            .map(|(i, _)| BusId(i as u16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(from: usize, n: usize) -> (Vec<f32>, Vec<f32>) {
        (
            (from..from + n).map(|i| i as f32).collect(),
            (from..from + n).map(|i| -(i as f32)).collect(),
        )
    }

    #[test]
    fn a_new_tap_has_nothing() {
        let tap = Tap::new(8);
        let (mut l, mut r) = ([9.0; 4], [9.0; 4]);
        assert_eq!(tap.read(&mut l, &mut r), 0);
        assert_eq!(tap.written(), 0);
        assert_eq!(l, [9.0; 4], "untouched");
    }

    #[test]
    fn frames_come_back_oldest_first_before_and_after_the_ring_wraps() {
        let tap = Tap::new(8);
        let (l, r) = block(0, 5);
        tap.write(&l, &r);
        let (mut ol, mut or) = ([0.0; 8], [0.0; 8]);
        assert_eq!(tap.read(&mut ol, &mut or), 5, "only what was written");
        assert_eq!(ol[..5], [0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(or[..5], [0.0, -1.0, -2.0, -3.0, -4.0]);

        // Past the end of the ring: the last 8 of 13 frames.
        let (l, r) = block(5, 8);
        tap.write(&l, &r);
        assert_eq!(tap.written(), 13);
        assert_eq!(tap.read(&mut ol, &mut or), 8);
        assert_eq!(ol, [5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0]);
        assert_eq!(or, [-5.0, -6.0, -7.0, -8.0, -9.0, -10.0, -11.0, -12.0]);

        // A shorter read is the newest frames.
        let (mut sl, mut sr) = ([0.0; 3], [0.0; 5]);
        assert_eq!(tap.read(&mut sl, &mut sr), 3, "the shorter slice rules");
        assert_eq!(sl, [10.0, 11.0, 12.0]);
        assert_eq!(sr[..3], [-10.0, -11.0, -12.0]);
    }

    #[test]
    fn many_odd_blocks_keep_the_order() {
        let tap = Tap::new(100);
        let mut next = 0;
        for n in [7, 128, 1, 33, 99, 100, 101, 3, 250] {
            let (l, r) = block(next, n);
            tap.write(&l, &r);
            next += n;
            let (mut ol, mut or) = (vec![0.0; 40], vec![0.0; 40]);
            let got = tap.read(&mut ol, &mut or);
            assert_eq!(got, 40.min(next));
            for (i, v) in ol[..got].iter().enumerate() {
                assert_eq!(*v, (next - got + i) as f32);
                assert_eq!(or[i], -*v);
            }
        }
    }

    #[test]
    fn a_block_longer_than_the_ring_keeps_its_end() {
        let tap = Tap::new(4);
        let (l, r) = block(0, 10);
        tap.write(&l, &r);
        let (mut ol, mut or) = ([0.0; 4], [0.0; 4]);
        assert_eq!(tap.read(&mut ol, &mut or), 4);
        assert_eq!(ol, [6.0, 7.0, 8.0, 9.0]);
        assert_eq!(tap.written(), 10);
    }

    /// A writer thread hammering the ring while this one reads: every read is a
    /// run of consecutive frames (never torn), and right is left negated.
    #[test]
    fn a_concurrent_reader_never_sees_a_torn_run() {
        let tap = Tap::new(512);
        let writer = {
            let tap = tap.clone();
            std::thread::spawn(move || {
                let mut next = 0usize;
                while next < 4_000_000 {
                    let (l, r) = block(next % (1 << 24), 128);
                    tap.write(&l, &r);
                    next += 128;
                }
            })
        };
        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        let mut reads = 0;
        while !writer.is_finished() {
            let n = tap.read(&mut l, &mut r);
            for i in 1..n {
                assert_eq!(l[i], l[i - 1] + 1.0, "consecutive frames at {i} of {n}");
                assert_eq!(r[i], -l[i]);
            }
            reads += 1;
        }
        writer.join().unwrap();
        assert!(reads > 10);
    }

    #[test]
    fn the_host_set_makes_and_drops_taps() {
        let mut set = TapSet::new(4);
        assert!(
            set.enable(BusId(9), 64).is_none(),
            "beyond the mixer's buses"
        );
        let tap = set.enable(BusId(2), 64).unwrap();
        assert_eq!(set.enabled().collect::<Vec<_>>(), [BusId(2)]);
        let (l, r) = block(0, 16);
        tap.write(&l, &r);
        let (mut ol, mut or) = ([0.0; 16], [0.0; 16]);
        assert_eq!(set.scope(BusId(2), &mut ol, &mut or), 16);
        assert_eq!(set.position(BusId(2)), 16);
        assert_eq!(set.scope(BusId(1), &mut ol, &mut or), 0);
        assert!(set.disable(BusId(2)));
        assert!(!set.disable(BusId(2)));
        assert_eq!(set.scope(BusId(2), &mut ol, &mut or), 0);
    }
}
