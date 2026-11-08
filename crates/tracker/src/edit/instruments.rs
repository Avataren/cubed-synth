//! Editing samples and instruments: fields, loops, loading PCM and WAV
//! files, XM instruments (samples, keymap, envelopes, fadeout, vibrato).

use serde_json::{json, Value};

use super::history::{instrument_meta, sample_meta, Change, Changes};
use super::json::Res;
use super::limits::{clean_name, format_name, limits, note_range};
use super::wav::{self, ChannelMix, Pcm, Quality};
use super::ModuleDoc;
use crate::module::{
    Adlib, AutoVibrato, Envelope, Format, Instrument, LoopKind, ModFlavor, Pattern, Sample,
};
use crate::pitch::AMIGA_CLOCK;
use crate::write::mod_period_for_note;

const MOD_MAX_SAMPLE: usize = 0x1fffe;
const S3M_MAX_SAMPLE: usize = 1_000_000;
const XM_MAX_SAMPLE: usize = 4_000_000;
/// The rate an XM sample at relative note 0 plays at when C-4 is struck.
const XM_BASE_RATE: f64 = 8363.0;
/// PAL Paula's clock.
const PAULA_CLOCK: f64 = AMIGA_CLOCK / 2.0;

/// Which sample or instrument an edit means. MOD and S3M: `instrument` is the
/// sample slot as a cell numbers it (1-based) and `sample` is 0. XM:
/// `instrument` is the instrument (1-based), `sample` one of its samples
/// (0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleRef {
    pub instrument: usize,
    pub sample: usize,
}

impl SampleRef {
    pub fn new(instrument: usize, sample: usize) -> SampleRef {
        SampleRef { instrument, sample }
    }
}

/// Fields of a sample to change (`None` leaves them).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SampleEdit {
    pub name: Option<String>,
    /// 0-64.
    pub volume: Option<u8>,
    /// MOD: -8 to 7. XM: -128 to 127.
    pub finetune: Option<i32>,
    /// XM: -96 to 95.
    pub relative_note: Option<i32>,
    /// XM: 0-255.
    pub pan: Option<u8>,
    /// S3M: 1-65535.
    pub c2spd: Option<u32>,
    pub loop_kind: Option<LoopKind>,
    pub loop_start: Option<usize>,
    pub loop_len: Option<usize>,
    /// S3M: an AdLib instrument's OPL registers.
    pub adlib: Option<Adlib>,
}

/// Where loaded sample bytes come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleSource {
    /// A WAV file (PCM or float, any channel count).
    Wav,
    /// Raw PCM: 8-bit (signed, or unsigned) or 16-bit little-endian signed.
    Raw {
        bits: u8,
        unsigned: bool,
        channels: usize,
        rate: u32,
    },
}

/// How loaded audio becomes a sample.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportOptions {
    pub source: SampleSource,
    /// How stereo becomes mono.
    pub mix: ChannelMix,
    /// How the rate is changed where it has to be (a MOD's, or `target_rate`).
    pub quality: Quality,
    /// MOD: the note (MIDI, C-2 is 36) the sample sounds at its own pitch
    /// when played; the audio is resampled to that note's Paula rate. Default
    /// C-2 (period 428, 8287 Hz).
    pub note: Option<u8>,
    /// S3M, XM: resample to this rate first. Without it the audio keeps its
    /// rate (S3M: `c2spd`; XM: relative note and finetune carry it).
    pub target_rate: Option<u32>,
    /// S3M, XM: the sample grid, 8 or 16 bits (default 8 for S3M, 16 for XM).
    /// A MOD's is 8.
    pub bits: Option<u8>,
    /// Scale to the loudest value the grid holds.
    pub normalize: bool,
    pub name: Option<String>,
    /// Take the loop a WAV's `smpl` chunk has.
    pub keep_loop: bool,
    /// Cut audio that is longer than the format holds instead of refusing.
    pub truncate: bool,
}

impl ImportOptions {
    pub fn wav() -> ImportOptions {
        ImportOptions {
            source: SampleSource::Wav,
            mix: ChannelMix::Mix,
            quality: Quality::Sinc,
            note: None,
            target_rate: None,
            bits: None,
            normalize: false,
            name: None,
            keep_loop: true,
            truncate: false,
        }
    }

    pub fn raw(bits: u8, unsigned: bool, channels: usize, rate: u32) -> ImportOptions {
        ImportOptions {
            source: SampleSource::Raw {
                bits,
                unsigned,
                channels,
                rate,
            },
            keep_loop: false,
            ..ImportOptions::wav()
        }
    }
}

/// The shapes `generate_wave` makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wave {
    Sine,
    Triangle,
    Saw,
    Square,
    Pulse,
    Noise,
}

impl Wave {
    pub fn parse(name: &str) -> Option<Wave> {
        match name {
            "sine" => Some(Wave::Sine),
            "triangle" => Some(Wave::Triangle),
            "saw" => Some(Wave::Saw),
            "square" => Some(Wave::Square),
            "pulse" => Some(Wave::Pulse),
            "noise" => Some(Wave::Noise),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Wave::Sine => "sine",
            Wave::Triangle => "triangle",
            Wave::Saw => "saw",
            Wave::Square => "square",
            Wave::Pulse => "pulse",
            Wave::Noise => "noise",
        }
    }
}

/// Which envelope of an XM instrument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeKind {
    Volume,
    Panning,
}

