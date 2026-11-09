// morph_wavetable.rs

use rustc_hash::{FxHashMap, FxHasher};
use std::collections::VecDeque;
use std::hash::Hasher;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use web_sys::console;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
fn log_console(message: &str) {
    console::log_1(&message.into());
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
fn log_console(_message: &str) {}

use super::{generate_mipmapped_bank_dynamic, Waveform, Wavetable, WavetableBank};
// Import the FFT-based mipmapping types (adjust the module path as needed)

/// Cubic interpolation helper. Assumes the samples slice is cyclic.
pub fn cubic_interp(samples: &[f32], pos: f32) -> f32 {
    let n = samples.len();
    let i = pos.floor() as isize;
    let frac = pos - i as f32;
    let idx = |j: isize| -> f32 {
        let index = ((i + j).rem_euclid(n as isize)) as usize;
        samples[index]
    };
    let p0 = idx(-1);
    let p1 = idx(0);
    let p2 = idx(1);
    let p3 = idx(2);
    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c = -0.5 * p0 + 0.5 * p2;
    let d = p1;
    a * frac * frac * frac + b * frac * frac + c * frac + d
}

/// A mipmapped wavetable that contains a bank of band-limited tables. The bank
/// is shared: a frame that has been built once is built again by nothing
/// (see [`mipmapped_frame`]).
pub struct MipmappedWavetable {
    pub bank: Arc<WavetableBank>,
}

/// A collection of mipmapped wavetables used for morphing between different waveforms.
pub struct WavetableMorphCollection {
    pub wavetables: Vec<MipmappedWavetable>,
}

impl Default for WavetableMorphCollection {
    fn default() -> Self {
        Self::new()
    }
}

impl WavetableMorphCollection {
    pub fn new() -> Self {
        Self {
            wavetables: Vec::new(),
        }
    }

    pub fn num_tables(&self) -> usize {
        self.wavetables.len()
    }

    /// Add a new mipmapped wavetable to the collection.
    pub fn add_wavetable(&mut self, wavetable: MipmappedWavetable) {
        self.wavetables.push(wavetable);
    }

    /// Generate a test morph collection containing basic waveforms.
    pub fn generate_test_collection(sample_rate: f32) -> Self {
        let max_table_size = 2048;
        let mut collection = Self::new();
        collection.add_wavetable(generate_mipmapped_bank(
            Waveform::Sine,
            sample_rate,
            max_table_size,
        ));
        collection.add_wavetable(generate_mipmapped_bank(
            Waveform::Triangle,
            sample_rate,
            max_table_size,
        ));
        collection.add_wavetable(generate_mipmapped_bank(
            Waveform::Saw,
            sample_rate,
            max_table_size,
        ));
        collection.add_wavetable(generate_mipmapped_bank(
            Waveform::Square,
            sample_rate,
            max_table_size,
        ));
        collection
    }

    /// Given a morph value (0.0–1.0), select a pair of adjacent mipmapped wavetables and mix factor.
    pub fn select_pair_by_morph(
        &self,
        morph: f32,
    ) -> (&MipmappedWavetable, &MipmappedWavetable, f32) {
        let num_tables = self.wavetables.len();
        if num_tables == 0 {
            panic!("WavetableMorphCollection is empty!");
        }
        let clamped = morph.clamp(0.0, 1.0);
        let float_index = clamped * (num_tables as f32 - 1.0);
        let lower_index = float_index.floor() as usize;
        let upper_index = if lower_index < num_tables - 1 {
            lower_index + 1
        } else {
            lower_index
        };
        let mix = float_index - lower_index as f32;
        (
            &self.wavetables[lower_index],
            &self.wavetables[upper_index],
            mix,
        )
    }

    /// Lookup a sample given a phase (0.0–1.0), a morph value, and the effective frequency.
    /// This selects the appropriate mipmap level for each waveform based on frequency.
    pub fn lookup_sample(&self, phase: f32, morph: f32, frequency: f32) -> f32 {
        let (wavetable1, wavetable2, mix) = self.select_pair_by_morph(morph);
        let table1 = wavetable1.bank.select_table(frequency);
        let table2 = wavetable2.bank.select_table(frequency);
        let pos1 = phase * table1.table_size as f32;
        let sample1 = cubic_interp(&table1.samples, pos1);
        let pos2 = phase * table2.table_size as f32;
        let sample2 = cubic_interp(&table2.samples, pos2);
        sample1 + mix * (sample2 - sample1)
    }
}

/// A bank of wavetable morph collections, keyed by a name.
pub struct WavetableSynthBank {
    pub collections: FxHashMap<String, Rc<WavetableMorphCollection>>,
}

impl WavetableSynthBank {
    pub fn new(sample_rate: f32) -> Self {
        let collection = WavetableMorphCollection::generate_test_collection(sample_rate);
        let mut collections = FxHashMap::default();
        collections.insert("default".to_string(), Rc::new(collection));
        Self { collections }
    }

    pub fn clear(&mut self) {
        self.collections.clear();
    }

    /// Add a new collection to the bank.
    pub fn add_collection(
        &mut self,
        name: impl Into<String>,
        collection: WavetableMorphCollection,
    ) {
        self.collections.insert(name.into(), Rc::new(collection));
    }

    /// Retrieve a collection by name.
    pub fn get_collection(&self, name: &str) -> Option<Rc<WavetableMorphCollection>> {
        self.collections.get(name).cloned()
    }
}

/// Helper function to generate a mipmapped bank for a given waveform.
pub fn generate_mipmapped_bank(
    waveform: Waveform,
    sample_rate: f32,
    max_table_size: usize,
) -> MipmappedWavetable {
    let bank = WavetableBank::new(waveform, max_table_size, sample_rate)
        .expect("Failed to generate mipmapped bank");
    log_console(&format!(
        "Generated mipmapped bank for {:?} waveform",
        waveform
    ));
    MipmappedWavetable {
        bank: Arc::new(bank),
    }
}

/// How much built frames the cache keeps (their samples and mip levels), in bytes.
#[cfg(target_arch = "wasm32")]
const FRAME_CACHE_BYTES: usize = 24 << 20;
#[cfg(not(target_arch = "wasm32"))]
const FRAME_CACHE_BYTES: usize = 96 << 20;

struct CachedFrame {
    samples: Vec<f32>,
    rate: f32,
    bank: Arc<WavetableBank>,
    bytes: usize,
}

#[derive(Default)]
struct FrameCache {
    frames: FxHashMap<u64, CachedFrame>,
    order: VecDeque<u64>,
    bytes: usize,
}

static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

/// How many frames [`mipmapped_frame`] has found built and how many it has built.
pub fn frame_cache_stats() -> (u64, u64) {
    (
        CACHE_HITS.load(Ordering::Relaxed),
        CACHE_MISSES.load(Ordering::Relaxed),
    )
}

fn frame_cache() -> &'static Mutex<FrameCache> {
    static CACHE: OnceLock<Mutex<FrameCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(FrameCache::default()))
}

