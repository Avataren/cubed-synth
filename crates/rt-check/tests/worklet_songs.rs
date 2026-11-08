//! The worklet's module-song exports on the render callback's thread: a module
//! opened stopped and followed, steered by decoded commands, and cut for the
//! page (`cw_songs_prepare`) every few quanta, allocate and free nothing. A binary of
//! its own, since the exports share one global mixer.

use cubed_mixer::{
    wire, BusId, BusSettings, Command, PlaySource, SongCell, SongControl, SongInstrument, SongLoop,
    SourceEvent, SourceId, VoiceId,
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
    // The modules (formats 3, 4, 5) and an Adlib song (format 1).
    let names = [
        ("tracker/tests/fixtures/demo.mod", 3),
        ("tracker/tests/fixtures/demo.s3m", 4),
        ("tracker/tests/fixtures/demo.xm", 5),
        ("opl/tests/fixtures/a2m/NAB622/corridors of time.a2m", 1),
        ("ahx/tests/fixtures/songs/a_new_beginning.ahx", 0),
    ];
    for (slot, (name, format)) in names.iter().enumerate() {
        let module = std::fs::read(format!("{}/../{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        put(&module, w::cw_file_buffer(module.len() as u32));
        assert_eq!(
            w::cw_prepare_module(slot as u32, *format, 0, 1, -1.0, 1),
            1,
            "{name}"
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
    for voice in 10..15 {
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
    // What only an Adlib song takes: live edits of an instrument, a cell's second effect and
    // the timing, as the page sends them (wire tags 32, 31 and 30).
    script.push(
        [
            song(
                13,
                SongControl::SetInstrument(SongInstrument {
                    number: 1,
                    fm: [
                        0x21, 0x31, 0x4f, 0x0b, 0xf2, 0xf1, 0x72, 0x7a, 0x03, 0x01, 0x0e,
                    ],
                    ..Default::default()
                }),
            ),
            song(
                13,
                SongControl::SetCell(SongCell {
                    pattern: 0,
                    row: 6,
                    channel: 1,
                    note: 40,
                    instrument: 1,
                    effect: 0x0c,
                    param: 0x20,
                    period: 0x0a03,
                    ..Default::default()
                }),
            ),
            song(
                13,
                SongControl::Timing {
                    tempo: 60,
                    speed: 5,
                },
            ),
            song(13, SongControl::Scopes(true)),
        ]
        .concat(),
    );
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