/// A change to an XM envelope (`None` leaves a part alone).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvelopeEdit {
    /// Turn the envelope on or off.
    pub enabled: Option<bool>,
    /// New points as (tick, value 0-64).
    pub points: Option<Vec<(u16, u16)>>,
    /// The sustain point: `Some(None)` removes it.
    pub sustain: Option<Option<usize>>,
    /// The loop's first and last point: `Some(None)` removes it.
    pub looped: Option<Option<(usize, usize)>>,
}

/// A change to an XM instrument (`None` leaves a field).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstrumentEdit {
    pub name: Option<String>,
    /// 0-4095.
    pub fadeout: Option<u16>,
    pub vibrato_kind: Option<u8>,
    pub vibrato_sweep: Option<u8>,
    pub vibrato_depth: Option<u8>,
    pub vibrato_rate: Option<u8>,
}

fn loop_name(k: LoopKind) -> &'static str {
    match k {
        LoopKind::Off => "off",
        LoopKind::Forward => "forward",
        LoopKind::PingPong => "pingpong",
    }
}

/// A loop kind from its name.
pub fn parse_loop_kind(name: &str) -> Option<LoopKind> {
    match name {
        "off" | "none" => Some(LoopKind::Off),
        "forward" => Some(LoopKind::Forward),
        "pingpong" | "ping-pong" | "bidi" => Some(LoopKind::PingPong),
        _ => None,
    }
}

fn envelope_json(e: &Option<Envelope>) -> Value {
    match e {
        None => Value::Null,
        Some(e) => json!({
            "points": e.points.iter().map(|&(t, v)| json!([t, v])).collect::<Vec<_>>(),
            "sustain": e.sustain,
            "loop": e.looped.map(|(a, b)| json!([a, b])),
        }),
    }
}

/// The rate a sample plays at when its format's reference note is struck.
fn natural_rate(format: Format, s: &Sample) -> f64 {
    match format {
        Format::Mod => PAULA_CLOCK / 428.0,
        Format::S3m => s.c2spd as f64,
        Format::Xm => {
            XM_BASE_RATE * 2f64.powf((s.relative_note as f64 + s.finetune as f64 / 128.0) / 12.0)
        }
    }
}

impl ModuleDoc {
    // ----- addressing -------------------------------------------------------------------

    /// The index in the module's sample list of the sample `r` names.
    pub fn resolve_sample(&self, r: SampleRef) -> Res<usize> {
        let m = &self.module;
        match m.format {
            Format::Xm => {
                let inst = r
                    .instrument
                    .checked_sub(1)
                    .and_then(|i| m.instruments.get(i))
                    .ok_or_else(|| {
                        format!(
                            "instrument {} does not exist (the module has {})",
                            r.instrument,
                            m.instruments.len()
                        )
                    })?;
                inst.samples.get(r.sample).copied().ok_or_else(|| {
                    format!(
                        "instrument {} has {} samples; sample {} does not exist (samples count from 0)",
                        r.instrument,
                        inst.samples.len(),
                        r.sample
                    )
                })
            }
            _ => {
                if r.sample != 0 {
                    return Err(format!(
                        "{} has one sample per slot (sample 0)",
                        format_name(m.format)
                    ));
                }
                if r.instrument == 0 || r.instrument > m.samples.len() {
                    return Err(format!(
                        "sample {} does not exist (the module has {})",
                        r.instrument,
                        m.samples.len()
                    ));
                }
                Ok(r.instrument - 1)
            }
        }
    }

    fn instrument_index(&self, instrument: usize) -> Res<usize> {
        if self.module.format != Format::Xm {
            return Err("only an XM has instruments apart from its samples".into());
        }
        instrument
            .checked_sub(1)
            .filter(|&i| i < self.module.instruments.len())
            .ok_or_else(|| {
                format!(
                    "instrument {instrument} does not exist (the module has {})",
                    self.module.instruments.len()
                )
            })
    }

    // ----- reading ------------------------------------------------------------------------

    fn sample_json(&self, instrument: usize, slot: usize, s: &Sample) -> Value {
        let m = &self.module;
        json!({
            "instrument": instrument,
            "sample": slot,
            "name": s.name,
            "length": s.data.len(),
            "volume": s.volume,
            "finetune": if m.format == Format::S3m { Value::Null } else { json!(s.finetune) },
            "relative_note": if m.format == Format::Xm { json!(s.relative_note) } else { Value::Null },
            "pan": if m.format == Format::Xm { json!(s.pan.unwrap_or(128)) } else { Value::Null },
            "c2spd": if m.format == Format::S3m { json!(s.c2spd) } else { Value::Null },
            "loop_kind": loop_name(s.loop_kind),
            "loop_start": s.loop_start,
            "loop_len": s.loop_len,
            "rate": natural_rate(m.format, s).round(),
            "adlib": s.adlib.map(|a| json!({"melodic": a.melodic, "registers": a.registers.to_vec()})),
        })
    }

    /// Every sample: `{"samples": [{"instrument", "sample", "name", "length",
    /// "volume", "finetune", "relative_note", "pan", "c2spd", "loop_kind",
    /// "loop_start", "loop_len", "rate", "adlib"}]}`. `instrument` and `sample`
    /// are the numbers [`SampleRef`] takes; fields a format lacks are null;
    /// `rate` is the Hz the sample sounds at when its reference note is
    /// struck (a MOD's C-2, an S3M's C-4 by `c2spd`, an XM's C-4).
    pub fn samples(&self) -> Value {
        let m = &self.module;
        let list: Vec<Value> = match m.format {
            Format::Xm => m
                .instruments
                .iter()
                .enumerate()
                .flat_map(|(i, inst)| {
                    inst.samples
                        .iter()
                        .enumerate()
                        .map(move |(k, &g)| (i, k, g))
                })
                .map(|(i, k, g)| self.sample_json(i + 1, k, &m.samples[g]))
                .collect(),
            _ => m
                .samples
                .iter()
                .enumerate()
                .map(|(i, s)| self.sample_json(i + 1, 0, s))
                .collect(),
        };
        json!({ "samples": list })
    }

