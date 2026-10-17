"""Writes demo.s3m: a short Scream Tracker 3 module made of synthetic
samples and one AdLib instrument (no third-party audio), exercising most of
ST3's commands.

python3 make_demo_s3m.py   (writes demo.s3m beside this file)
"""
import math
import os
import random
import struct

NOTES = {n: i for i, n in enumerate(["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"])}
KEY_OFF = 254


def note(name):
    """'C-4' style names; ST3 stores octave << 4 | semitone."""
    pitch, octave = name[:-1].rstrip("-"), int(name[-1])
    return (octave << 4) | NOTES[pitch]


def letter(c):
    return ord(c) - 0x40


rng = random.Random(2)


def pcm8(values):
    """Unsigned 8-bit, as ST3's newer files store samples."""
    return bytes(max(0, min(255, int(round(v * 127)) + 128)) for v in values)


samples = [
    # name, data, volume, loop start, loop end (0 = none), c2spd
    ("pulse", pcm8([0.7 if i < 24 else -0.7 for i in range(64)]), 40, 0, 64, 8363),
    ("tri", pcm8([1 - abs(i - 32) / 16 for i in range(64)]), 48, 0, 64, 16726),
    ("hat", pcm8([rng.uniform(-1, 1) * math.exp(-i / 120) for i in range(1500)]), 32, 0, 0, 22050),
    ("kick", pcm8([math.sin(2 * math.pi * 50 * (1 - math.exp(-i / 300)) * 6) * math.exp(-i / 700) for i in range(2400)]), 56, 0, 0, 8363),
]
# An FM organ: modulator/carrier characteristics, levels, attack/decay,
# sustain/release, waves, then feedback/connection.
ADLIB = [0x21, 0x21, 0x1A, 0x00, 0xF2, 0xF3, 0x34, 0x45, 0x01, 0x00, 0x06, 0x00]

CHANNELS = [0x00, 0x08, 0x01, 0x09, 0x10]  # L1 R1 L2 R2 A1 (AdLib melody 1)
ROWS = 64


def empty():
    return [{} for _ in range(ROWS)]


def put(pattern, row, ch, n=None, inst=0, vol=None, cmd=None, info=0):
    cell = {}
    if n is not None:
        cell["note"] = n
        cell["inst"] = inst
    if vol is not None:
        cell["vol"] = vol
    if cmd is not None:
        cell["cmd"] = (letter(cmd), info)
    pattern[row][ch] = cell


