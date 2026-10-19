#!/usr/bin/env python3
"""Writes the synthetic GoatTracker songs `tests/goattracker.rs` checks.

Each song is built to reach branches of the import (GoatTracker 2, `GTS5`) or
of the GoatTracker 1 conversion (`GTS!`), after the cases of the another_synth
DAW's tests (src/tests/sid-sng-import.test.ts, sid-sng-gt1-fix.test.ts). The
expected results are not written here: `oracle.ts` runs the DAW's own import
over these files and writes `expected.json`.

    python3 make_fixtures.py            # writes the .sng files beside this script
"""

import os

HERE = os.path.dirname(os.path.abspath(__file__))


def text(s, n=32):
    b = s.encode("latin-1")
    assert len(b) <= n
    return b + bytes(n - len(b))


def header(magic, name="", author="", copyright=""):
    out = bytearray(magic) + text(name) + text(author) + text(copyright)
    assert len(out) == 100
    return out


def orderlist(data, restart):
    """A channel's orderlist: `data` without the endmark, then $FF, then the restart byte."""
    body = bytes(data) + b"\xff"
    return bytes([len(body)]) + body + bytes([restart])


def gts5(subtunes, instruments, tables, patterns, magic=b"GTS5", **texts):
    """subtunes: [[(data, restart)] x 3]; instruments: dicts; tables: 4 lists of (left, right);
    patterns: lists of (note, instrument, command, param) rows (the end row is added)."""
    out = header(magic, **texts)
    out.append(len(subtunes))
    for lists in subtunes:
        for data, restart in lists:
            out += orderlist(data, restart)
    out.append(len(instruments))
    for ins in instruments:
        out += bytes([
            ins.get("ad", 0), ins.get("sr", 0),
            ins.get("wave", 0), ins.get("pulse", 0), ins.get("filter", 0), ins.get("speed", 0),
            ins.get("vibdelay", 0), ins.get("gate", 0), ins.get("firstwave", 0),
        ]) + text(ins.get("name", ""), 16)
    for table in tables:
        out.append(len(table))
        out += bytes(l for l, _ in table) + bytes(r for _, r in table)
    out.append(len(patterns))
    for rows in patterns:
        out.append(len(rows) + 1)
        for row in rows:
            out += bytes(row)
        out += b"\xff\x00\x00\x00"
    return bytes(out)


def gt1(subtunes, instruments, patterns, filter_table=None, **texts):
    """instruments: up to 31 dicts (the rest empty) with h (8 bytes, h[7] set from the wave),
    name and wave pairs; patterns: lists of (note, instrument, command, param) rows."""
    out = header(b"GTS!", **texts)
    out.append(len(subtunes))
    for lists in subtunes:
        for data, restart in lists:
            out += orderlist(data, restart)
    for i in range(31):
        ins = instruments[i] if i < len(instruments) else {}
        wave = ins.get("wave", [])
        h = list(ins.get("h", [0] * 7))[:7] + [len(wave) * 2]
        out += bytes(h) + text(ins.get("name", ""), 16)
        for l, r in wave:
            out += bytes([l, r])
    out.append(len(patterns))
    for rows in patterns:
        body = bytearray()
        for note, ins, cmd, param in rows:
            body += bytes([note, (ins << 3) | cmd, param])
        body += b"\xff\x00\x00"
        out.append(len(body))
        out += body
    if filter_table is not None:
        flat = bytearray(256)
        for c, row in filter_table.items():
            flat[c * 4:c * 4 + 4] = bytes(row)
        out += flat
    return bytes(out)


def one_song(channel_lists):
    return [[(data, restart) for data, restart in channel_lists]]


THREE = one_song([([0], 0), ([0], 0), ([0], 0)])
# A blank wave program: one row and its end.
WAVE_END = [(0x41, 0x00), (0xFF, 0x00)]

songs = {}

# ------------------------------------------------------------------ GTS5

