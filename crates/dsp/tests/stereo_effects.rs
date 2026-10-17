//! Golden outputs of the effect nodes, and the check that their in-place stereo
//! path (`StereoEffect::process_stereo`, used by `EffectStack` and mixer bus
//! inserts) renders exactly what the port-map `AudioNode::process` path does.
//!
//! The hashes were recorded from the port-map path before the stereo path
//! existed, so a change to either path that alters a single bit fails here.

use cubed_dsp::effect_stack::EffectStack;
use cubed_dsp::graph::{ModulationSource, ModulationTransformation, ModulationType};
use cubed_dsp::nodes::{
    Bitcrusher, Chorus, Compressor, Convolver, Delay, Freeverb, Limiter, Saturation,
};
use cubed_dsp::{AudioNode, Outputs, PortId, PortMap};

const RATE: f32 = 48_000.0;
const FRAMES: usize = 32_768;
/// Uneven block sizes, so the 4-wide SIMD loops also run their remainders.
const BLOCKS: [usize; 5] = [128, 97, 128, 31, 3];

/// A deterministic stereo test signal: two sines, noise, and a loud stretch in
/// the middle that drives the compressor and the limiter into gain reduction.
fn signal() -> (Vec<f32>, Vec<f32>) {
    let mut seed = 0x1234_5678_u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    };
    let mut left = Vec::with_capacity(FRAMES);
    let mut right = Vec::with_capacity(FRAMES);
    for i in 0..FRAMES {
        let t = i as f32 / RATE;
        let gain = if (FRAMES / 3..FRAMES / 2).contains(&i) {
            2.5
        } else {
            1.0
        };
        let tau = std::f32::consts::TAU;
        left.push(gain * (0.6 * (tau * 220.0 * t).sin() + 0.1 * noise()));
        right.push(gain * (0.5 * (tau * 330.0 * t).sin() + 0.1 * noise()));
    }
    (left, right)
}

/// An exponentially decaying noise burst, so the convolver's IR is the same
/// every run (the engine's `ImpulseResponseGenerator` seeds from the OS).
fn impulse_response() -> Vec<f32> {
    let mut seed = 0x9e37_79b9_u32;
    (0..4_800)
        .map(|i| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            n * (-(i as f32) / 900.0).exp()
        })
        .collect()
}

/// The effects as `AudioEngine::init` builds its effect stack, all enabled.
fn effects() -> Vec<(&'static str, Box<dyn AudioNode>)> {
    let mut plate = Convolver::new(impulse_response(), 128, RATE);
    plate.set_wet_level(0.3);
    vec![
        (
            "chorus",
            Box::new(Chorus::new(RATE, 65.0, 15.0, 5.0, 0.5, 0.3, 0.5, 90.0)),
        ),
        ("delay", Box::new(Delay::new(RATE, 2000.0, 500.0, 0.5, 0.1))),
        (
            "freeverb",
            Box::new(Freeverb::new(RATE, 0.95, 0.5, 0.3, 0.7, 1.0)),
        ),
        ("convolver", Box::new(plate)),
        (
            "limiter",
            Box::new(Limiter::new(RATE, -0.5, 0.1, 50.0, 1.5, true)),
        ),
        (
            "compressor",
            Box::new(Compressor::new(RATE, -12.0, 4.0, 10.0, 80.0, 3.0, 0.5)),
        ),
        ("saturation", Box::new(Saturation::new(2.0, 0.5))),
        ("bitcrusher", Box::new(Bitcrusher::new(12, 4, 0.5))),
    ]
}

/// Calls `f(start, len)` for each block of the test signal.
fn for_each_block(mut f: impl FnMut(usize, usize)) {
    let mut start = 0;
    let mut block = 0;
    while start < FRAMES {
        let len = BLOCKS[block % BLOCKS.len()].min(FRAMES - start);
        f(start, len);
        start += len;
        block += 1;
    }
}

