//! Renders a module to raw interleaved stereo f32 at 48 kHz, optionally one
//! channel alone, without the Amiga filter or the format's output trim (as
//! the DAW's sink renders), for comparison with its Web Audio path.
//! `PLAYER_DEFAULTS=1` renders as the mixer plays it instead (filter, trim).
//!
//! cargo run -p cubed-tracker --release --example tracker_render -- song.mod seconds solo out.f32

use std::io::Write;

use cubed_tracker::{Format, Player, PlayerOptions};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, seconds, solo, out] = &args[..] else {
        panic!("usage: tracker_render <song> <seconds> <solo channel or -1> <out.f32>");
    };
    let seconds: f64 = seconds.parse().unwrap();
    let solo: i64 = solo.parse().unwrap();
    let module = cubed_tracker::load(&std::fs::read(path).unwrap()).unwrap();
    let trim_db: f32 = match module.format {
        Format::Mod => -4.0 - 10.0 * (module.channels.max(4) as f32 / 4.0).log10(),
        _ => -6.0,
    };
    let channels = module.channels;
    let separation = std::env::var("SEPARATION").ok().map(|s| s.parse().unwrap());
    let options = PlayerOptions {
        looped: false,
        gain: 10f32.powf(-trim_db / 20.0),
        filter: false,
        stereo_separation: separation,
        daw_quirks: std::env::var_os("DAW_QUIRKS").is_some(),
    };
    let options = if std::env::var_os("PLAYER_DEFAULTS").is_some() {
        PlayerOptions {
            looped: false,
            ..Default::default()
        }
    } else {
        options
    };
    let mut player = Player::new(module, 48000.0, options);
    if solo >= 0 {
        for c in 0..channels {
            player.set_channel_muted(c, c as i64 != solo);
        }
    }
    let frames = (seconds * 48000.0).ceil() as usize;
    let (mut l, mut r) = (vec![0.0f32; 128], vec![0.0f32; 128]);
    let mut file = std::io::BufWriter::new(std::fs::File::create(out).unwrap());
    let mut done = 0;
    while done < frames {
        let k = (frames - done).min(128);
        player.render(&mut l[..k], &mut r[..k]);
        for i in 0..k {
            file.write_all(&l[i].to_le_bytes()).unwrap();
            file.write_all(&r[i].to_le_bytes()).unwrap();
        }
        done += k;
    }
}
