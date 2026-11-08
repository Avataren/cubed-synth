//! The worklet's module-song exports on the render callback's thread: a module
//! opened stopped and followed, steered by decoded commands, and cut for the
//! page (`cw_songs_prepare`) every few quanta, allocate and free nothing. A binary of
//! its own, since the exports share one global mixer.

use cubed_mixer::{
    wire, BusId, BusSettings, Command, PlaySource, SongControl, SongLoop, SourceEvent, SourceId,
    VoiceId,
};
use cubed_rt_check::{allocations_during, trace_allocations, CountingAllocator};
use cubed_worklet as w;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const RATE: f32 = 48_000.0;

fn put(bytes: &[u8], at: *mut u8) {
    // SAFETY: the export just made room for the bytes.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len()) }
}

fn encode(command: Command) -> Vec<u8> {
    let mut bytes = Vec::new();
    assert!(wire::encode(&command, &mut bytes));
    bytes
}

#[test]
fn followed_songs_render_and_report_without_allocating() {
    trace_allocations(std::env::var_os("CUBED_RT_TRACE").is_some());
    w::cw_init(RATE, 0, 0, 0, 1, 2);
    let send = |command: Command| {
        let bytes = encode(command);
        put(&bytes, w::cw_inbox(bytes.len() as u32));
        assert_eq!(w::cw_apply(bytes.len() as u32), 1);
    };
    send(Command::SetBus(BusId(1), BusSettings::default()));
    let names = ["demo.mod", "demo.s3m", "demo.xm"];
    for (slot, name) in names.iter().enumerate() {
        let module = std::fs::read(format!(
            "{}/../tracker/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        put(&module, w::cw_file_buffer(module.len() as u32));
        assert_eq!(
            w::cw_prepare_module(slot as u32, 3 + slot as u32, 0, 1, -1.0, 1),
            1
        );
        send(Command::PlaySource(PlaySource::new(
            VoiceId(10 + slot as u64),
            SourceId(slot as u32),
            BusId(1),
        )));
    }
    let song = |voice: u64, control: SongControl| {
        encode(Command::SourceEvent {
            voice: VoiceId(voice),
            event: SourceEvent::Song(control),
        })
    };
    // What the page sends, one batch every 200 quanta; the encoding is the page's.
    let mut script: Vec<Vec<u8>> = Vec::new();
    for voice in 10..13 {
        script.push(
            [
                song(voice, SongControl::Scopes(true)),
                song(voice, SongControl::Seek { order: 1, row: 4 }),
                song(voice, SongControl::Play),
            ]
            .concat(),
        );
        script.push(
            [
                song(
                    voice,
                    SongControl::Loop(SongLoop::Range {
                        order: 1,
                        row0: 4,
                        row1: 11,
                    }),
                ),
                song(voice, SongControl::SoloMask(0b101)),
                song(
                    voice,
                    SongControl::PreviewOn {
                        instrument: 2,
                        note: 60,
                        velocity: 0.9,
                        duration: 6_000,
                    },
                ),
            ]
            .concat(),
        );
        script.push(
            [
                song(voice, SongControl::Stop),
                song(voice, SongControl::Seek { order: 0, row: 0 }),
                song(voice, SongControl::SoloMask(0)),
                song(
                    voice,
                    SongControl::PreviewOn {
                        instrument: 5,
                        note: 64,
                        velocity: 1.0,
                        duration: 0,
                    },
                ),
            ]
            .concat(),
        );
        script.push(
            [
                song(voice, SongControl::PreviewOff),
                song(voice, SongControl::Loop(SongLoop::Pattern { order: 2 })),
                song(voice, SongControl::Play),
                song(voice, SongControl::Scopes(false)),
            ]
            .concat(),
        );
    }
    let mut reported = 0;
    let mut scoped = 0;
    for q in 0..(script.len() * 200) {
        let batch = script.get(q / 200).filter(|_| q % 200 == 0);
        if let Some(bytes) = batch {
            put(bytes, w::cw_inbox(bytes.len() as u32));
        }
        let len = batch.map_or(0, |b| b.len() as u32);
        let events = allocations_during(|| {
            if len > 0 {
                w::cw_apply(len);
            }
            w::cw_process(128);
            if q % 6 == 0 {
                let count = w::cw_songs_prepare() as usize;
                reported += count;
                // The page reads what was cut.
                // SAFETY: the buffers hold what `cw_songs_prepare` just cut.
                let status = unsafe {
                    std::slice::from_raw_parts(
                        w::cw_song_status(),
                        count * w::cw_song_status_words() as usize,
                    )
                };
                let levels = unsafe {
                    std::slice::from_raw_parts(
                        w::cw_song_levels(),
                        count * w::cw_song_level_slots() as usize,
                    )
                };
                let _ = (status.iter().sum::<u32>(), levels.iter().sum::<f32>());
                scoped += w::cw_song_scope_count() as usize;
            }
        });
        assert_eq!(events, 0, "quantum {q}: {events} allocations or frees");
    }
    assert!(reported > 100, "songs were reported: {reported}");
    assert!(scoped > 20, "scopes were cut: {scoped}");
    assert_eq!(w::cw_rejected(), 0);
    assert_eq!(w::cw_bad_batches(), 0);
}
