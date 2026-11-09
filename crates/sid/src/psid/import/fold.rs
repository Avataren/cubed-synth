//! Instruments folded into row commands: the accents of a sound (a louder
//! attack, another start of the pulse sweep, a drum on another wave program)
//! as a command on the notes of one instrument, not an instrument each. A port
//! of `transcribe/fold.ts`.
//!
//! GoatTracker runs a row's tick-0 command after its note's instrument init
//! (player.s `mt_tick0jump1`, player.rs `tick0`), so a note of instrument B
//! plays exactly as one of A with `5xx` (attack/decay), `6xx` (sustain/
//! release), `8xx` (wave pointer), `9xx` (pulse pointer) or `Axx` (filter
//! pointer) when A and B differ in that one thing. B folds into A when every
//! row naming B has its command column free; instruments that differ in
//! nothing (a wave program cut to fit the tables can make two so) merge.

use super::song::SubsongRows;
use crate::song::{Instrument, Row};
use std::collections::{HashMap, HashSet};

struct Field {
    command: u8,
    /// B's value as the command's parameter, or `None` when no command sets it (a zero pointer: GoatTracker keeps the running one).
    param: fn(&Instrument) -> Option<u8>,
    /// B's instrument with A's value here: equal to A when this is all they differ in.
    as_host: fn(&Instrument, &Instrument) -> Instrument,
}

const FIELDS: [Field; 5] = [
    Field {
        command: 0x5,
        param: |i| Some((i.attack << 4) | i.decay),
        as_host: |i, h| Instrument {
            attack: h.attack,
            decay: h.decay,
            ..i.clone()
        },
    },
    Field {
        command: 0x6,
        param: |i| Some((i.sustain << 4) | i.release),
        as_host: |i, h| Instrument {
            sustain: h.sustain,
            release: h.release,
            ..i.clone()
        },
    },
    Field {
        command: 0x8,
        param: |i| (i.wave_ptr != 0).then_some(i.wave_ptr),
        as_host: |i, h| Instrument {
            wave_ptr: h.wave_ptr,
            ..i.clone()
        },
    },
    Field {
        command: 0x9,
        param: |i| (i.pulse_ptr != 0).then_some(i.pulse_ptr),
        as_host: |i, h| Instrument {
            pulse_ptr: h.pulse_ptr,
            ..i.clone()
        },
    },
    Field {
        command: 0xa,
        param: |i| (i.filter_ptr != 0).then_some(i.filter_ptr),
        as_host: |i, h| Instrument {
            filter_ptr: h.filter_ptr,
            ..i.clone()
        },
    },
];

/// What makes an instrument sound as it does (its name aside).
fn sound_eq(a: &Instrument, b: &Instrument) -> bool {
    a.attack == b.attack
        && a.decay == b.decay
        && a.sustain == b.sustain
        && a.release == b.release
        && a.first_wave == b.first_wave
        && a.gate_timer == b.gate_timer
        && a.hard_restart == b.hard_restart
        && a.no_gate_off == b.no_gate_off
        && a.vibrato_delay == b.vibrato_delay
        && a.wave_ptr == b.wave_ptr
        && a.pulse_ptr == b.pulse_ptr
        && a.filter_ptr == b.filter_ptr
        && a.speed_ptr == b.speed_ptr
}

enum How {
    Same,
    Command(u8, u8),
}

/// How `ins` plays as `host`: nothing to add, one command, or `None` (it differs in more).
fn as_host(ins: &Instrument, host: &Instrument) -> Option<How> {
    if sound_eq(ins, host) {
        return Some(How::Same);
    }
    for f in &FIELDS {
        if let Some(param) = (f.param)(ins) {
            if sound_eq(&(f.as_host)(ins, host), host) {
                return Some(How::Command(f.command, param));
            }
        }
    }
    None
}

/// `instruments` (1-based in `songs`' rows) with every one that plays as
/// another plus a command folded into it, the most used first as hosts; the
/// rows renumbered.
pub fn fold_instruments(
    instruments: &[Instrument],
    songs: Vec<SubsongRows>,
) -> (Vec<Instrument>, Vec<SubsongRows>) {
    let mut uses = vec![0usize; instruments.len() + 1];
    // Instruments with a note whose command column is taken (a pitch effect, a tempo): they fold only when nothing needs adding.
    let mut busy: HashSet<usize> = HashSet::new();
    for s in &songs {
        for v in &s.voices {
            for r in v {
                if r.instrument == 0 {
                    continue;
                }
                uses[r.instrument as usize] += 1;
                if r.command != 0 {
                    busy.insert(r.instrument as usize);
                }
            }
        }
    }
    let mut order: Vec<usize> = (1..=instruments.len()).collect();
    order.sort_by(|&a, &b| uses[b].cmp(&uses[a]).then(a.cmp(&b)));
    let mut hosts: Vec<usize> = Vec::new();
    // Per instrument: its host and the command its notes take.
    let mut fold: HashMap<usize, (usize, u8, u8)> = HashMap::new();
    for n in order {
        let ins = &instruments[n - 1];
        let mut found: Option<(usize, u8, u8)> = None;
        for &h in &hosts {
            match as_host(ins, &instruments[h - 1]) {
                Some(How::Same) => {
                    found = Some((h, 0, 0));
                    break;
                }
                Some(How::Command(c, p)) if !busy.contains(&n) && found.is_none() => {
                    found = Some((h, c, p));
                }
                Some(How::Command(..)) => {}
                None => {}
            }
        }
        match found {
            Some(f) => {
                fold.insert(n, f);
            }
            None => hosts.push(n),
        }
    }
    if fold.is_empty() {
        return (instruments.to_vec(), songs);
    }
    hosts.sort_unstable();
    let number: HashMap<usize, usize> =
        hosts.iter().enumerate().map(|(i, &h)| (h, i + 1)).collect();
    let row = |r: &Row| -> Row {
        if r.instrument == 0 {
            return *r;
        }
        match fold.get(&(r.instrument as usize)) {
            None => Row {
                instrument: number[&(r.instrument as usize)] as u8,
                ..*r
            },
            Some(&(host, 0, _)) => Row {
                instrument: number[&host] as u8,
                ..*r
            },
            Some(&(host, command, param)) => Row {
                instrument: number[&host] as u8,
                command,
                param,
                ..*r
            },
        }
    };
    let out_instruments = hosts.iter().map(|&h| instruments[h - 1].clone()).collect();
    let out_songs = songs
        .into_iter()
        .map(|s| SubsongRows {
            voices: s
                .voices
                .iter()
                .map(|v| v.iter().map(&row).collect())
                .collect(),
            ..s
        })
        .collect();
    (out_instruments, out_songs)
}
