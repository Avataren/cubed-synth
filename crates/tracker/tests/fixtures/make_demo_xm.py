"""Writes demo.xm: a short FastTracker 2 module made of synthetic samples (no
third-party audio), exercising instruments (keymaps, envelopes, fadeout,
auto-vibrato), the volume column and FT2's commands.

python3 make_demo_xm.py   (writes demo.xm beside this file)
"""
import math
import os
import random
import struct

NOTES = {n: i for i, n in enumerate(["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"])}
KEY_OFF = 97
CHANNELS = 6
rng = random.Random(3)


def note(name):
    """'C-4' style names; XM numbers C-0 as 1."""
    pitch, octave = name[:-1].rstrip("-"), int(name[-1])
    return 1 + octave * 12 + NOTES[pitch]


def delta8(values):
    out, last = bytearray(), 0
    for v in values:
        s = max(-128, min(127, int(round(v * 127))))
        out.append((s - last) & 0xFF)
        last = s
    return bytes(out)


def delta16(values):
    out, last = bytearray(), 0
    for v in values:
        s = max(-32768, min(32767, int(round(v * 32767))))
        out += struct.pack("<H", (s - last) & 0xFFFF)
        last = s
    return bytes(out)


def sample(name, data, bits=8, volume=48, finetune=0, pan=128, relative=0, loop=0, loop_start=0, loop_len=0):
    return dict(name=name, data=data, bits=bits, volume=volume, finetune=finetune, pan=pan,
                relative=relative, loop=loop, loop_start=loop_start, loop_len=loop_len)


def env(points, sustain=None, loop=None):
    return dict(points=points, sustain=sustain, loop=loop)


saw = [1 - 2 * i / 63 for i in range(64)]
square = [0.6 if i < 32 else -0.6 for i in range(64)]
sine16 = [math.sin(2 * math.pi * i / 128) for i in range(128)]
instruments = [
    # A pad: volume envelope with a sustain and a loop, fadeout, auto-vibrato.
    dict(name="pad", samples=[sample("saw", delta8(saw), volume=40, loop=1, loop_len=64)],
         keymap=[0] * 96,
         volenv=env([(0, 0), (6, 64), (20, 40), (40, 52), (60, 40)], sustain=2, loop=(2, 4)),
         panenv=env([(0, 32), (30, 10), (60, 54)], loop=(0, 2)),
         fadeout=1200, vib=(0, 8, 6, 12)),
    # A two-sample keymap: square up to B-3, a 16-bit sine above, ping-pong.
    dict(name="keys", samples=[sample("sq", delta8(square), volume=36, pan=64, loop=1, loop_len=64),
                               sample("sine", delta16(sine16), bits=16, volume=44, pan=192, relative=12,
                                      finetune=-20, loop=2, loop_start=16, loop_len=96)],
         keymap=[0] * 48 + [1] * 48, volenv=None, panenv=None, fadeout=0, vib=(0, 0, 0, 0)),
    # A kick and a hat, one-shot, the hat with a decay envelope and no sustain.
    dict(name="kick", samples=[sample("kick", delta8([math.sin(2 * math.pi * 55 * (1 - math.exp(-i / 300)) * 6) * math.exp(-i / 800) for i in range(2600)]), volume=56)],
         keymap=[0] * 96, volenv=None, panenv=None, fadeout=0, vib=(0, 0, 0, 0)),
    dict(name="hat", samples=[sample("hat", delta8([rng.uniform(-1, 1) for _ in range(1200)]), volume=30, pan=170)],
         keymap=[0] * 96, volenv=env([(0, 64), (4, 20), (10, 0)]), panenv=None, fadeout=0, vib=(0, 0, 0, 0)),
]

ROWS = [64, 48, 32]


def empty(rows):
    return [{} for _ in range(rows)]


def put(pattern, row, ch, n=0, inst=0, vol=0, fx=0, param=0):
    pattern[row][ch] = (n, inst, vol, fx, param)


