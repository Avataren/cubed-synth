//! How close a transcription sounds to its original: the transcribed subsong
//! is played, the two register traces are aligned on their note starts, and
//! per voice per frame compared:
//!
//!  - `gate`: the voice sounds in both or in neither (gate on, test bit off);
//!  - `onsets`: audible note starts in both within a frame (F1 score);
//!  - `pitch`: where both sound, the pitch within half a semitone;
//!  - `wave`: where both sound, the same waveform bits;
//!  - `envelope`: at the note starts both share, the same attack/decay/sustain/release;
//!  - `level`: how loud the voice is, frame by frame, as the SID's envelope
//!    makes it of the writes (`envelope.rs`, the ADSR delay bug included):
//!    1 - the mean level difference / 255, where either sounds;
//!  - `pulse`: where both sound a pulse, 1 - the mean width difference / 2048.
//!
//! `score` weighs them (pitch 0.3, gate 0.15, waveform 0.15, level 0.15,
//! onsets 0.1, envelope 0.1, pulse 0.05), and when the original uses the
//! filter, the filter (mode, routing, cutoff within 8 of 255) for 0.15 of the
//! whole. A port of `psid/fidelity.ts`: the transcribed subsong is exported with
//! [`export`](super::export) and run on the emulated C64 like the original.

use super::capture::{capture_sid_with, CaptureOptions, SidTrace, SID_EVENT_GATE_LOW};
use super::envelope::{envelope_levels, EnvelopeFrame};
use super::export::{export_sid, SID_EXPORT_DEFAULTS};
use super::frames::{pitch_of, trace_frames, TraceFrames, VoiceFrames};
use super::util::js_round;
use crate::psid::c64::Kernal;
use crate::psid::file::PsidFile;
use crate::song::SidSong;

#[derive(Clone, Debug, Default)]
pub struct VoiceFidelity {
    pub gate: f64,
    pub onsets: f64,
    pub pitch: f64,
    pub wave: f64,
    pub envelope: f64,
    pub level: f64,
    pub pulse: f64,
    pub score: f64,
}

#[derive(Clone, Debug)]
pub struct Fidelity {
    pub voices: Vec<VoiceFidelity>,
    /// Where the original routes a voice through the filter: the same mode and routing, and the cutoff within 8 of 255.
    pub filter: f64,
    pub score: f64,
    /// Frames compared, and the transcription's delay against the original (the median of the voices').
    pub frames: usize,
    pub offset: i64,
    /// Each voice's own delay: a voice is compared with the original where its notes line up best.
    pub voice_offsets: Vec<i64>,
}

/// The filter's weight in the score when a tenth of the frames or more use it.
const FILTER_WEIGHT: f64 = 0.15;
/// The share of frames from which the filter counts in full.
const FILTER_FULL_SHARE: f64 = 0.1;

/// The delays (frames) a voice's transcription is tried at, and the stretch of the song they are scored over.
const MIN_DELAY: i64 = -30;
const MAX_DELAY: i64 = 90;
const SCAN_FRAMES: usize = 1500;
/// Delays whose score is within this of the best count as tied.
const VOICE_DELAY_GAIN: f64 = 0.01;

/// The voice's oscillator sounds under its gate: gate set, test bit clear (the test bit holds it silent).
fn sound_on(ctrl: u8) -> bool {
    ctrl & 0x09 == 0x01
}

/// A note starts audibly at frame `i`: a gate-on with the oscillator running,
/// or the test bit let go under a held gate (GoatTracker's first frame, `$09`,
/// then the note's waveform).
fn audible_onset_at(f: &TraceFrames, v: usize, i: usize) -> bool {
    let c = &f.voices[v].ctrl;
    if !sound_on(c[i]) {
        return false;
    }
    f.gate_on_at(v, i) || (i > 0 && c[i - 1] & 0x09 == 0x09)
}

