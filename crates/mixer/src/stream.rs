//! A source fed from another thread: long music decoded a little ahead of
//! playback instead of all at once.
//!
//! [`stream`] makes a [`Stream`] (the source the mixer plays) and its
//! [`StreamWriter`] (for the decoding thread). The writer pushes stereo
//! frames at the file's rate into a lock-free ring; the stream resamples them
//! to the mixer's rate. When the writer finishes and the ring runs dry the
//! stream ends; when the stream is dropped (its voice ended) the writer
//! sees [`StreamWriter::is_abandoned`] and its thread can stop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::source::Source;

struct Shared {
    finished: AtomicBool,
    underruns: AtomicU64,
}

/// The decoding thread's end of a stream.
pub struct StreamWriter {
    ring: rtrb::Producer<f32>,
    shared: Arc<Shared>,
}

/// The mixer's end: plays what the writer pushed.
pub struct Stream {
    ring: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    /// Source frames per output frame.
    step: f64,
    /// Where between `window[1]` and `window[2]` the next output falls.
    t: f64,
    /// Four source frames around the read position, per channel.
    window: [[f32; 4]; 2],
    /// Source frames still to come after the writer finished: the window's
    /// tail drains through the interpolation.
    drained: u8,
}

/// A stream of stereo frames at `source_rate`, played at `mixer_rate`, with
/// room for `capacity_frames` frames between the two threads.
pub fn stream(capacity_frames: usize, source_rate: f32, mixer_rate: f32) -> (StreamWriter, Stream) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity_frames.max(1) * 2);
    let shared = Arc::new(Shared {
        finished: AtomicBool::new(false),
        underruns: AtomicU64::new(0),
    });
    (
        StreamWriter {
            ring: producer,
            shared: shared.clone(),
        },
        Stream {
            ring: consumer,
            shared,
            step: source_rate.max(1.0) as f64 / mixer_rate.max(1.0) as f64,
            t: 0.0,
            window: [[0.0; 4]; 2],
            drained: 0,
        },
    )
}

impl StreamWriter {
    /// Frames there is room for now.
    pub fn room(&self) -> usize {
        self.ring.slots() / 2
    }

    /// Pushes as many of the interleaved stereo frames as fit; returns how
    /// many frames went in.
    pub fn push(&mut self, interleaved: &[f32]) -> usize {
        let frames = (interleaved.len() / 2).min(self.room());
        let Ok(mut chunk) = self.ring.write_chunk_uninit(frames * 2) else {
            return 0;
        };
        let (a, b) = chunk.as_mut_slices();
        let (first, second) = interleaved[..frames * 2].split_at(a.len());
        for (slot, &value) in a.iter_mut().zip(first) {
            slot.write(value);
        }
        for (slot, &value) in b.iter_mut().zip(second) {
            slot.write(value);
        }
        // SAFETY: every slot of both halves was written above.
        unsafe { chunk.commit_all() };
        frames
    }

    /// No more frames: the stream ends once it has played what it has.
    pub fn finish(&self) {
        self.shared.finished.store(true, Ordering::Release);
    }

    /// The stream is gone (its voice ended): stop decoding.
    pub fn is_abandoned(&self) -> bool {
        self.ring.is_abandoned()
    }

    /// Blocks the stream played as silence because the writer fell behind.
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }
}

/// Catmull-Rom interpolation between `x[1]` and `x[2]` at `t` in [0, 1).
#[inline]
fn hermite(x: &[f32; 4], t: f32) -> f32 {
    let c1 = 0.5 * (x[2] - x[0]);
    let c2 = x[0] - 2.5 * x[1] + 2.0 * x[2] - 0.5 * x[3];
    let c3 = 0.5 * (x[3] - x[0]) + 1.5 * (x[1] - x[2]);
    ((c3 * t + c2) * t + c1) * t + x[1]
}

impl Stream {
    /// Moves the window on by one source frame. False when there is none:
    /// the writer is behind (silence) or done.
    fn advance(&mut self) -> bool {
        let frame = match self.ring.read_chunk(2) {
            Ok(chunk) => {
                let (a, b) = chunk.as_slices();
                let l = a.first().or(b.first()).copied().unwrap_or(0.0);
                let r = a
                    .get(1)
                    .or_else(|| b.get(1 - a.len()))
                    .copied()
                    .unwrap_or(0.0);
                chunk.commit_all();
                Some([l, r])
            }
            Err(_) if self.shared.finished.load(Ordering::Acquire) && self.drained < 3 => {
                // Drain the last frames through the window with silence.
                self.drained += 1;
                Some([0.0, 0.0])
            }
            Err(_) => None,
        };
        let Some(frame) = frame else {
            return false;
        };
        for (c, &value) in frame.iter().enumerate() {
            let w = &mut self.window[c];
            w.copy_within(1.., 0);
            w[3] = value;
        }
        true
    }
}

impl Source for Stream {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let n = left.len().min(right.len());
        for i in 0..n {
            while self.t >= 1.0 {
                if !self.advance() {
                    left[i..n].fill(0.0);
                    right[i..n].fill(0.0);
                    if self.shared.finished.load(Ordering::Acquire) {
                        return false;
                    }
                    self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
                self.t -= 1.0;
            }
            let t = self.t as f32;
            left[i] = hermite(&self.window[0], t);
            right[i] = hermite(&self.window[1], t);
            self.t += self.step;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_what_was_pushed_then_ends() {
        let (mut writer, mut stream) = stream(1024, 48_000.0, 48_000.0);
        let frames: Vec<f32> = (0..300).flat_map(|i| [i as f32, -(i as f32)]).collect();
        assert_eq!(writer.push(&frames), 300);
        writer.finish();
        let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
        let alive = stream.render(&mut l, &mut r);
        assert!(!alive);
        // At the same rate the output is the input, a frame and a half late
        // for the interpolation window.
        let first = l.iter().position(|&s| s == 1.0).expect("frame 1 plays");
        for k in 0..290 {
            assert!((l[first + k] - (k + 1) as f32).abs() < 1e-4, "{k}");
            assert!((r[first + k] + (k + 1) as f32).abs() < 1e-4);
        }
    }

    #[test]
    fn resamples_and_reports_underruns() {
        let (mut writer, mut stream) = stream(48_000, 24_000.0, 48_000.0);
        let frames = vec![0.5f32; 2 * 1000];
        writer.push(&frames);
        let (mut l, mut r) = (vec![0.0; 128], vec![0.0; 128]);
        let mut heard = 0;
        for _ in 0..20 {
            assert!(stream.render(&mut l, &mut r), "not finished: keeps going");
            heard += l.iter().filter(|&&s| (s - 0.5).abs() < 1e-3).count();
        }
        // 1000 frames at half the rate last ~2000 output frames.
        assert!((1990..=2010).contains(&heard), "{heard}");
        assert!(writer.underruns() > 0);
        drop(stream);
        assert!(writer.is_abandoned());
    }
}