    /// The instruments a cell can name: `{"instruments": [{"n", "name",
    /// "samples": count, ...}]}`. A MOD's and S3M's sample slots are
    /// instruments of one sample (no more fields); an XM's carry
    /// `volume_envelope` and `panning_envelope` (`{"points": [[tick, value]],
    /// "sustain": point or null, "loop": [first, last] or null}` or null),
    /// `fadeout`, `vibrato` and the `keymap` (96 sample numbers, the first
    /// for C-0, MIDI 12).
    pub fn instruments(&self) -> Value {
        let m = &self.module;
        let list: Vec<Value> = match m.format {
            Format::Xm => m
                .instruments
                .iter()
                .enumerate()
                .map(|(i, inst)| {
                    json!({
                        "n": i + 1,
                        "name": inst.name,
                        "samples": inst.samples.len(),
                        "volume_envelope": envelope_json(&inst.volume_envelope),
                        "panning_envelope": envelope_json(&inst.panning_envelope),
                        "fadeout": inst.fadeout,
                        "vibrato": {
                            "kind": inst.vibrato.kind,
                            "sweep": inst.vibrato.sweep,
                            "depth": inst.vibrato.depth,
                            "rate": inst.vibrato.rate,
                        },
                        "keymap": inst.keymap.to_vec(),
                    })
                })
                .collect(),
            _ => m
                .samples
                .iter()
                .enumerate()
                .map(|(i, s)| json!({"n": i + 1, "name": s.name, "samples": 1}))
                .collect(),
        };
        json!({ "instruments": list })
    }