/// The delay (frames) of `b` against `a` that lines up most note starts of the
/// voices, from `lo` to `hi`, and how many it lines up (0: none does, the delay means nothing).
fn best_offset(a: &TraceFrames, b: &TraceFrames, lo: i64, hi: i64, span: usize) -> (i64, i64) {
    let mut best = lo.max(0);
    let mut best_hits: i64 = -1;
    let n = span.min(a.frames);
    for off in lo..=hi {
        let mut hits = 0;
        for v in 0..3 {
            for i in 0..n {
                let j = i as i64 + off;
                if j < 0 || j >= b.frames as i64 {
                    continue;
                }
                if audible_onset_at(a, v, i) && audible_onset_at(b, v, j as usize) {
                    hits += 1;
                }
            }
        }
        if hits > best_hits {
            best = off;
            best_hits = hits;
        }
    }
    (best, best_hits.max(0))
}

/// Voice `v`'s envelope level at the end of each frame of `f`.
pub fn voice_levels(f: &TraceFrames, v: usize) -> Vec<u8> {
    let vf = &f.voices[v];
    let frames: Vec<EnvelopeFrame> = (0..f.frames)
        .map(|i| EnvelopeFrame {
            ad: vf.ad[i],
            sr: vf.sr[i],
            ctrl: vf.ctrl[i],
            gate_low: vf.events[i] & SID_EVENT_GATE_LOW != 0,
        })
        .collect();
    envelope_levels(&frames, js_round(f.clock_hz / f.rate_hz) as i64)
}

/// Voice `v` of `b` (delayed by `offset`) against voice `v` of `a`; `la`, `lb`: their envelope levels.
pub fn compare_voice(
    a: &TraceFrames,
    b: &TraceFrames,
    v: usize,
    offset: i64,
    max_frames: usize,
    la: &[u8],
    lb: &[u8],
) -> VoiceFidelity {
    let n = (max_frames as i64)
        .min(a.frames as i64)
        .min(b.frames as i64 - offset)
        .max(0);
    let va = &a.voices[v];
    let vb = &b.voices[v];
    let mut loud = 0i64;
    let mut level_err = 0i64;
    let mut gate = 0i64;
    let mut sounding = 0i64;
    let mut pitch = 0i64;
    let mut wave = 0i64;
    let mut pulse_n = 0i64;
    let mut pulse_err = 0i64;
    let mut on_a = 0i64;
    let mut on_b = 0i64;
    let mut on_both = 0i64;
    let mut env = 0i64;
    for i in 0..n {
        let j = i + offset;
        if j < 0 {
            continue;
        }
        let (iu, ju) = (i as usize, j as usize);
        let ca = va.ctrl[iu];
        let cb = vb.ctrl[ju];
        if la[iu] > 0 || lb[ju] > 0 {
            loud += 1;
            level_err += (la[iu] as i64 - lb[ju] as i64).abs();
        }
        if sound_on(ca) == sound_on(cb) {
            gate += 1;
        }
        if audible_onset_at(a, v, iu) {
            on_a += 1;
            let near = [j - 1, j, j + 1]
                .into_iter()
                .find(|&k| k >= 0 && k < b.frames as i64 && audible_onset_at(b, v, k as usize));
            if let Some(near) = near {
                on_both += 1;
                if va.ad[iu] == vb.ad[near as usize] && va.sr[iu] == vb.sr[near as usize] {
                    env += 1;
                }
            }
        }
        if audible_onset_at(b, v, ju) {
            on_b += 1;
        }
        if sound_on(ca) && sound_on(cb) {
            sounding += 1;
            let pa = pitch_of(va.freq[iu], a.clock_hz);
            let pb = pitch_of(vb.freq[ju], b.clock_hz);
            if (pa == f64::NEG_INFINITY && pb == f64::NEG_INFINITY) || (pa - pb).abs() <= 0.5 {
                pitch += 1;
            }
            if ca & 0xf0 == cb & 0xf0 {
                wave += 1;
            }
            if ca & 0x40 != 0 && cb & 0x40 != 0 {
                pulse_n += 1;
                pulse_err += (va.pw[iu] as i64 - vb.pw[ju] as i64).abs();
            }
        }
    }
    let f1 = if on_a + on_b == 0 {
        1.0
    } else {
        (2 * on_both) as f64 / (on_a + on_b) as f64
    };
    let nf = n as f64;
    let r = VoiceFidelity {
        gate: if n == 0 { 1.0 } else { gate as f64 / nf },
        onsets: f1,
        pitch: if sounding == 0 {
            1.0
        } else {
            pitch as f64 / sounding as f64
        },
        wave: if sounding == 0 {
            1.0
        } else {
            wave as f64 / sounding as f64
        },
        envelope: if on_both == 0 {
            if on_a == 0 {
                1.0
            } else {
                0.0
            }
        } else {
            env as f64 / on_both as f64
        },
        level: if loud == 0 {
            1.0
        } else {
            1.0 - level_err as f64 / loud as f64 / 255.0
        },
        pulse: if pulse_n == 0 {
            1.0
        } else {
            (1.0 - pulse_err as f64 / pulse_n as f64 / 2048.0).max(0.0)
        },
        score: 0.0,
    };
    let score = r.pitch * 0.3
        + r.gate * 0.15
        + r.wave * 0.15
        + r.level * 0.15
        + r.onsets * 0.1
        + r.envelope * 0.1
        + r.pulse * 0.05;
    VoiceFidelity { score, ..r }
}