songs["gts5_basics.sng"] = gts5(
    subtunes=[
        [
            # Transposes hold, repeats apply to the next pattern; restart in a command run.
            ([0xF3, 0x00, 0xD3, 0x01, 0xE5, 0xDF, 0x00, 0xF0, 0x01], 2),
            ([0xE0, 0x01, 0xFE, 0xD0, 0x00], 0),
            ([0x02], 0),
        ],
        [([0x00], 0), ([0x01], 0), ([0xF1, 0x02, 0x02], 1)],
    ],
    instruments=[
        {"ad": 0x09, "sr": 0xA4, "wave": 1, "pulse": 1, "filter": 0, "speed": 0,
         "vibdelay": 3, "gate": 0x02, "firstwave": 0x09, "name": "lead"},
        {"ad": 0x00, "sr": 0xF0, "wave": 3, "pulse": 0, "filter": 1, "speed": 1,
         "vibdelay": 0, "gate": 0x80 | 0x40 | 0x3F, "firstwave": 0xFF, "name": "sixteen chars!!!"},
    ],
    tables=[
        [(0x41, 0x00), (0xFF, 0x00), (0x81, 0x30), (0x11, 0x00), (0xFF, 0x03)],
        [(0x88, 0x00), (0x20, 0x10), (0xFF, 0x02)],
        [(0x90, 0xF1), (0x00, 0x40), (0xFF, 0x00)],
        [(0x01, 0x20)],
    ],
    patterns=[
        [(0x60, 1, 0x0, 0x00), (0x61, 0, 0x1, 0x01), (0xBC, 2, 0xF, 0x06), (0xBD, 0, 0x0, 0x00)],
        [(0xBE, 0, 0x0, 0x00), (0xBF, 0, 0xA, 0x01), (0x70, 1, 0x4, 0x01)],
        [(0x6C, 2, 0xD, 0x0F)],
    ],
    name="Basics\x00ignored", author="cubed-synth", copyright="CC0",
)

songs["gts5_padded_table.sng"] = gts5(
    subtunes=THREE,
    instruments=[{"wave": 1, "pulse": 6, "speed": 2, "name": "points past"}],
    tables=[[(0x41, 0x00), (0xFF, 0x00)], [(0x88, 0x00), (0xFF, 0x00), (0x88, 0x00)], [], []],
    patterns=[[(0x60, 1, 0, 0)]],
)

# The loop restarts at the pattern byte after a transpose, with the end-of-list transpose still running.
songs["gts5_loop_transpose.sng"] = gts5(
    subtunes=one_song([([0xF2, 0x00, 0xF5, 0x01], 1), ([0x00, 0xD1, 0x01], 1), ([0x00], 0)]),
    instruments=[],
    tables=[[], [], [], []],
    patterns=[[(0x60, 0, 0, 0)], [(0x62, 0, 0, 0)]],
)

songs["x_6581_3x.sng"] = gts5(THREE, [], [[], [], [], []], [[(0x60, 0, 0, 0)]])
songs["dispatch_by_magic.xm"] = gts5(THREE, [], [[], [], [], []], [[(0x60, 0, 0, 0)]])

# Refusals: the reason must be the DAW's.
base = dict(subtunes=THREE, instruments=[{"wave": 1}], tables=[WAVE_END, [], [], []],
            patterns=[[(0x60, 1, 0, 0)]])
good = gts5(**base)
songs["refuse_not_gt.sng"] = b"XXXX" + good[4:]
songs["refuse_beta_gts3.sng"] = b"GTS3" + good[4:]
songs["refuse_truncated_header.sng"] = good[:60]
songs["refuse_truncated.sng"] = good[:-3]
songs["refuse_trailing_bytes.sng"] = good + b"\x00\x00"
songs["refuse_dual_sid.sng"] = gts5(**{**base, "subtunes": [[([0], 0)] * 6]})
songs["refuse_no_subtunes.sng"] = good[:100] + b"\x00" + good[101:]
songs["refuse_bad_instrument.sng"] = gts5(**{**base, "patterns": [[(0x60, 2, 0, 0)]]})
songs["refuse_bad_command.sng"] = gts5(**{**base, "patterns": [[(0x60, 1, 0x10, 0)]]})
songs["refuse_bad_note.sng"] = gts5(**{**base, "patterns": [[(0x50, 1, 0, 0)]]})
songs["refuse_long_pattern.sng"] = gts5(**{**base, "patterns": [[(0x60, 0, 0, 0)] * 129]})
songs["refuse_pattern_missing.sng"] = gts5(**{**base, "subtunes": one_song([([0], 0), ([3], 0), ([0], 0)])})
songs["refuse_ends_with_command.sng"] = gts5(**{**base, "subtunes": one_song([([0, 0xF2], 0), ([0], 0), ([0], 0)])})
songs["refuse_no_pattern.sng"] = gts5(**{**base, "subtunes": one_song([([0xF2], 0), ([0], 0), ([0], 0)])})
songs["refuse_restart_past_end.sng"] = gts5(**{**base, "subtunes": one_song([([0, 0], 2), ([0], 0), ([0], 0)])})
# A pattern without its end row: hand-cut so its length counts no $FF row.
no_end = bytearray(good)
assert no_end[-8:-4] == bytes([0x60, 1, 0, 0])
no_end[-9] = 2
no_end[-4:] = bytes([0x60, 1, 0, 0])
songs["refuse_no_pattern_end.sng"] = bytes(no_end)

