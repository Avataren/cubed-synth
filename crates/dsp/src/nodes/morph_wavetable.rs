// morph_wavetable.rs

use rustc_hash::FxHashMap;
use std::rc::Rc;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use web_sys::console;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
fn log_console(message: &str) {
    console::log_1(&message.into());
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
fn log_console(_message: &str) {}

use super::{Waveform, WavetableBank};
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

/// A mipmapped wavetable that contains a bank of band-limited tables.
pub struct MipmappedWavetable {
    pub bank: WavetableBank,
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
    MipmappedWavetable { bank }
}
