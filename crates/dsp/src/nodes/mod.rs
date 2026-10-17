pub mod analog_oscillator;
pub mod arpeggiator;
pub mod bitcrusher;
pub mod chorus;
pub mod compressor;
pub mod convolver;
pub mod delay;
pub mod envelope;
pub mod eq;
pub mod filter_collection;
pub mod freeverb;
pub mod gate_mixer;
pub mod glide;
pub mod global_frequency_node;
pub mod global_velocity_node;
pub mod lfo;
pub mod limiter;
pub mod mixer;
pub mod morph_wavetable;
pub mod noise_generator;
pub mod sampler;
pub mod saturation;
pub mod wavetable;
pub mod wavetable_oscillator;

pub use analog_oscillator::*;
pub use arpeggiator::*;
pub use bitcrusher::*;
pub use chorus::*;
pub use compressor::*;
pub use convolver::*;
pub use delay::*;
pub use envelope::*;
pub use filter_collection::*;
pub use freeverb::*;
pub use gate_mixer::*;
pub use glide::*;
pub use global_frequency_node::*;
pub use global_velocity_node::*;
pub use lfo::*;
pub use limiter::*;
pub use mixer::*;
pub use noise_generator::*;
pub use sampler::*;
pub use saturation::*;
pub use wavetable::*;
pub use wavetable_oscillator::*;

/// Copies `input` into `output` and zeroes the rest of `output`: how an effect's
/// port-map `process` hands its input to `StereoEffect::process_stereo`, which
/// then works on the output buffers in place.
pub(crate) fn copy_input(output: &mut [f32], input: &[f32]) {
    let len = input.len().min(output.len());
    output[..len].copy_from_slice(&input[..len]);
    output[len..].fill(0.0);
}