# ------------------------------------------------------------------ GTS!

FILTER_FOR_COMMANDS = {
    0: (0x00, 0x00, 0x09, 0x03),  # bytes 2-3: the funktempo
    1: (0xF1, 0x1F, 0x40, 0x02),
}

songs["gt1_commands.sng"] = gt1(
    subtunes=one_song([([0x00], 0), ([0x01], 0), ([0xF2, 0x00], 0)]),
    instruments=[
        # Lefts $08-$0F are old delays; ends with a jump.
        {"h": [0x09, 0xA9, 0, 0, 0, 0, 0], "name": "delays",
         "wave": [(0x41, 0x00), (0x08, 0x00), (0x0F, 0x0C), (0xFF, 0x00)]},
        # No end jump: one is added.
        {"h": [0x0A, 0xF0, 0, 0, 0, 0, 1], "name": "open", "wave": [(0x81, 0xC0), (0x41, 0x00)]},
        # A jump past its own program: it stops instead.
        {"h": [0x00, 0xF0, 0, 0, 0, 0, 0], "name": "far", "wave": [(0x41, 0x00), (0xFF, 0x09)]},
    ],
    patterns=[
        [
            (0x00, 1, 0, 0), (0x5C, 0, 1, 0x10), (0x5D, 0, 2, 0x00), (0x5E, 0, 3, 0x40),
            (0x5F, 0, 4, 0x34), (0x60, 0, 5, 0x01), (0x9E, 0, 6, 0x8A), (0x9F, 0, 7, 0x10),
            (0xA0, 0, 7, 0xF5), (0xFE, 0, 7, 0x00), (0x20, 2, 3, 0x00), (0x21, 3, 1, 0x10),
        ],
        [(0x30, 2, 4, 0x34), (0x31, 0, 5, 0x00), (0x32, 0, 2, 0x80)],
    ],
    filter_table=FILTER_FOR_COMMANDS,
    name="GT1 commands",
)

songs["gt1_arpeggio.sng"] = gt1(
    subtunes=THREE,
    instruments=[
        {"h": [0x09, 0xA9, 0, 0, 0, 0, 0], "name": "short",
         "wave": [(0x41, 0x00), (0x21, 0x00), (0xFF, 0x00)]},
        {"h": [0x00, 0xF0, 0, 0, 0, 0, 0], "name": "thirteen chars", "wave": WAVE_END},
        # Not played by any pattern but not empty: the first clone replaces it.
        {"h": [0x01, 0x01, 0, 0, 0, 0, 0], "name": "unused", "wave": WAVE_END},
    ],
    patterns=[[
        (0x10, 0, 0, 0x37),              # before any instrument: dropped
        (0x10, 1, 0, 0x37),              # clone of 1
        (0x12, 0, 0, 0x37),              # same (instrument, param): the same clone
        (0x5F, 0, 0, 0x47),              # on a rest: not converted
        (0x14, 2, 0, 0xC4),              # bit 7: a 1-frame delay per step; long name kept
        (0x16, 1, 0, 0x04),
    ]],
)

