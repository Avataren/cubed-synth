//! Renders an engine sound through a scripted drive to a WAV file, to hear a recipe
//! without a game: the starter, the engine catching, idle, blips of the throttle, a pull
//! through the gears with shifts, a closed-throttle overrun, and a tyre slide.
//!
//!     cargo run -p cubed-mixer --example engine_render --release -- out.wav [cylinders]
//!
//! With `cylinders` 8 it plays a cross-plane V8's firing order.

use cubed_mixer::{
    BusId, Command, EngineControls, EngineSound, Mixer, MixerConfig, PlayEngine, VoiceId,
};

const RATE: u32 = 48_000;

/// The drive: (seconds, controls at the end of the stretch); the controls glide over it.
fn script() -> Vec<(f32, EngineControls)> {
    let at = |rpm: f32, throttle: f32, load: f32, speed: f32, skid: f32| EngineControls {
        rpm,
        throttle,
        load,
        speed,
        skid,
    };
    let mut steps = vec![
        (0.3, at(0.0, 0.0, 0.0, 0.0, 0.0)),
        // The starter cranks, then it fires.
        (0.15, at(230.0, 0.0, 0.0, 0.0, 0.0)),
        (0.6, at(240.0, 0.0, 0.0, 0.0, 0.0)),
        (0.25, at(1300.0, 0.6, 0.4, 0.0, 0.0)),
        (0.6, at(820.0, 0.1, 0.05, 0.0, 0.0)),
        (2.0, at(800.0, 0.1, 0.05, 0.0, 0.0)),
        // Blips.
        (0.25, at(3500.0, 1.0, 0.6, 0.0, 0.0)),
        (0.7, at(900.0, 0.0, -0.2, 0.0, 0.0)),
        (0.3, at(5200.0, 1.0, 0.6, 0.0, 0.0)),
        (1.0, at(850.0, 0.0, -0.2, 0.0, 0.0)),
        (1.0, at(800.0, 0.1, 0.05, 0.0, 0.0)),
    ];
    // Pull away and through the gears: each gear revs up, the shift drops the revs.
    let mut speed = 0.0;
    for (gear, (from, to, seconds)) in [
        (1800.0, 5500.0, 2.6),
        (3300.0, 5500.0, 3.4),
        (3600.0, 5500.0, 4.5),
        (3900.0, 5300.0, 5.5),
    ]
    .into_iter()
    .enumerate()
    {
        let top = [12.0, 21.0, 32.0, 42.0][gear];
        steps.push((0.15, at(from, 1.0, 0.9, speed, 0.0)));
        steps.push((seconds, at(to, 1.0, 0.85, top, 0.0)));
        // The shift: the throttle shut for a moment.
        steps.push((0.25, at(to - 300.0, 0.0, -0.1, top, 0.0)));
        speed = top;
    }
    steps.extend([
        // Lifting off at speed: overrun, the engine braking.
        (4.0, at(2200.0, 0.0, -0.3, 30.0, 0.0)),
        (1.0, at(2400.0, 0.4, 0.3, 28.0, 0.0)),
        // A slide: the tyres squeal.
        (0.3, at(3200.0, 1.0, 0.8, 24.0, 0.9)),
        (1.2, at(3000.0, 0.6, 0.5, 20.0, 1.2)),
        (0.6, at(2500.0, 0.3, 0.2, 18.0, 0.0)),
        (2.0, at(1100.0, 0.0, -0.2, 6.0, 0.0)),
        (2.0, at(800.0, 0.1, 0.05, 0.0, 0.0)),
        // Switched off.
        (0.6, at(0.0, 0.0, 0.0, 0.0, 0.0)),
        (0.5, at(0.0, 0.0, 0.0, 0.0, 0.0)),
    ]);
    steps
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).cloned().unwrap_or_else(|| "engine.wav".into());
    let cylinders: u8 = args.get(2).and_then(|c| c.parse().ok()).unwrap_or(4);
    let mut engine = EngineSound {
        cylinders,
        firing: EngineSound::even(cylinders, false),
        ..EngineSound::default()
    };
    if cylinders == 8 {
        // A cross-plane V8: even intervals, but each bank's exhausts uneven.
        engine.firing = [
            0.0, 90.0, 270.0, 180.0, 630.0, 540.0, 450.0, 360.0, 0.0, 0.0, 0.0, 0.0,
        ];
        engine.header_spread = 0.8;
        engine.muffler = 0.4;
    }
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE as f32,
        ..MixerConfig::default()
    });
    let voice = VoiceId(1);
    mixer.apply(Command::PlayEngine(PlayEngine::new(
        voice,
        engine,
        BusId::MASTER,
    )));
    let block = 128;
    let mut left = vec![0.0f32; block];
    let mut right = vec![0.0f32; block];
    let mut samples: Vec<i16> = Vec::new();
    let mut last = EngineControls::default();
    for (seconds, target) in script() {
        let frames = (seconds * RATE as f32) as usize;
        let blocks = frames.div_ceil(block).max(1);
        for b in 0..blocks {
            // Controls arrive once a frame in a game: every 800 frames here (60 Hz).
            let t = (b + 1) as f32 / blocks as f32;
            let lerp = |a: f32, b: f32| a + (b - a) * t;
            if (b * block) % 800 < block {
                mixer.apply(Command::SetEngine {
                    voice,
                    controls: EngineControls {
                        rpm: lerp(last.rpm, target.rpm),
                        throttle: lerp(last.throttle, target.throttle),
                        load: lerp(last.load, target.load),
                        speed: lerp(last.speed, target.speed),
                        skid: lerp(last.skid, target.skid),
                    },
                });
            }
            left.fill(0.0);
            right.fill(0.0);
            mixer.process(&mut left, &mut right);
            for (l, r) in left.iter().zip(&right) {
                let mono = (l + r) * 0.5;
                samples.push((mono.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
        }
        last = target;
    }
    write_wav(&out, &samples);
    println!("wrote {out}: {:.1} s", samples.len() as f32 / RATE as f32);
}

fn write_wav(path: &str, samples: &[i16]) {
    let data = samples.len() as u32 * 2;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write the wav");
}