/// Renders the signal through `node.process`, the graph's port-map interface.
fn render_ports(node: &mut dyn AudioNode, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let mut out_left = vec![0.0; FRAMES];
    let mut out_right = vec![0.0; FRAMES];
    for_each_block(|start, len| {
        let source = |buffer| ModulationSource {
            buffer,
            amount: 1.0,
            mod_type: ModulationType::Additive,
            transformation: ModulationTransformation::None,
        };
        let mut inputs = PortMap::default();
        inputs.insert(PortId::AudioInput0, vec![source(&left[start..start + len])]);
        inputs.insert(
            PortId::AudioInput1,
            vec![source(&right[start..start + len])],
        );
        let mut outputs = Outputs::default();
        outputs.insert(PortId::AudioOutput0, &mut out_left[start..start + len]);
        outputs.insert(PortId::AudioOutput1, &mut out_right[start..start + len]);
        node.process(&inputs.as_inputs(), &mut outputs, len);
    });
    (out_left, out_right)
}

/// Renders the signal through `StereoEffect::process_stereo`, in place.
fn render_stereo(node: &mut dyn AudioNode, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let name = node.name();
    let effect = node
        .as_stereo_effect()
        .unwrap_or_else(|| panic!("{name} has no stereo path"));
    let mut out_left = left.to_vec();
    let mut out_right = right.to_vec();
    for_each_block(|start, len| {
        let range = start..start + len;
        effect.process_stereo(&mut out_left[range.clone()], &mut out_right[range]);
    });
    (out_left, out_right)
}

/// Renders the signal through an `EffectStack` holding `effects` in order.
fn render_stack(
    effects: Vec<(&'static str, Box<dyn AudioNode>)>,
    left: &[f32],
    right: &[f32],
) -> (Vec<f32>, Vec<f32>) {
    let mut stack = EffectStack::new(128);
    for (_, effect) in effects {
        stack.add_effect(effect);
    }
    let mut out_left = vec![0.0; FRAMES];
    let mut out_right = vec![0.0; FRAMES];
    for_each_block(|start, len| {
        let range = start..start + len;
        stack.process_audio(
            &left[range.clone()],
            &right[range.clone()],
            &mut out_left[range.clone()],
            &mut out_right[range],
        );
    });
    (out_left, out_right)
}

/// FNV-1a over the samples' bit patterns: equal hashes mean bit-identical output.
fn hash(left: &[f32], right: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for sample in left.iter().chain(right) {
        for byte in sample.to_bits().to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

const GOLDEN: [(&str, u64); 8] = [
    ("chorus", 0x2387_16c4_6cac_3fe8),
    ("delay", 0xdd20_328f_e900_914d),
    ("freeverb", 0x88c8_44c3_c24b_878d),
    ("convolver", 0xe946_173c_f2c7_475e),
    ("limiter", 0x2cd8_98c8_c4f4_4e1e),
    ("compressor", 0x4482_916e_7895_9df8),
    ("saturation", 0xb61b_86c3_69e3_4ba0),
    ("bitcrusher", 0x9fd8_dbc1_6387_2cc1),
];
const GOLDEN_STACK: u64 = 0xf59c_4586_5e99_7af5;

#[test]
fn port_path_matches_golden_outputs() {
    let (left, right) = signal();
    let mut failures = Vec::new();
    for ((name, mut effect), (golden_name, golden)) in effects().into_iter().zip(GOLDEN) {
        assert_eq!(name, golden_name);
        let (out_left, out_right) = render_ports(effect.as_mut(), &left, &right);
        let got = hash(&out_left, &out_right);
        println!("(\"{name}\", {got:#018x}),");
        if got != golden {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "outputs changed: {failures:?}");
}

#[test]
fn stereo_path_matches_golden_outputs() {
    let (left, right) = signal();
    let mut failures = Vec::new();
    for ((name, mut effect), (_, golden)) in effects().into_iter().zip(GOLDEN) {
        let (out_left, out_right) = render_stereo(effect.as_mut(), &left, &right);
        if hash(&out_left, &out_right) != golden {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "outputs changed: {failures:?}");
}

#[test]
fn effect_stack_matches_golden_output() {
    let (left, right) = signal();
    let (out_left, out_right) = render_stack(effects(), &left, &right);
    let got = hash(&out_left, &out_right);
    println!("GOLDEN_STACK = {got:#018x}");
    assert_eq!(got, GOLDEN_STACK);
}