/// Compare frames `a` (the original) with `b` (the transcription), each voice of `b` delayed by its offset.
pub fn compare_frames(
    a: &TraceFrames,
    b: &TraceFrames,
    delays: &[i64],
    max_frames: usize,
) -> Fidelity {
    let voice_offsets: Vec<i64> = (0..3)
        .map(|v| delays.get(v).or(delays.first()).copied().unwrap_or(0))
        .collect();
    let mut sorted = voice_offsets.clone();
    sorted.sort_unstable();
    let offset = sorted[1];
    let n = (max_frames as i64)
        .min(a.frames as i64)
        .min(b.frames as i64 - offset)
        .max(0) as usize;
    let voices: Vec<VoiceFidelity> = (0..3)
        .map(|v| {
            compare_voice(
                a,
                b,
                v,
                voice_offsets[v],
                max_frames,
                &voice_levels(a, v),
                &voice_levels(b, v),
            )
        })
        .collect();
    // The filter, where the original routes a voice through it.
    let mut filtered = 0i64;
    let mut filter_hits = 0i64;
    for i in 0..n {
        let j = i as i64 + offset;
        if j < 0 || a.resonance[i] & 7 == 0 {
            continue;
        }
        let ju = j as usize;
        filtered += 1;
        let same_routing = a.resonance[i] & 7 == b.resonance[ju] & 7;
        let same_mode = a.mode_volume[i] & 0x70 == b.mode_volume[ju] & 0x70;
        if same_routing
            && same_mode
            && ((a.cutoff[i] >> 3) as i64 - (b.cutoff[ju] >> 3) as i64).abs() <= 8
        {
            filter_hits += 1;
        }
    }
    let filter = if filtered == 0 {
        1.0
    } else {
        filter_hits as f64 / filtered as f64
    };
    // Voices that never sound in the original count less; the filter counts when it is used.
    let weight = |v: usize| -> f64 {
        if a.voices[v].ctrl.iter().any(|&c| sound_on(c)) {
            1.0
        } else {
            0.2
        }
    };
    let total = weight(0) + weight(1) + weight(2);
    let mut voice_score = 0.0;
    for (v, x) in voices.iter().enumerate() {
        voice_score += x.score * weight(v);
    }
    voice_score /= total;
    // The filter counts for what it is used: in full from a tenth of the frames (the share at which
    // the transcriber sets a song's own filter, `static_filter`), less below. A filter routed in 1% of
    // a song's frames costs that song a fifteenth of what a filter all through it does.
    let filter_weight = if filtered == 0 {
        0.0
    } else {
        FILTER_WEIGHT * (1.0f64).min(filtered as f64 / (1.0f64).max(n as f64) / FILTER_FULL_SHARE)
    };
    let score = voice_score * (1.0 - filter_weight) + filter * filter_weight;
    Fidelity {
        voices,
        filter,
        score,
        frames: n,
        offset,
        voice_offsets,
    }
}