fn frame_key(samples: &[f32], rate: f32) -> u64 {
    let mut hasher = FxHasher::default();
    hasher.write_u32(rate.to_bits());
    hasher.write_usize(samples.len());
    for s in samples {
        hasher.write_u32(s.to_bits());
    }
    hasher.finish()
}

/// The mip levels of one cycle of a wavetable (`samples`, a power of two or
/// not, at engine rate `rate`), as [`generate_mipmapped_bank_dynamic`] builds
/// them. Building one costs a forward FFT and an inverse one for every level
/// (a dozen), so a bank is kept (up to a few tens of MB across all frames, the
/// oldest dropped first) and handed out again to any engine that asks for the
/// same cycle: a wavetable of 256 frames in which one frame was edited builds
/// that frame only when the patch is built again.
///
/// A cycle with nothing above the DC offset in it (a silent frame) is a silent
/// table: the DAW's loader refuses such a patch, this plays it as silence.
pub fn mipmapped_frame(samples: &[f32], rate: f32) -> Result<MipmappedWavetable, String> {
    let key = frame_key(samples, rate);
    {
        let cache = frame_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.frames.get(&key) {
            if hit.rate == rate && hit.samples == samples {
                CACHE_HITS.fetch_add(1, Ordering::Relaxed);
                return Ok(MipmappedWavetable {
                    bank: hit.bank.clone(),
                });
            }
        }
    }
    CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    let size = samples.len();
    let bank = match generate_mipmapped_bank_dynamic(samples.to_vec(), size, rate) {
        Ok(bank) => bank,
        Err(e) if e.to_string().contains("No significant harmonics") => WavetableBank {
            tables: vec![Wavetable {
                samples: vec![0.0; size + 1],
                table_size: size,
                top_freq_hz: f32::MAX,
            }],
        },
        Err(e) => return Err(e.to_string()),
    };
    let bytes = size * 4
        + bank
            .tables
            .iter()
            .map(|t| t.samples.len() * 4)
            .sum::<usize>();
    let bank = Arc::new(bank);
    let mut cache = frame_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = cache.frames.remove(&key) {
        cache.bytes -= old.bytes;
        cache.order.retain(|k| *k != key);
    }
    cache.bytes += bytes;
    cache.order.push_back(key);
    cache.frames.insert(
        key,
        CachedFrame {
            samples: samples.to_vec(),
            rate,
            bank: bank.clone(),
            bytes,
        },
    );
    while cache.bytes > FRAME_CACHE_BYTES && cache.order.len() > 1 {
        if let Some(oldest) = cache.order.pop_front() {
            if let Some(gone) = cache.frames.remove(&oldest) {
                cache.bytes -= gone.bytes;
            }
        }
    }
    Ok(MipmappedWavetable { bank })
}

/// A collection of the frames of one wavetable (a cycle each; their count
/// and the oscillator's `wave_index` morph along them), at engine rate `rate`.
pub fn collection_from_frames(
    frames: &[&[f32]],
    rate: f32,
) -> Result<WavetableMorphCollection, String> {
    let mut collection = WavetableMorphCollection::new();
    for frame in frames {
        collection.add_wavetable(mipmapped_frame(frame, rate)?);
    }
    Ok(collection)
}

/// Bytes the frame cache holds now (for tests and diagnostics).
pub fn frame_cache_bytes() -> usize {
    frame_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .bytes
}
