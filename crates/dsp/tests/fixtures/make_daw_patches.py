#!/usr/bin/env python3
"""Writes daw_patches.json: a few patches of another_synth's public/default-patch.json
(a bank of 53, 38 MB with its embedded samples) without their audioAssets, so the
tests can edit and load real DAW patches without the bank. Usage:

    make_daw_patches.py <path to default-patch.json>

The patches (indices in the bank): 0 SawBass1 (filter), 1 Snare1 (sampler), 5 Pad 1
(sampler, 3 voices), 6 FMBass 1 (FM), 7 Flutey (wavetable, LFO, macro routes),
13 Organ1 (4 voices), 34 E. Piano 1 (8 voices), 52 Gritty Growl Bass (odd extra keys).
"""
import json
import os
import sys

src = sys.argv[1]
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "daw_patches.json")
bank = json.load(open(src))
picked = {"metadata": {"id": "fixture", "name": "DAW patches (fixture)", "version": 1}, "patches": []}
for i in [0, 1, 5, 6, 7, 13, 34, 52]:
    patch = bank["patches"][i]
    patch["audioAssets"] = {}
    picked["patches"].append(patch)
json.dump(picked, open(out, "w"), separators=(",", ":"))