/// `f` played on past its end through its exact loop, to `n` frames (`f`
/// itself when it has no loop or is long enough): a capture stops where its
/// state repeats, and the music goes on from `loop_frame` just the same.
pub fn unrolled(f: &TraceFrames, n: usize) -> TraceFrames {
    let Some(lp) = f.loop_frame else {
        return f.clone();
    };
    if lp >= f.frames || f.frames >= n {
        return f.clone();
    }
    let period = f.frames - lp;
    let at = |i: usize| -> usize {
        if i < f.frames {
            i
        } else {
            lp + ((i - f.frames) % period)
        }
    };
    let u = |src: &[u8]| -> Vec<u8> { (0..n).map(|i| src[at(i)]).collect() };
    let u16s = |src: &[u16]| -> Vec<u16> { (0..n).map(|i| src[at(i)]).collect() };
    TraceFrames {
        frames: n,
        voices: f
            .voices
            .iter()
            .map(|v| VoiceFrames {
                freq: u16s(&v.freq),
                pw: u16s(&v.pw),
                ctrl: u(&v.ctrl),
                ad: u(&v.ad),
                sr: u(&v.sr),
                events: u(&v.events),
            })
            .collect(),
        cutoff: u16s(&f.cutoff),
        resonance: u(&f.resonance),
        mode_volume: u(&f.mode_volume),
        ..f.clone()
    }
}

/// `song` with subsong `gt_subsong` as its only one: what a player is handed to play that subsong.
fn song_for_subsong(song: &SidSong, gt_subsong: usize) -> SidSong {
    let s = gt_subsong.min(song.subsongs.len().saturating_sub(1));
    if s == 0 && song.subsongs.len() == 1 {
        return song.clone();
    }
    let mut one = song.clone();
    one.subsongs = vec![song.subsongs[s].clone()];
    one
}

/// The fidelity of `song`'s subsong `gt_subsong` against the original `trace`, or the reason it could not be measured.
///
/// As the DAW measures it: the subsong is exported as a `.sid` (GoatTracker's player with the song
/// packed after it, `export`) and run on the same emulated C64 as the original.
pub fn measure_fidelity(
    trace: &SidTrace,
    song: &SidSong,
    gt_subsong: usize,
    max_frames: usize,
    kernal: Kernal,
) -> Result<Fidelity, String> {
    let exported = export_sid(&song_for_subsong(song, gt_subsong), &SID_EXPORT_DEFAULTS)?;
    let parsed = PsidFile::parse(&exported).map_err(|e| e.to_string())?;
    let a = unrolled(&trace_frames(trace), max_frames);
    let seconds = 600f64.min((max_frames + 200) as f64 / a.rate_hz.max(1.0) + 5.0);
    let capture = capture_sid_with(
        &parsed,
        &CaptureOptions {
            subsong: 0,
            max_ticks: None,
            max_seconds: Some(seconds),
            cancel: None,
        },
        kernal,
    )?;
    let b = unrolled(&trace_frames(&capture), max_frames + 200);
    // Each voice's delay: where it scores best over a stretch of the song (a transcription's voices need
    // not start at the same frame, and a tune whose rows repeat every few frames has note starts that
    // line up at many delays, which the score tells apart). Delays that score alike keep the one nearest
    // the voices' common delay, counted from their note starts.
    let common = best_offset(&a, &b, -8, 80, 1500).0;
    let scan = SCAN_FRAMES.min(max_frames);
    let offsets: Vec<i64> = (0..3)
        .map(|v| {
            let la = voice_levels(&a, v);
            let lb = voice_levels(&b, v);
            let mut scores: Vec<(i64, f64)> = Vec::new();
            let mut top = -1.0f64;
            for off in MIN_DELAY..=MAX_DELAY {
                let score = compare_voice(&a, &b, v, off, scan, &la, &lb).score;
                scores.push((off, score));
                if score > top {
                    top = score;
                }
            }
            let mut best = common;
            let mut best_gap = i64::MAX;
            for &(off, score) in &scores {
                if score < top - VOICE_DELAY_GAIN {
                    continue;
                }
                let gap = (off - common).abs();
                if gap < best_gap {
                    best = off;
                    best_gap = gap;
                }
            }
            // The scan only proposes: over the whole stretch compared, the candidate must beat the common delay.
            if best != common {
                let own = compare_voice(&a, &b, v, best, max_frames, &la, &lb).score;
                let shared = compare_voice(&a, &b, v, common, max_frames, &la, &lb).score;
                if own <= shared + VOICE_DELAY_GAIN {
                    best = common;
                }
            }
            best
        })
        .collect();
    Ok(compare_frames(&a, &b, &offsets, max_frames))
}
