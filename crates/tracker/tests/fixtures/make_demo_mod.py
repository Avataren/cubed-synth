"""Writes demo.mod: a short 4-channel ProTracker module made of synthetic
samples (no third-party audio), exercising most of ProTracker's effects.

python3 make_demo_mod.py   (writes demo.mod beside this file)
"""
import math
import os
import random
import struct

PERIODS = [856, 808, 762, 720, 678, 640, 604, 570, 538, 508, 480, 453,
           428, 404, 381, 360, 339, 320, 302, 285, 269, 254, 240, 226,
           214, 202, 190, 180, 170, 160, 151, 143, 135, 127, 120, 113]
NOTES = {n: i for i, n in enumerate(["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"])}


def period(name):
    """'C-2' style note names, octaves 1-3 as ProTracker shows them."""
    pitch, octave = name[:-1].rstrip("-"), int(name[-1])
    return PERIODS[(octave - 1) * 12 + NOTES[pitch]]


def pcm(values):
    return bytes(max(-128, min(127, int(round(v * 127)))) & 0xFF for v in values)


rng = random.Random(1)
samples = [
    # name, data, finetune, volume, loop start, loop length (bytes)
    ("square", pcm([0.8 if i < 16 else -0.8 for i in range(32)]), 0, 32, 0, 32),
    ("saw", pcm([1 - 2 * i / 63 for i in range(64)]), 0, 32, 0, 64),
    ("sine", pcm([math.sin(2 * math.pi * i / 64) for i in range(64)]), 3, 40, 0, 64),
    ("noise", pcm([rng.uniform(-1, 1) * math.exp(-i / 500) for i in range(2000)]), 0, 40, 0, 0),
    ("bass", pcm([math.sin(2 * math.pi * i / 32) * (0.4 + 0.6 * math.exp(-i / 1500)) for i in range(4000)]), -2, 48, 2000, 2000),
    ("kick", pcm([math.sin(2 * math.pi * 60 * (1 - math.exp(-i / 400)) * 8) * math.exp(-i / 900) for i in range(3000)]), 0, 48, 0, 0),
]

ROWS, CHANNELS = 64, 4


def empty():
    return [[(0, 0, 0, 0) for _ in range(CHANNELS)] for _ in range(ROWS)]


def put(pattern, row, ch, note=None, sample=0, effect=0, param=0):
    pattern[row][ch] = (period(note) if note else 0, sample, effect, param)


p0 = empty()
for row in range(0, 64, 8):  # chords and leads
    put(p0, row, 0, ["C-2", "A-1", "F-1", "G-1"][(row // 16) % 4], 1, 0x0, 0x37)
    put(p0, row + 4, 0, None, 0, 0x0, 0x47)
for row in range(0, 64, 16):  # a bass sliding between notes
    put(p0, row, 1, ["C-1", "A-1"][(row // 16) % 2], 5)
    put(p0, row + 8, 1, ["G-1", "E-1"][(row // 16) % 2], 0, 0x3, 0x08)
    for r in range(row + 9, row + 12):
        put(p0, r, 1, None, 0, 0x3, 0x00)
for row in range(0, 64, 4):  # drums: kicks, retriggered and cut noise
    put(p0, row, 2, "C-2", 6)
    put(p0, row + 2, 2, "C-3", 4, 0xE, 0x93 if row % 16 == 12 else 0xC2)
for row in range(0, 64, 16):  # vibrato, tremolo and volume slides
    put(p0, row, 3, "E-2", 3, 0x4, 0x46)
    put(p0, row + 4, 3, None, 0, 0x4, 0x00)
    put(p0, row + 8, 3, None, 0, 0x7, 0x48)
    put(p0, row + 12, 3, None, 0, 0xA, 0x04)
put(p0, 0, 3, "E-2", 3, 0xF, 0x05)  # speed 5

p1 = empty()
put(p1, 0, 0, "C-3", 2, 0x1, 0x02)
put(p1, 1, 0, None, 0, 0x1, 0x00)
put(p1, 2, 0, None, 0, 0x2, 0x04)
put(p1, 3, 0, None, 0, 0xE, 0x62)  # loop rows 0-3 twice more
put(p1, 0, 1, None, 0, 0xE, 0x60)
for row in range(4, 64, 4):
    put(p1, row, 1, "A-1", 5, 0x9, 0x04 + row // 4)  # sample offsets
for row in range(4, 60, 8):
    put(p1, row, 2, "D-2", 6, 0xE, 0xD2)  # delayed kicks
    put(p1, row + 4, 2, "C-3", 4, 0x8, 0x20 + row * 3)  # panned noise
put(p1, 8, 3, "G-2", 3, 0x5, 0x00)
put(p1, 9, 3, "C-2", 0, 0x3, 0x04)
put(p1, 10, 3, None, 0, 0x5, 0x02)
put(p1, 12, 3, None, 0, 0x6, 0x20)
put(p1, 16, 3, "A-2", 3, 0xE, 0x13)
put(p1, 17, 3, None, 0, 0xE, 0x23)
put(p1, 18, 3, None, 0, 0xE, 0xA4)
put(p1, 19, 3, None, 0, 0xE, 0xB8)
put(p1, 20, 3, "B-2", 3, 0xC, 0x20)
put(p1, 24, 0, "E-2", 1, 0xE, 0x00)  # LED filter on
put(p1, 40, 0, "E-2", 1, 0xE, 0x01)  # and off
put(p1, 32, 2, None, 0, 0xF, 0x96)  # tempo 150
put(p1, 48, 3, "D-2", 2, 0xE, 0x53)  # finetune
put(p1, 56, 3, None, 0, 0xE, 0xE1)  # pattern delay

p2 = empty()
for row in range(0, 32, 2):
    put(p2, row, 0, ["C-2", "D#2", "G-2", "A#2"][row // 2 % 4], 2, 0xA, 0x01)
    put(p2, row, 1, "C-1", 5, 0x0, 0x0C)
put(p2, 16, 2, "C-2", 6, 0xF, 0x04)
put(p2, 24, 3, "C-2", 3, 0x4, 0x8F)
put(p2, 30, 3, None, 0, 0xB, 0x03)  # jump to order 3, skipping pattern 2's tail
put(p2, 30, 2, None, 0, 0xD, 0x16)  # (Bxx wins over Dxx)

orders = [0, 1, 2, 1]
title = b"cubed-tracker demo"
out = bytearray(title.ljust(20, b"\0"))
for i in range(31):
    if i < len(samples):
        name, data, finetune, volume, loop_start, loop_len = samples[i]
        out += name.encode().ljust(22, b"\0")
        out += struct.pack(">HBBHH", len(data) // 2, finetune & 0x0F, volume,
                           loop_start // 2, max(loop_len, 2) // 2)
    else:
        out += b"\0" * 22 + struct.pack(">HBBHH", 0, 0, 64, 0, 1)
out += bytes([len(orders), 127]) + bytes(orders + [0] * (128 - len(orders))) + b"M.K."
for pattern in (p0, p1, p2):
    for row in pattern:
        for per, smp, fx, prm in row:
            out += bytes([(smp & 0xF0) | (per >> 8), per & 0xFF, ((smp & 0x0F) << 4) | fx, prm])
for s in samples:
    out += s[1]
path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "demo.mod")
open(path, "wb").write(out)
print(path, len(out), "bytes")