p0 = empty()
put(p0, 0, 0, note("C-4"), 1, cmd="A", info=5)
put(p0, 0, 4, note("C-4"), 5)
for row in range(0, 64, 8):
    put(p0, row, 0, note(["C-4", "A-3", "F-3", "G-3"][row // 16 % 4]), 1, cmd="J", info=0x37)
    put(p0, row + 2, 0, cmd="J", info=0x00)  # J00 repeats the 37
    put(p0, row + 4, 0, cmd="D", info=0x04)
    put(p0, row + 6, 0, cmd="J", info=0x00)  # and now plays J04
for row in range(0, 64, 4):
    put(p0, row, 2, note("C-5"), 4)
    put(p0, row + 2, 2, note("C-6"), 3, vol=24, cmd="S", info=0xC2)
for row in range(0, 64, 16):
    put(p0, row, 1, note("E-4"), 2, cmd="H", info=0x46)
    put(p0, row + 4, 1, cmd="H", info=0x00)
    put(p0, row + 8, 1, cmd="R", info=0x48)
    put(p0, row + 12, 1, cmd="D", info=0x0F)  # D0F slides every tick
put(p0, 16, 4, note("E-4"), 5, cmd="H", info=0x24)
put(p0, 24, 4, KEY_OFF)
put(p0, 32, 4, note("G-4"), 5, vol=40)
put(p0, 40, 4, note("C-5"), 5, cmd="G", info=0x10)
put(p0, 48, 4, cmd="G", info=0x00)
put(p0, 56, 4, KEY_OFF, cmd="D", info=0x00)

p1 = empty()
put(p1, 0, 0, note("C-5"), 2, cmd="F", info=0x08)
put(p1, 1, 0, cmd="F", info=0x00)
put(p1, 2, 0, cmd="E", info=0xF4)  # a fine slide down
put(p1, 3, 0, cmd="E", info=0xE2)  # an extra-fine one
put(p1, 0, 1, cmd="S", info=0xB0)
put(p1, 3, 1, cmd="S", info=0xB2)  # loop rows 0-3 twice more
for row in range(4, 60, 8):
    put(p1, row, 2, note("D-5"), 4, cmd="S", info=0xD2)
    put(p1, row + 4, 2, note("C-6"), 3, cmd="X", info=row * 4)
put(p1, 8, 3, note("G-4"), 1, cmd="L", info=0x02)
put(p1, 9, 3, note("C-4"), 0, cmd="G", info=0x20)
put(p1, 12, 3, cmd="K", info=0x20)
put(p1, 16, 3, note("A-4"), 2, cmd="D", info=0x3F)  # fine slide up
put(p1, 17, 3, cmd="D", info=0xF2)  # fine slide down
put(p1, 18, 3, cmd="Q", info=0x83)
put(p1, 20, 3, cmd="I", info=0x21)
put(p1, 24, 3, note("B-4"), 2, cmd="O", info=0x08)
put(p1, 28, 3, cmd="U", info=0x48)
put(p1, 32, 2, cmd="T", info=0x96)
put(p1, 36, 0, cmd="V", info=0x30)
put(p1, 44, 0, cmd="V", info=0x40)
put(p1, 48, 1, note("D-4"), 2, cmd="S", info=0x83)
put(p1, 56, 3, cmd="S", info=0xE1)  # pattern delay
put(p1, 0, 4, note("A-3"), 5, cmd="H", info=0x33)
put(p1, 32, 4, note("D-4"), 5, cmd="F", info=0x04)

p2 = empty()
for row in range(0, 32, 2):
    put(p2, row, 0, note(["C-4", "D#4", "G-4", "A#4"][row // 2 % 4]), 1, cmd="D", info=0x01)
    put(p2, row, 2, note("C-5"), 4)
put(p2, 30, 3, cmd="B", info=0x03)
put(p2, 30, 1, cmd="C", info=0x16)
put(p2, 0, 4, note("C-4"), 5, vol=20)

patterns = [p0, p1, p2]
orders = [0, 1, 2, 1]


def pack(pattern):
    out = bytearray()
    for row in pattern:
        for ch, cell in sorted(row.items()):
            flag = ch
            body = bytearray()
            if "note" in cell:
                flag |= 0x20
                body += bytes([cell["note"], cell["inst"]])
            if "vol" in cell:
                flag |= 0x40
                body.append(cell["vol"])
            if "cmd" in cell:
                flag |= 0x80
                body += bytes(cell["cmd"])
            out.append(flag)
            out += body
        out.append(0)
    return struct.pack("<H", len(out) + 2) + out


def align(buf):
    while len(buf) % 16:
        buf.append(0)


ins_count = len(samples) + 1
order_bytes = orders + [255] * (len(orders) % 2)
header = bytearray(b"cubed-tracker demo".ljust(28, b"\0"))
header += bytes([0x1A, 0x10, 0, 0])
header += struct.pack("<HHHHHH", len(order_bytes), ins_count, len(patterns), 0, 0x1320, 2)
header += b"SCRM"
header += bytes([64, 6, 125, 0xB0, 0, 0xFC]) + bytes(8) + struct.pack("<H", 0)
header += bytes(CHANNELS + [0xFF] * (32 - len(CHANNELS)))
header += bytes(order_bytes)
pointers_at = len(header)
header += bytes(2 * (ins_count + len(patterns)))
header += bytes([0x20 | p for p in (3, 12, 4, 11, 7)] + [0] * 27)
out = bytearray(header)
align(out)

instrument_at = []
for name, data, volume, loop_start, loop_end, c2spd in samples:
    instrument_at.append(len(out))
    out += bytes(80)
align(out)
adlib_at = len(out)
out += bytes(80)
align(out)
pattern_at = []
for p in patterns:
    pattern_at.append(len(out))
    out += pack(p)
    align(out)
data_at = []
for s in samples:
    data_at.append(len(out))
    out += s[1]
    align(out)

for i, (name, data, volume, loop_start, loop_end, c2spd) in enumerate(samples):
    at = instrument_at[i]
    seg = data_at[i] >> 4
    rec = bytearray([1]) + name.encode()[:12].ljust(12, b"\0")
    rec += bytes([(seg >> 16) & 0xFF, seg & 0xFF, (seg >> 8) & 0xFF])
    rec += struct.pack("<IIIBBBBI", len(data), loop_start, loop_end, volume, 0, 0, 1 if loop_end else 0, c2spd)
    rec += bytes(12) + name.encode().ljust(28, b"\0") + b"SCRS"
    out[at:at + 80] = rec
rec = bytearray([2]) + b"organ".ljust(12, b"\0") + bytes(3) + bytes(ADLIB)
rec += bytes([60, 0, 0, 0]) + struct.pack("<I", 8363) + bytes(12) + b"organ".ljust(28, b"\0") + b"SCRI"
out[adlib_at:adlib_at + 80] = rec

for i, at in enumerate(instrument_at + [adlib_at] + pattern_at):
    struct.pack_into("<H", out, pointers_at + 2 * i, at >> 4)

path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "demo.s3m")
open(path, "wb").write(out)
print(path, len(out), "bytes")