    /// The sample's waveform for drawing: `buckets` (min, max) pairs over
    /// frames `from` up to `to` (the whole sample by default).
    pub fn sample_peaks(
        &self,
        r: SampleRef,
        buckets: usize,
        range: Option<(usize, usize)>,
    ) -> Res<Value> {
        let s = &self.module.samples[self.resolve_sample(r)?];
        let (from, to) = range.unwrap_or((0, s.data.len()));
        let to = to.min(s.data.len());
        let from = from.min(to);
        let buckets = buckets.clamp(1, 16384);
        let span = to - from;
        let mut peaks = Vec::with_capacity(buckets);
        for b in 0..buckets {
            let a = from + span * b / buckets;
            let z = (from + span * (b + 1) / buckets).max(a + usize::from(a < to));
            let z = z.min(to);
            let (mut lo, mut hi) = (0f32, 0f32);
            if a < z {
                lo = f32::MAX;
                hi = f32::MIN;
                for &v in &s.data[a..z] {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            peaks.push(json!([lo, hi]));
        }
        Ok(json!({"frames": s.data.len(), "from": from, "to": to, "peaks": peaks}))
    }

    /// The sample as a 16-bit mono WAV, at the rate its reference note plays it
    /// at.
    pub fn sample_wav(&self, r: SampleRef) -> Res<Vec<u8>> {
        let s = &self.module.samples[self.resolve_sample(r)?];
        let rate = natural_rate(self.module.format, s).round().clamp(1.0, 4e6) as u32;
        Ok(wav::encode_wav(&s.data, rate))
    }

    // ----- sample fields -----------------------------------------------------------------

    /// The loop a sample of `len` frames keeps after an edit: inside the data,
    /// on the format's grid, or none.
    fn fit_loop(
        &self,
        len: usize,
        kind: LoopKind,
        start: usize,
        size: usize,
    ) -> Res<(LoopKind, usize, usize)> {
        let format = self.module.format;
        if kind == LoopKind::PingPong && format != Format::Xm {
            return Err(format!("{} has no ping-pong loops", format_name(format)));
        }
        if kind == LoopKind::Off {
            return Ok((LoopKind::Off, 0, 0));
        }
        let ust = self.module.mod_flavor == Some(ModFlavor::UltimateSoundtracker);
        let even = format == Format::Mod;
        let (mut start, mut size) = (start, size);
        if even {
            size &= !1;
            if !ust {
                start &= !1;
            }
        }
        if start >= len {
            return Ok((LoopKind::Off, 0, 0));
        }
        size = size.min(len - start);
        if even {
            size &= !1;
        }
        let min = if format == Format::Mod { 3 } else { 2 };
        if size < min {
            return Ok((LoopKind::Off, 0, 0));
        }
        Ok((kind, start, size))
    }

    /// Changes fields of a sample; the loop is fitted to the data (inside it,
    /// in whole words for a MOD) and the result is returned as stored.
    pub fn set_sample(&mut self, r: SampleRef, edit: &SampleEdit) -> Res<(Value, Changes)> {
        let index = self.resolve_sample(r)?;
        let m = &self.module;
        let lim = limits(m);
        let mut meta = sample_meta(&m.samples[index]);
        let len = m.samples[index].data.len();
        if let Some(name) = &edit.name {
            meta.name = clean_name(name, lim.max_name)?.0;
        }
        if let Some(v) = edit.volume {
            if v > 64 {
                return Err(format!("the volume is 0-64, not {v}"));
            }
            meta.volume = v;
        }
        if let Some(f) = edit.finetune {
            match m.format {
                Format::Mod if (-8..=7).contains(&f) => meta.finetune = f as i8,
                Format::Mod => return Err(format!("a MOD's finetune is -8 to 7, not {f}")),
                Format::Xm if (-128..=127).contains(&f) => meta.finetune = f as i8,
                Format::Xm => return Err(format!("an XM's finetune is -128 to 127, not {f}")),
                Format::S3m => {
                    return Err("an S3M sample is tuned by its c2spd, not a finetune".into())
                }
            }
        }
        if let Some(n) = edit.relative_note {
            if m.format != Format::Xm {
                return Err(format!(
                    "{} has no relative note (an XM does)",
                    format_name(m.format)
                ));
            }
            if !(-96..=95).contains(&n) {
                return Err(format!("the relative note is -96 to 95, not {n}"));
            }
            meta.relative_note = n as i8;
        }
        if let Some(p) = edit.pan {
            if m.format != Format::Xm {
                return Err(format!(
                    "{} samples have no pan of their own (an XM's do)",
                    format_name(m.format)
                ));
            }
            meta.pan = Some(p);
        }
        if let Some(c) = edit.c2spd {
            if m.format != Format::S3m {
                return Err(format!(
                    "{} has no c2spd (an S3M does)",
                    format_name(m.format)
                ));
            }
            if !(1..=65535).contains(&c) {
                return Err(format!("c2spd is 1-65535, not {c}"));
            }
            meta.c2spd = c;
        }
        if let Some(a) = edit.adlib {
            if m.format != Format::S3m {
                return Err("only an S3M has AdLib instruments".into());
            }
            if !m.samples[index].data.is_empty() {
                return Err(
                    "this slot holds PCM: clear it before making it an AdLib instrument".into(),
                );
            }
            meta.adlib = Some(a);
        }
        if edit.loop_kind.is_some() || edit.loop_start.is_some() || edit.loop_len.is_some() {
            if meta.adlib.is_some() {
                return Err("an AdLib instrument has no loop".into());
            }
            let kind = edit.loop_kind.unwrap_or(meta.loop_kind);
            let explicit_len = edit.loop_len;
            let start = edit.loop_start.unwrap_or(meta.loop_start);
            // A loop turned on without a size takes the rest of the sample.
            let size = explicit_len.unwrap_or(if meta.loop_kind == LoopKind::Off {
                len.saturating_sub(start)
            } else {
                meta.loop_len
            });
            let (k, s, z) = self.fit_loop(len, kind, start, size)?;
            meta.loop_kind = k;
            meta.loop_start = s;
            meta.loop_len = z;
        }
        let changed = meta != sample_meta(&m.samples[index]);
        let ((), changes) = self.edit(|d| {
            if changed {
                d.change(Change::SampleMeta { index, meta });
            }
            Ok(())
        })?;
        let slot_json = self.sample_json(r.instrument, r.sample, &self.module.samples[index]);
        Ok((slot_json, changes))
    }

    // ----- sample data ---------------------------------------------------------------------

    /// Puts `data` (already on the sample grid) into a sample, with the
    /// loop refitted, and `meta` as its other fields.
    fn put_sample(&mut self, index: usize, data: Vec<f32>, mut meta: Sample) -> Res<()> {
        let (k, s, z) =
            self.fit_loop(data.len(), meta.loop_kind, meta.loop_start, meta.loop_len)?;
        meta.loop_kind = k;
        meta.loop_start = s;
        meta.loop_len = z;
        self.change(Change::SampleData { index, data });
        self.change(Change::SampleMeta { index, meta });
        Ok(())
    }

    /// Loads audio into a sample (WAV file bytes, or raw PCM) and converts it
    /// for the format; see [`ImportOptions`]. The sample's name, volume and
    /// tuning fields stay (the tuning that carries the rate is set); the loop
    /// is the file's when `keep_loop`, else none.
    pub fn import_sample(
        &mut self,
        r: SampleRef,
        bytes: &[u8],
        opts: &ImportOptions,
    ) -> Res<(Value, Changes)> {
        let index = self.resolve_sample(r)?;
        let format = self.module.format;
        if self.module.samples[index].adlib.is_some() {
            return Err("this slot is an AdLib instrument: clear it before loading audio".into());
        }
        // Decode.
        let pcm = match opts.source {
            SampleSource::Wav => wav::decode_wav(bytes, opts.mix)?,
            SampleSource::Raw {
                bits,
                unsigned,
                channels,
                rate,
            } => {
                if rate == 0 {
                    return Err("raw audio needs a sample rate".into());
                }
                Pcm {
                    rate,
                    data: wav::decode_raw(bytes, bits, unsigned, channels, opts.mix)?,
                    loop_: None,
                }
            }
        };
        if pcm.data.is_empty() {
            return Err("the audio has no frames".into());
        }
        // Rate and tuning by format.
        let mut meta = sample_meta(&self.module.samples[index]);
        let (max_frames, default_bits) = match format {
            Format::Mod => (MOD_MAX_SAMPLE, 8),
            Format::S3m => (S3M_MAX_SAMPLE, 8),
            Format::Xm => (XM_MAX_SAMPLE, 16),
        };
        let bits = if format == Format::Mod {
            8
        } else {
            match opts.bits.unwrap_or(default_bits) {
                b @ (8 | 16) => b,
                b => return Err(format!("the sample grid is 8 or 16 bits, not {b}")),
            }
        };
        let (target, ()) = match format {
            Format::Mod => {
                let note = opts.note.unwrap_or(36);
                let period = mod_period_for_note(note, false)
                    .ok_or_else(|| format!("note {note} has no Amiga period"))?;
                (PAULA_CLOCK / period as f64, ())
            }
            Format::S3m => {
                let rate = opts.target_rate.unwrap_or(pcm.rate).clamp(1, 65535);
                meta.c2spd = rate;
                (rate as f64, ())
            }
            Format::Xm => {
                let rate = opts.target_rate.unwrap_or(pcm.rate).max(1);
                let semitones = 12.0 * (rate as f64 / XM_BASE_RATE).log2();
                let rel = semitones.round().clamp(-96.0, 95.0);
                meta.relative_note = rel as i8;
                meta.finetune = ((semitones - rel) * 128.0).round().clamp(-128.0, 127.0) as i8;
                (rate as f64, ())
            }
        };
        let mut data = if (target - pcm.rate as f64).abs() < 1e-6 {
            pcm.data.clone()
        } else {
            wav::resample(&pcm.data, pcm.rate as f64, target, opts.quality)
        };
        let scale = data.len() as f64 / pcm.data.len() as f64;
        if opts.normalize {
            // The loudest value the grid holds, so the peak survives rounding.
            let top = if bits == 8 {
                127.0 / 128.0
            } else {
                32767.0 / 32768.0
            };
            wav::normalize(&mut data, top);
        }
        wav::quantize_to(&mut data, bits);
        if format == Format::Mod && !data.len().is_multiple_of(2) {
            data.push(0.0);
        }
        if data.len() > max_frames {
            if !opts.truncate {
                return Err(format!(
                    "{} frames is too long for {} (at most {max_frames}): lower the rate, cut the audio or pass truncate",
                    data.len(),
                    format_name(format)
                ));
            }
            data.truncate(max_frames);
        }
        if let Some(name) = &opts.name {
            meta.name = clean_name(name, limits(&self.module).max_name)?.0;
        }
        if meta.volume == 0 && self.module.samples[index].data.is_empty() {
            meta.volume = 64;
        }
        if format == Format::S3m && meta.c2spd == 0 {
            meta.c2spd = 8363;
        }
        // The loop.
        meta.loop_kind = LoopKind::Off;
        meta.loop_start = 0;
        meta.loop_len = 0;
        if opts.keep_loop {
            if let Some((start, len, kind)) = pcm.loop_ {
                meta.loop_kind = if kind == LoopKind::PingPong && format != Format::Xm {
                    LoopKind::Forward
                } else {
                    kind
                };
                meta.loop_start = (start as f64 * scale).round() as usize;
                meta.loop_len = (len as f64 * scale).round() as usize;
            }
        }
        let ((), changes) = self.edit(|d| d.put_sample(index, data, meta))?;
        let slot_json = self.sample_json(r.instrument, r.sample, &self.module.samples[index]);
        Ok((slot_json, changes))
    }

    /// Replaces a sample's frames with `data` (-1 to 1) as they are, on the
    /// sample's grid (8-bit for a MOD; S3M and XM keep the width the data
    /// needs, up to 16 bits). The loop is refitted.
    pub fn set_sample_data(&mut self, r: SampleRef, mut data: Vec<f32>) -> Res<(Value, Changes)> {
        let index = self.resolve_sample(r)?;
        let format = self.module.format;
        let max = match format {
            Format::Mod => MOD_MAX_SAMPLE,
            Format::S3m => S3M_MAX_SAMPLE,
            Format::Xm => XM_MAX_SAMPLE,
        };
        wav::quantize_to(&mut data, if format == Format::Mod { 8 } else { 16 });
        if format == Format::Mod && !data.len().is_multiple_of(2) {
            data.push(0.0);
        }
        if data.len() > max {
            return Err(format!(
                "{} frames is too long for {} (at most {max})",
                data.len(),
                format_name(format)
            ));
        }
        let meta = sample_meta(&self.module.samples[index]);
        let ((), changes) = self.edit(|d| d.put_sample(index, data, meta))?;
        let j = self.sample_json(r.instrument, r.sample, &self.module.samples[index]);
        Ok((j, changes))
    }

    /// Scales the sample so its peak reaches full scale (the grid's largest
    /// value).
    pub fn normalize_sample(&mut self, r: SampleRef) -> Res<Changes> {
        let index = self.resolve_sample(r)?;
        let mut data = self.module.samples[index].data.clone();
        let bits = if self.module.format == Format::Mod {
            8
        } else {
            16
        };
        let top = if bits == 8 {
            127.0 / 128.0
        } else {
            32767.0 / 32768.0
        };
        wav::normalize(&mut data, top);
        wav::quantize_to(&mut data, bits);
        let meta = sample_meta(&self.module.samples[index]);
        let ((), changes) = self.edit(|d| d.put_sample(index, data, meta))?;
        Ok(changes)
    }

    /// Reverses the sample (its loop moves with it).
    pub fn reverse_sample(&mut self, r: SampleRef) -> Res<Changes> {
        let index = self.resolve_sample(r)?;
        let s = &self.module.samples[index];
        let mut data = s.data.clone();
        data.reverse();
        let mut meta = sample_meta(s);
        if meta.loop_kind != LoopKind::Off {
            meta.loop_start = data.len().saturating_sub(meta.loop_start + meta.loop_len);
        }
        let ((), changes) = self.edit(|d| d.put_sample(index, data, meta))?;
        Ok(changes)
    }

    /// Empties a sample: no data, no loop; the volume stays. In a MOD or S3M
    /// the slot becomes an empty one; its name stays.
    pub fn clear_sample(&mut self, r: SampleRef) -> Res<Changes> {
        let index = self.resolve_sample(r)?;
        let mut meta = sample_meta(&self.module.samples[index]);
        meta.loop_kind = LoopKind::Off;
        meta.loop_start = 0;
        meta.loop_len = 0;
        meta.adlib = None;
        let ((), changes) = self.edit(|d| d.put_sample(index, Vec::new(), meta))?;
        Ok(changes)
    }

    /// Makes a looped single-cycle waveform of `length` frames (even, for a
    /// MOD) with the given shape: the classic chip instrument. `seed` makes
    /// noise repeatable.
    pub fn generate_wave(
        &mut self,
        r: SampleRef,
        wave: Wave,
        length: usize,
        seed: u32,
    ) -> Res<(Value, Changes)> {
        let index = self.resolve_sample(r)?;
        let format = self.module.format;
        let max = if format == Format::Mod {
            MOD_MAX_SAMPLE
        } else {
            S3M_MAX_SAMPLE
        };
        if !(4..=max).contains(&length) {
            return Err(format!("a wave is 4 to {max} frames, not {length}"));
        }
        if self.module.samples[index].adlib.is_some() {
            return Err("this slot is an AdLib instrument".into());
        }
        let length = if format == Format::Mod {
            length & !1
        } else {
            length
        };
        let mut state = seed.max(1);
        let mut data: Vec<f32> = (0..length)
            .map(|i| {
                let t = i as f32 / length as f32;
                match wave {
                    Wave::Sine => (t * std::f32::consts::TAU).sin(),
                    Wave::Triangle => {
                        if t < 0.5 {
                            4.0 * t - 1.0
                        } else {
                            3.0 - 4.0 * t
                        }
                    }
                    Wave::Saw => 2.0 * t - 1.0,
                    Wave::Square => {
                        if t < 0.5 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    Wave::Pulse => {
                        if t < 0.25 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    Wave::Noise => {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        state as f32 / u32::MAX as f32 * 2.0 - 1.0
                    }
                }
            })
            .collect();
        let top = if format == Format::Mod {
            127.0 / 128.0
        } else {
            32767.0 / 32768.0
        };
        wav::normalize(&mut data, top);
        wav::quantize_to(&mut data, if format == Format::Mod { 8 } else { 16 });
        let mut meta = sample_meta(&self.module.samples[index]);
        if meta.name.is_empty() {
            meta.name = wave.name().to_string();
        }
        if meta.volume == 0 {
            meta.volume = 64;
        }
        if format == Format::S3m && meta.c2spd == 0 {
            meta.c2spd = 8363;
        }
        meta.loop_kind = LoopKind::Forward;
        meta.loop_start = 0;
        meta.loop_len = length;
        let ((), changes) = self.edit(|d| d.put_sample(index, data, meta))?;
        let j = self.sample_json(r.instrument, r.sample, &self.module.samples[index]);
        Ok((j, changes))
    }

    // ----- the instrument list -----------------------------------------------------------------

    /// Cells that name instrument `number` (1-based) in any pattern.
    fn instrument_uses(&self, number: usize) -> Option<(usize, usize, usize)> {
        for (p, pattern) in self.module.patterns.iter().enumerate() {
            for row in 0..pattern.rows() {
                for (ch, c) in pattern.row(row).iter().enumerate() {
                    if c.instrument as usize == number {
                        return Some((p, row, ch));
                    }
                }
            }
        }
        None
    }

    /// Renumbers the instrument column after instrument `removed` (1-based)
    /// went away: cells that used it lose it, later ones move down.
    fn renumber_cells_after_removal(&mut self, removed: usize) {
        for p in 0..self.module.patterns.len() {
            let pattern = &self.module.patterns[p];
            let mut new: Option<Pattern> = None;
            for row in 0..pattern.rows() {
                for ch in 0..pattern.channels() {
                    let c = pattern.cell(row, ch);
                    let n = c.instrument as usize;
                    if n >= removed {
                        let target = new.get_or_insert_with(|| pattern.clone());
                        target.cell_mut(row, ch).instrument =
                            if n == removed { 0 } else { (n - 1) as u8 };
                    }
                }
            }
            if let Some(pattern) = new {
                self.change(Change::Pattern { index: p, pattern });
            }
        }
    }

    /// Appends an instrument: an XM gets one with an empty sample, an S3M an
    /// empty sample slot (volume 64, c2spd 8363). A MOD's 31 slots are fixed.
    pub fn add_instrument(&mut self, name: Option<&str>) -> Res<(usize, Changes)> {
        let lim = limits(&self.module);
        let format = self.module.format;
        if format == Format::Mod {
            return Err(
                "a MOD has 31 sample slots (15 in the oldest layout); they are always there".into(),
            );
        }
        let count = if format == Format::Xm {
            self.module.instruments.len()
        } else {
            self.module.samples.len()
        };
        if count >= lim.max_slots {
            return Err(format!(
                "{} holds {} {}",
                format_name(format),
                lim.max_slots,
                if format == Format::Xm {
                    "instruments"
                } else {
                    "samples"
                }
            ));
        }
        let name = match name {
            Some(n) => clean_name(n, lim.max_name)?.0,
            None => String::new(),
        };
        let ((), changes) = self.edit(|d| {
            if format == Format::Xm {
                let index = d.module.instruments.len();
                let inst = Instrument {
                    name,
                    ..Instrument::default()
                };
                d.change(Change::InstrumentInsert { index, meta: inst });
                d.change(Change::XmSampleInsert {
                    owner: index,
                    slot: 0,
                    sample: new_xm_sample(),
                });
            } else {
                let index = d.module.samples.len();
                d.change(Change::SampleInsert {
                    index,
                    sample: Sample {
                        name,
                        volume: 64,
                        c2spd: 8363,
                        ..Sample::default()
                    },
                });
            }
            Ok(())
        })?;
        Ok((count + 1, changes))
    }

    /// Removes an instrument. Cells that play it lose the instrument and later
    /// instruments are renumbered; that is refused while a cell uses it
    /// unless `force`. In an S3M only the last slot can go (the others would
    /// renumber); a MOD's slots are fixed.
    pub fn remove_instrument(&mut self, number: usize, force: bool) -> Res<Changes> {
        let format = self.module.format;
        match format {
            Format::Mod => {
                return Err("a MOD's sample slots are fixed: clear the sample instead".into())
            }
            Format::S3m => {
                if number != self.module.samples.len() {
                    return Err(
                        "only the last sample slot of an S3M can be removed: clear this one instead"
                            .into(),
                    );
                }
                if number == 0 {
                    return Err("sample 0 does not exist".into());
                }
            }
            Format::Xm => {
                self.instrument_index(number)?;
            }
        }
        if !force {
            if let Some((p, row, ch)) = self.instrument_uses(number) {
                return Err(format!(
                    "instrument {number} is played at pattern {p}, row {row}, channel {} (pass force to remove it anyway)",
                    ch + 1
                ));
            }
        }
        let ((), changes) = self.edit(|d| {
            if format == Format::Xm {
                let index = number - 1;
                while !d.module.instruments[index].samples.is_empty() {
                    d.change(Change::XmSampleRemove {
                        owner: index,
                        slot: d.module.instruments[index].samples.len() - 1,
                    });
                }
                d.change(Change::InstrumentRemove { index });
            } else {
                d.change(Change::SampleRemove { index: number - 1 });
            }
            d.renumber_cells_after_removal(number);
            Ok(())
        })?;
        Ok(changes)
    }

    /// XM: adds an empty sample to an instrument (at most 16). Returns its
    /// number within the instrument.
    pub fn add_sample(&mut self, instrument: usize) -> Res<(usize, Changes)> {
        let owner = self.instrument_index(instrument)?;
        let slot = self.module.instruments[owner].samples.len();
        if slot >= limits(&self.module).max_samples_per_instrument {
            return Err(format!(
                "an XM instrument holds {} samples",
                limits(&self.module).max_samples_per_instrument
            ));
        }
        let ((), changes) = self.edit(|d| {
            d.change(Change::XmSampleInsert {
                owner,
                slot,
                sample: new_xm_sample(),
            });
            Ok(())
        })?;
        Ok((slot, changes))
    }

    /// XM: removes a sample from an instrument. Keys that played it play the
    /// first sample, later samples move down a number. An instrument that
    /// loses its last sample is reset to a plain empty one (an XM keeps
    /// envelopes and keymaps only with a sample).
    pub fn remove_sample(&mut self, instrument: usize, sample: usize) -> Res<Changes> {
        let owner = self.instrument_index(instrument)?;
        let inst = &self.module.instruments[owner];
        if sample >= inst.samples.len() {
            return Err(format!(
                "instrument {instrument} has {} samples; sample {sample} does not exist",
                inst.samples.len()
            ));
        }
        let mut meta = instrument_meta(inst);
        if inst.samples.len() == 1 {
            meta = Instrument {
                name: meta.name,
                ..Instrument::default()
            };
        } else {
            for k in &mut meta.keymap {
                let n = *k as usize;
                *k = if n == sample {
                    0
                } else if n > sample {
                    (n - 1) as u8
                } else {
                    *k
                };
            }
        }
        let ((), changes) = self.edit(|d| {
            d.change(Change::InstrumentMeta { index: owner, meta });
            d.change(Change::XmSampleRemove {
                owner,
                slot: sample,
            });
            Ok(())
        })?;
        Ok(changes)
    }

    // ----- XM instrument fields --------------------------------------------------------------------

    /// XM: changes an instrument's name, fadeout and auto-vibrato.
    pub fn set_instrument(&mut self, instrument: usize, edit: &InstrumentEdit) -> Res<Changes> {
        let index = self.instrument_index(instrument)?;
        let mut meta = instrument_meta(&self.module.instruments[index]);
        if let Some(n) = &edit.name {
            meta.name = clean_name(n, limits(&self.module).max_name)?.0;
        }
        if let Some(f) = edit.fadeout {
            if f > 4095 {
                return Err(format!("the fadeout is 0-4095, not {f}"));
            }
            meta.fadeout = f;
        }
        let touches_vibrato = edit.vibrato_kind.is_some()
            || edit.vibrato_sweep.is_some()
            || edit.vibrato_depth.is_some()
            || edit.vibrato_rate.is_some();
        if touches_vibrato || edit.fadeout.is_some() {
            self.need_sample_for_extras(index, "fadeout and vibrato")?;
        }
        let mut v: AutoVibrato = meta.vibrato;
        if let Some(k) = edit.vibrato_kind {
            if k > 3 {
                return Err(format!(
                    "the vibrato type is 0 sine, 1 square, 2 ramp down, 3 ramp up, not {k}"
                ));
            }
            v.kind = k;
        }
        if let Some(s) = edit.vibrato_sweep {
            v.sweep = s;
        }
        if let Some(d) = edit.vibrato_depth {
            if d > 15 {
                return Err(format!("the vibrato depth is 0-15, not {d}"));
            }
            v.depth = d;
        }
        if let Some(r) = edit.vibrato_rate {
            if r > 63 {
                return Err(format!("the vibrato rate is 0-63, not {r}"));
            }
            v.rate = r;
        }
        meta.vibrato = v;
        let ((), changes) = self.edit(|d| {
            d.change(Change::InstrumentMeta { index, meta });
            Ok(())
        })?;
        Ok(changes)
    }

    fn need_sample_for_extras(&self, index: usize, what: &str) -> Res<()> {
        if self.module.instruments[index].samples.is_empty() {
            return Err(format!(
                "an XM keeps {what} only for an instrument with a sample: add a sample first"
            ));
        }
        Ok(())
    }

    /// XM: edits an envelope. Points are (tick, value): at most 12, the first
    /// at tick 0, ticks rising, ticks up to 324, values 0-64; the sustain
    /// point and the loop's ends must be points.
    pub fn set_envelope(
        &mut self,
        instrument: usize,
        kind: EnvelopeKind,
        edit: &EnvelopeEdit,
    ) -> Res<Changes> {
        let index = self.instrument_index(instrument)?;
        self.need_sample_for_extras(index, "envelopes")?;
        let mut meta = instrument_meta(&self.module.instruments[index]);
        let slot = match kind {
            EnvelopeKind::Volume => &mut meta.volume_envelope,
            EnvelopeKind::Panning => &mut meta.panning_envelope,
        };
        let mut env = match (slot.clone(), edit.enabled) {
            (Some(e), _) => e,
            (None, Some(true)) | (None, None) if edit.points.is_some() => Envelope {
                points: vec![(0, 0)],
                sustain: None,
                looped: None,
            },
            (None, Some(true)) => Envelope {
                points: match kind {
                    EnvelopeKind::Volume => vec![(0, 64), (20, 64)],
                    EnvelopeKind::Panning => vec![(0, 32), (20, 32)],
                },
                sustain: None,
                looped: None,
            },
            (None, _) => {
                if edit.points.is_some() || edit.sustain.is_some() || edit.looped.is_some() {
                    return Err("the envelope is off: set enabled to turn it on".into());
                }
                return Ok(Changes::default());
            }
        };
        if edit.enabled == Some(false) {
            *slot = None;
        } else {
            if let Some(points) = &edit.points {
                env.points = points.clone();
            }
            if let Some(s) = edit.sustain {
                env.sustain = s;
            }
            if let Some(l) = edit.looped {
                env.looped = l;
            }
            check_envelope(&env)?;
            *slot = Some(env);
        }
        let ((), changes) = self.edit(|d| {
            d.change(Change::InstrumentMeta { index, meta });
            Ok(())
        })?;
        Ok(changes)
    }

    /// XM: points the keys `from..=to` (MIDI notes, C-0 is 12) at sample
    /// `sample` of the instrument.
    pub fn set_keymap(
        &mut self,
        instrument: usize,
        from: u8,
        to: u8,
        sample: usize,
    ) -> Res<Changes> {
        let index = self.instrument_index(instrument)?;
        self.need_sample_for_extras(index, "a keymap")?;
        let inst = &self.module.instruments[index];
        if sample >= inst.samples.len() {
            return Err(format!(
                "instrument {instrument} has {} samples; sample {sample} does not exist",
                inst.samples.len()
            ));
        }
        let (lo, hi) = note_range(&self.module);
        let (a, b) = (from.min(to), from.max(to));
        if a < lo || b > hi {
            return Err(format!(
                "keys run {lo} to {hi} (C-0 to B-7), not {a} to {b}"
            ));
        }
        let mut meta = instrument_meta(inst);
        for n in a..=b {
            meta.keymap[(n - lo) as usize] = sample as u8;
        }
        let ((), changes) = self.edit(|d| {
            d.change(Change::InstrumentMeta { index, meta });
            Ok(())
        })?;
        Ok(changes)
    }

    /// XM: sets the whole keymap (96 sample numbers, the first for C-0).
    pub fn set_keymap_all(&mut self, instrument: usize, keymap: [u8; 96]) -> Res<Changes> {
        let index = self.instrument_index(instrument)?;
        self.need_sample_for_extras(index, "a keymap")?;
        let inst = &self.module.instruments[index];
        if let Some(&bad) = keymap.iter().find(|&&k| k as usize >= inst.samples.len()) {
            return Err(format!(
                "the keymap names sample {bad}; instrument {instrument} has {}",
                inst.samples.len()
            ));
        }
        let mut meta = instrument_meta(inst);
        meta.keymap = keymap;
        let ((), changes) = self.edit(|d| {
            d.change(Change::InstrumentMeta { index, meta });
            Ok(())
        })?;
        Ok(changes)
    }
}

/// A new XM sample: empty, full volume, centred.
fn new_xm_sample() -> Sample {
    Sample {
        volume: 64,
        pan: Some(128),
        ..Sample::default()
    }
}

fn check_envelope(e: &Envelope) -> Res<()> {
    if e.points.is_empty() {
        return Err("an envelope needs at least one point".into());
    }
    if e.points.len() > 12 {
        return Err(format!(
            "an envelope has at most 12 points, not {}",
            e.points.len()
        ));
    }
    if e.points[0].0 != 0 {
        return Err("the first envelope point is at tick 0".into());
    }
    for w in e.points.windows(2) {
        if w[1].0 <= w[0].0 {
            return Err(format!(
                "envelope ticks must rise ({} then {})",
                w[0].0, w[1].0
            ));
        }
    }
    for &(t, v) in &e.points {
        if t > 324 {
            return Err(format!("an envelope runs to tick 324, not {t}"));
        }
        if v > 64 {
            return Err(format!("an envelope value is 0-64, not {v}"));
        }
    }
    if let Some(s) = e.sustain {
        if s >= e.points.len() {
            return Err(format!(
                "sustain point {s} does not exist ({} points)",
                e.points.len()
            ));
        }
    }
    if let Some((a, b)) = e.looped {
        if a > b || b >= e.points.len() {
            return Err(format!(
                "the loop {a}..{b} is not inside the {} points",
                e.points.len()
            ));
        }
    }
    Ok(())
}
