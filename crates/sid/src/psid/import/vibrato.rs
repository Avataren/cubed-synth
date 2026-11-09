//! GoatTracker's vibrato as a speed-table row: the shapes it can swing in, and
//! the row that fits a swing. A port of `transcribe/vibrato.ts`.

use std::sync::OnceLock;

/// GoatTracker's vibrato (player.s `mt_effect_4`) for compare value `speed`: per frame, +1 (add) or -1 (subtract).
fn gt_vibrato_directions(speed: i32, frames: usize) -> Vec<i32> {
    let mut out = Vec::with_capacity(frames);
    let mut time: i32 = 0;
    for _ in 0..frames {
        let mut a = time;
        if a & 0x80 != 0 || a <= speed {
            a = (a + 2) & 0xff;
        } else {
            a = ((a ^ 0xff) + 2) & 0xff;
        }
        time = a;
        out.push(if a & 1 != 0 { -1 } else { 1 });
    }
    out
}

struct VibratoShape {
    speed: i32,
    period: i32,
    /// Peak to peak, in steps.
    span: i32,
}

fn shapes() -> &'static [VibratoShape] {
    static SHAPES: OnceLock<Vec<VibratoShape>> = OnceLock::new();
    SHAPES.get_or_init(|| {
        let mut out = Vec::new();
        for s in 1..=30 {
            let mut pos = 0;
            let path: Vec<i32> = gt_vibrato_directions(s, 200)
                .into_iter()
                .map(|d| {
                    pos += d;
                    pos
                })
                .collect();
            let tail = &path[100..];
            let span = tail.iter().max().unwrap() - tail.iter().min().unwrap();
            for p in 2..64usize {
                if (0..60).all(|i| tail.get(i + p).is_some_and(|&t| t == tail[i])) {
                    out.push(VibratoShape {
                        speed: s,
                        period: p as i32,
                        span,
                    });
                    break;
                }
            }
        }
        out
    })
}

/// The speed-table row of a swing of `period` frames and `span` semitones peak
/// to peak (`deeper`: extra depth shifts, for a pattern vibrato, which the
/// player swings about 4 times deeper than an instrument's model): `left` is
/// `$80 | compare value`, `right` the depth shift.
pub fn vibrato_for(period: f64, span: f64, deeper: i32) -> (u8, u8) {
    let mut speed = 1;
    let mut best_err = f64::INFINITY;
    let mut chosen = None;
    for shape in shapes() {
        let err = (shape.period as f64 - period).abs();
        if err < best_err {
            speed = shape.speed;
            best_err = err;
            chosen = Some(shape);
        }
    }
    // One step of the calculated speed is 1 / 2^shift of the note step.
    let span_steps = chosen.map_or(f64::NAN, |s| s.span as f64);
    let shift = super::util::js_round((span_steps / span).log2()).clamp(0.0, 8.0) as i32;
    (0x80 | speed as u8, (shift + deeper).min(8) as u8)
}