p0 = empty(ROWS[0])
for row in range(0, 64, 16):
    put(p0, row, 0, note(["C-4", "A-3", "F-3", "G-3"][row // 16]), 1)
    put(p0, row + 12, 0, KEY_OFF)
put(p0, 0, 1, note("C-4"), 2, vol=0x30, fx=0x0, param=0x37)
put(p0, 8, 1, note("E-5"), 2, vol=0xC4)  # set pan 4
put(p0, 16, 1, note("G-3"), 2, vol=0xF2, fx=0x0, param=0)  # volume-column tone porta
put(p0, 17, 1, 0, 0, vol=0xF0)
put(p0, 24, 1, 0, 0, vol=0x63)  # volume slide down
put(p0, 28, 1, 0, 0, vol=0xB4)  # vibrato depth
put(p0, 32, 1, note("C-5"), 2, fx=0x19, param=0x30)  # pan slide
put(p0, 40, 1, 0, 2)  # instrument alone: back to its volume and pan
put(p0, 48, 1, note("D-5"), 0, fx=0x21, param=0x14)  # extra-fine porta up
for row in range(0, 64, 4):
    put(p0, row, 2, note("C-4"), 3)
    put(p0, row + 2, 3, note("C-6"), 4, vol=0x20 + row % 16, fx=0x1B if row % 16 == 4 else 0, param=0x82 if row % 16 == 4 else 0)
put(p0, 20, 4, note("A-4"), 1, fx=0x1D, param=0x21)  # tremor
put(p0, 28, 4, KEY_OFF, fx=0x14, param=0x00)
put(p0, 32, 4, note("E-4"), 1, fx=0x15, param=0x10)  # envelope position
put(p0, 36, 4, 0, 0, fx=0x14, param=0x03)  # key off on tick 3
put(p0, 0, 5, 0, 0, fx=0x10, param=0x30)  # global volume 48
put(p0, 8, 5, 0, 0, fx=0x11, param=0x40)  # global volume slide up
put(p0, 9, 5, 0, 0, fx=0x11, param=0x00)

p1 = empty(ROWS[1])
put(p1, 0, 0, note("C-3"), 2, fx=0x1, param=0x08)
put(p1, 1, 0, 0, 0, fx=0x1, param=0x00)
put(p1, 2, 0, 0, 0, fx=0xE, param=0x2F)
put(p1, 3, 0, 0, 0, fx=0xE, param=0x63)  # loop rows 0-3 three times more
put(p1, 0, 1, 0, 0, fx=0xE, param=0x60)
for row in range(4, 44, 8):
    put(p1, row, 2, note("D-4"), 3, fx=0xE, param=0xD3)
    put(p1, row + 4, 3, note("C-6"), 4, fx=0x8, param=row * 5)
put(p1, 8, 4, note("G-4"), 1, fx=0x5, param=0x02)
put(p1, 9, 4, note("C-4"), 0, fx=0x3, param=0x20)
put(p1, 12, 4, 0, 0, fx=0x6, param=0x20)
put(p1, 16, 4, note("A-4"), 2, fx=0xE, param=0x1F)
put(p1, 17, 4, 0, 0, fx=0xE, param=0xA2)
put(p1, 18, 4, 0, 0, fx=0x7, param=0x48)
put(p1, 24, 4, note("B-4"), 2, fx=0x9, param=0x02)
put(p1, 32, 2, 0, 0, fx=0xF, param=0x96)
put(p1, 40, 4, 0, 0, fx=0xE, param=0xE1)
put(p1, 44, 5, 0, 0, fx=0x11, param=0x08)

p2 = empty(ROWS[2])
for row in range(0, 32, 2):
    put(p2, row, 0, note(["C-4", "D#4", "G-4", "A#4"][row // 2 % 4]), 2, fx=0xA, param=0x01)
    put(p2, row, 2, note("C-4"), 3)
put(p2, 30, 3, 0, 0, fx=0xB, param=0x03)
put(p2, 30, 1, 0, 0, fx=0xD, param=0x10)

patterns = [p0, p1, p2]
orders = [0, 1, 2, 1]


def pack(pattern):
    out = bytearray()
    for row in pattern:
        for ch in range(CHANNELS):
            n, inst, vol, fx, param = row.get(ch, (0, 0, 0, 0, 0))
            fields = [n, inst, vol, fx, param]
            flag = 0x80 | sum(1 << i for i, v in enumerate(fields) if v)
            out.append(flag)
            out += bytes(v for v in fields if v)
    return bytes(out)


out = bytearray(b"Extended Module: " + b"cubed-tracker demo".ljust(20, b"\0") + b"\x1a" + b"cubed-tracker".ljust(20, b" ") + struct.pack("<H", 0x0104))
out += struct.pack("<IHHHHHHHH", 276, len(orders), 0, CHANNELS, len(patterns), len(instruments), 1, 6, 125)
out += bytes(orders + [0] * (256 - len(orders)))
for p, rows in zip(patterns, ROWS):
    data = pack(p)
    out += struct.pack("<IBHH", 9, 0, rows, len(data)) + data


def env_bytes(e):
    if not e:
        return bytes(48), 0, 0, 0, 0, 0
    pts = b"".join(struct.pack("<HH", t, v) for t, v in e["points"]) + bytes(48 - 4 * len(e["points"]))
    flags = 1 | (2 if e["sustain"] is not None else 0) | (4 if e["loop"] else 0)
    loop = e["loop"] or (0, 0)
    return pts, len(e["points"]), e["sustain"] or 0, loop[0], loop[1], flags


for ins in instruments:
    vol_pts, vn, vs, vls, vle, vf = env_bytes(ins["volenv"])
    pan_pts, pn, ps, pls, ple, pf = env_bytes(ins["panenv"])
    head = ins["name"].encode().ljust(22, b"\0") + bytes([0]) + struct.pack("<H", len(ins["samples"]))
    head += struct.pack("<I", 40) + bytes(ins["keymap"]) + vol_pts + pan_pts
    head += bytes([vn, pn, vs, vls, vle, ps, pls, ple, vf, pf]) + bytes(ins["vib"]) + struct.pack("<H", ins["fadeout"]) + bytes(22)
    out += struct.pack("<I", 4 + len(head)) + head
    for s in ins["samples"]:
        width = 2 if s["bits"] == 16 else 1
        kind = s["loop"] | (0x10 if s["bits"] == 16 else 0)
        out += struct.pack("<IIIBbBBbB", len(s["data"]), s["loop_start"] * width, s["loop_len"] * width,
                           s["volume"], s["finetune"], kind, s["pan"], s["relative"], 0)
        out += s["name"].encode().ljust(22, b"\0")
    for s in ins["samples"]:
        out += s["data"]

path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "demo.xm")
open(path, "wb").write(out)
print(path, len(out), "bytes")