songs["gt1_pulse.sng"] = gt1(
    subtunes=THREE,
    instruments=[
        # start, speed, low, high (top 8 bits each); bit 0 of start is "no hard restart".
        {"h": [0, 0xF0, 0x81, 0x00, 0x00, 0x00, 0], "name": "no hr hold", "wave": WAVE_END},
        {"h": [0, 0xF0, 0x00, 0x20, 0x10, 0xF0, 0], "name": "zero start", "wave": WAVE_END},
        {"h": [0, 0xF0, 0x80, 0x21, 0x40, 0xC0, 0], "name": "odd speed", "wave": WAVE_END},
        {"h": [0, 0xF0, 0x20, 0x20, 0x10, 0xC0, 0], "name": "to high", "wave": WAVE_END},
        {"h": [0, 0xF0, 0x08, 0x02, 0x01, 0xFF, 0], "name": "long legs", "wave": WAVE_END},
        {"h": [0, 0xF0, 0x80, 0xF0, 0x01, 0xFF, 0], "name": "clamped", "wave": WAVE_END},
        {"h": [0, 0xF0, 0x80, 0x01, 0x40, 0xC0, 0], "name": "speed 1", "wave": WAVE_END},
        {"h": [0, 0xF0, 0xC0, 0x10, 0x40, 0x80, 0], "name": "above high", "wave": WAVE_END},
    ],
    patterns=[[(0x10 + i, i + 1, 0, 0) for i in range(8)]],
)

songs["gt1_filter.sng"] = gt1(
    subtunes=THREE,
    instruments=[
        {"h": [0, 0xF0, 0, 0, 0, 0, 1], "name": "set", "wave": WAVE_END},
        {"h": [0, 0xF0, 0, 0, 0, 0, 4], "name": "sweep", "wave": WAVE_END},
        {"h": [0, 0xF0, 0, 0, 0, 0, 0x50], "name": "past", "wave": WAVE_END},
    ],
    patterns=[[(0x10, 1, 0, 0), (0x11, 2, 5, 0x06), (0x12, 3, 5, 0x00), (0x13, 0, 5, 0x70)]],
    filter_table={
        0: (0, 0, 0, 0),
        1: (0xF1, 0x1F, 0x40, 0x02),   # set, passband low, cutoff; falls through
        2: (0x00, 0x90, 0x02, 0x05),   # 144 frames at +2: two rows; jumps to 5
        3: (0, 0, 0, 0),               # all zero: maps to what is laid next
        4: (0x72, 0xA5, 0x00, 0x09),   # master volume 5 and voice 3 off are dropped
        5: (0x00, 0x00, 0x10, 0x01),   # 0 frames: its jump alone, back to 1
        6: (0x10, 0x20, 0x30, 0x48),   # next past the table: stops
        9: (0x00, 0x10, 0xFE, 0x0A),   # falls through to the blank last row
    },
)

songs["gt1_no_filter_table.sng"] = gt1(
    subtunes=THREE,
    instruments=[{"h": [0, 0xF0, 0, 0, 0, 0, 3], "name": "lost filter", "wave": WAVE_END}],
    patterns=[[(0x10, 1, 5, 0x00), (0x11, 0, 5, 0x02), (0x12, 0, 7, 0x00)]],
)

# Arpeggios on instrument 1 (no wave of its own) until the 63 instrument
# slots run out (command 8) and then the wave table (dropped).
songs["gt1_slots_full.sng"] = gt1(
    subtunes=THREE,
    instruments=[{"h": [0, 0xF0, 0, 0, 0, 0, 0], "name": "arp",
                  "wave": [(0x00, 0x00), (0xFF, 0x00)]}],
    patterns=[[(0x10, 1 if k == 0 else 0, 0, 0x01 + k) for k in range(66)]],
)

gt1_good = gt1(THREE, [{"h": [0, 0xF0, 0, 0, 0, 0, 0], "wave": WAVE_END}], [[(0x10, 1, 0, 0)]])
songs["refuse_gt1_trailing_bytes.sng"] = gt1_good + b"\x00" * 7
bad_len = bytearray(gt1_good)
# The pattern's length byte: 2 rows of 3 bytes; make it 5 (and drop a byte so the file still ends there).
at = len(bad_len) - 7
assert bad_len[at] == 6
bad_len[at] = 5
songs["refuse_gt1_pattern_length.sng"] = bytes(bad_len[:-1])
songs["refuse_gt1_end_note.sng"] = gt1(THREE, [{"h": [0, 0xF0, 0, 0, 0, 0, 0], "wave": WAVE_END}],
                                       [[(0xFF, 1, 0, 0), (0x10, 1, 0, 0)]])

for name, data in songs.items():
    with open(os.path.join(HERE, name), "wb") as f:
        f.write(data)
print(f"wrote {len(songs)} songs")
