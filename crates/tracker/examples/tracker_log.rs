//! Logs what the player does, one JSON object per line, in the schema of
//! the DAW-side reference harness, so the two logs can be compared. It
//! plays as the DAW does (`daw_quirks`); `FAITHFUL=1` plays as the original
//! trackers do.
//!
//! cargo run -p cubed-tracker --example tracker_log -- song.mod [seconds]

use std::io::Write;

use cubed_tracker::{Command, Observer, PitchSource, Player, PlayerOptions, Ramp};

struct Log(Vec<String>);

fn r(x: f64) -> f64 {
    (x * 1e9).round() / 1e9
}

impl Observer for Log {
    fn row(&mut self, time: f64, order: usize, row: usize) {
        self.0.push(format!(
            r#"{{"t":{},"k":"row","order":{order},"row":{row}}}"#,
            r(time)
        ));
    }

    fn command(&mut self, time: f64, ch: usize, instrument: u8, command: &Command) {
        let t = r(time);
        let inst = format!("{instrument:02}");
        self.0.push(match *command {
            Command::NoteOn { midi, velocity, frequency, pan, offset } => {
                let mut s = format!(
                    r#"{{"t":{t},"k":"noteOn","ch":{ch},"inst":"{inst}","midi":{midi},"vel":{velocity},"freq":{}"#,
                    r(frequency)
                );
                if let Some(p) = pan {
                    s += &format!(r#","pan":{p}"#);
                }
                if let Some(o) = offset {
                    s += &format!(r#","off":{o}"#);
                }
                s + "}"
            }
            Command::NoteOff => format!(r#"{{"t":{t},"k":"noteOff","ch":{ch},"inst":"{inst}"}}"#),
            Command::Pitch { frequency, source } => {
                let src = match source {
                    Some(PitchSource::Table) => r#","src":"table""#,
                    Some(PitchSource::Target) => r#","src":"target""#,
                    None => "",
                };
                format!(r#"{{"t":{t},"k":"pitch","ch":{ch},"inst":"{inst}","freq":{}{src}}}"#, r(frequency))
            }
            Command::Volume { volume, ramp } => {
                let ramp = match ramp {
                    Ramp::Step => r#","ramp":"step""#,
                    Ramp::Linear => r#","ramp":"linear""#,
                    Ramp::Default => "",
                };
                format!(r#"{{"t":{t},"k":"vol","ch":{ch},"inst":"{inst}","vol":{}{ramp}}}"#, r(volume))
            }
            Command::Pan { pan } => format!(
                r#"{{"t":{t},"k":"pan","ch":{ch},"inst":"{inst}","pan":{}}}"#,
                r(((pan + 1.0) / 2.0).clamp(0.0, 1.0))
            ),
            Command::EnvelopePosition { tick } => {
                format!(r#"{{"t":{t},"k":"envpos","ch":{ch},"inst":"{inst}","tick":{tick}}}"#)
            }
            Command::Retrigger { midi, velocity, frequency } => format!(
                r#"{{"t":{t},"k":"retrig","ch":{ch},"inst":"{inst}","midi":{midi},"vel":{velocity},"freq":{}}}"#,
                r(frequency)
            ),
        });
    }

    fn global_volume(&mut self, time: f64, volume: f64) {
        self.0.push(format!(
            r#"{{"t":{},"k":"global","vol":{}}}"#,
            r(time),
            r(volume)
        ));
    }

    fn all_notes_off(&mut self, time: f64) {
        self.0.push(format!(r#"{{"t":{},"k":"alloff"}}"#, r(time)));
    }

    fn filter(&mut self, time: f64, on: bool) {
        self.0
            .push(format!(r#"{{"t":{},"k":"filter","on":{on}}}"#, r(time)));
    }

    fn adlib_write(&mut self, time: f64, register: u16, value: u8) {
        self.0.push(format!(
            r#"{{"t":{},"k":"opl","reg":{register},"val":{value}}}"#,
            r(time)
        ));
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: tracker_log <song> [seconds]");
    let seconds: f64 = args.next().map_or(600.0, |s| s.parse().expect("seconds"));
    let bytes = std::fs::read(&path).expect("read song");
    let module = cubed_tracker::load(&bytes).expect("parse song");
    let daw_quirks = std::env::var_os("FAITHFUL").is_none();
    let mut player = Player::new(
        module,
        48000.0,
        PlayerOptions {
            looped: false,
            daw_quirks,
            ..Default::default()
        },
    );
    let mut log = Log(Vec::new());
    let (mut l, mut r) = ([0.0f32; 512], [0.0f32; 512]);
    while player.time() < seconds && player.render_observed(&mut l, &mut r, &mut log) {}
    log.0.push(format!(
        r#"{{"t":{},"k":"end","ended":{}}}"#,
        (player.time() * 1e9).round() / 1e9,
        player.finished()
    ));
    let mut out = std::io::stdout().lock();
    for line in log.0 {
        writeln!(out, "{line}").unwrap();
    }
}
