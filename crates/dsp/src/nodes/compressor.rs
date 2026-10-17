use rustc_hash::FxHashMap;
use std::any::Any;

use crate::graph::ModulationSource;
use crate::traits::{AudioNode, PortId};

/// Simple stereo compressor with peak detection and wet/dry mix.
pub struct Compressor {
    active: bool,
    threshold_db: f32,
    ratio: f32,
    attack_coeff: f32,
    release_coeff: f32,
    makeup_gain: f32,
    mix: f32,
    envelope: f32,
    sample_rate: f32,
}

impl Compressor {
    pub fn new(
        sample_rate: f32,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_gain_db: f32,
        mix: f32,
    ) -> Self {
        Self {
            active: true,
            threshold_db,
            ratio: ratio.max(1.0),
            attack_coeff: Self::time_to_coeff(attack_ms, sample_rate),
            release_coeff: Self::time_to_coeff(release_ms, sample_rate),
            makeup_gain: Self::db_to_linear(makeup_gain_db),
            mix: mix.clamp(0.0, 1.0),
            envelope: 0.0,
            sample_rate,
        }
    }

    #[inline]
    fn time_to_coeff(time_ms: f32, sample_rate: f32) -> f32 {
        let clamped = time_ms.max(0.01);
        (-1.0 / (clamped * 0.001 * sample_rate)).exp()
    }

    #[inline]
    fn db_to_linear(db: f32) -> f32 {
        10.0_f32.powf(db * 0.05)
    }

    pub fn set_threshold_db(&mut self, threshold_db: f32) {
        self.threshold_db = threshold_db;
    }

    pub fn set_ratio(&mut self, ratio: f32) {
        self.ratio = ratio.max(1.0);
    }

    pub fn set_attack_ms(&mut self, attack_ms: f32) {
        self.attack_coeff = Self::time_to_coeff(attack_ms, self.sample_rate);
    }

    pub fn set_release_ms(&mut self, release_ms: f32) {
        self.release_coeff = Self::time_to_coeff(release_ms, self.sample_rate);
    }

    pub fn set_makeup_gain_db(&mut self, makeup_gain_db: f32) {
        self.makeup_gain = Self::db_to_linear(makeup_gain_db);
    }

    pub fn set_mix(&mut self, mix: f32) {
        self.mix = mix.clamp(0.0, 1.0);
    }

    fn compute_gain(&self, level: f32) -> f32 {
        if level <= 1e-6 {
            return 1.0;
        }

        let level_db = 20.0 * level.log10();
        if level_db <= self.threshold_db {
            return self.makeup_gain;
        }

        let compressed_db = self.threshold_db + (level_db - self.threshold_db) / self.ratio;
        let gain_db = compressed_db - level_db;
        Self::db_to_linear(gain_db) * self.makeup_gain
    }

    #[inline]
    fn update_envelope(&self, current: f32, input: f32) -> f32 {
        if input > current {
            input + self.attack_coeff * (current - input)
        } else {
            input + self.release_coeff * (current - input)
        }
    }
}

impl AudioNode for Compressor {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        let mut ports = FxHashMap::default();
        ports.insert(PortId::AudioInput0, false);
        ports.insert(PortId::AudioInput1, false);
        ports.insert(PortId::AudioOutput0, true);
        ports.insert(PortId::AudioOutput1, true);
        ports
    }

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        if !self.should_process() {
            if let Some(out_left) = outputs.get_mut(&PortId::AudioOutput0) {
                out_left[..buffer_size].fill(0.0);
            }
            if let Some(out_right) = outputs.get_mut(&PortId::AudioOutput1) {
                out_right[..buffer_size].fill(0.0);
            }
            return;
        }

        let left_in = inputs
            .get(&PortId::AudioInput0)
            .and_then(|sources| sources.first())
            .map(|src| &src.buffer[..buffer_size])
            .unwrap_or_else(|| {
                static ZERO_BUFFER: [f32; 1024] = [0.0; 1024];
                &ZERO_BUFFER[..buffer_size.min(ZERO_BUFFER.len())]
            });

        let right_in = inputs
            .get(&PortId::AudioInput1)
            .and_then(|sources| sources.first())
            .map(|src| &src.buffer[..buffer_size])
            .unwrap_or_else(|| {
                static ZERO_BUFFER: [f32; 1024] = [0.0; 1024];
                &ZERO_BUFFER[..buffer_size.min(ZERO_BUFFER.len())]
            });

        let outs = outputs.get_disjoint_mut([&PortId::AudioOutput0, &PortId::AudioOutput1]);
        let [Some(out_left), Some(out_right)] = outs else {
            return;
        };
        let out_left: &mut [f32] = out_left;
        let out_right: &mut [f32] = out_right;

        let mix = self.mix;
        let dry_mix = 1.0 - mix;

        for i in 0..buffer_size {
            let dry_l = left_in.get(i).copied().unwrap_or(0.0);
            let dry_r = right_in.get(i).copied().unwrap_or(0.0);
            let detector = dry_l.abs().max(dry_r.abs());
            self.envelope = self.update_envelope(self.envelope, detector);
            let gain = self.compute_gain(self.envelope);

            let wet_l = dry_l * gain;
            let wet_r = dry_r * gain;

            out_left[i] = dry_l * dry_mix + wet_l * mix;
            out_right[i] = dry_r * dry_mix + wet_r * mix;
        }
    }

    fn reset(&mut self) {
        self.envelope = 0.0;
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn set_active(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.reset();
        }
    }

    fn name(&self) -> &'static str {
        "Compressor"
    }

    fn node_type(&self) -> &str {
        "compressor"
    }
}
