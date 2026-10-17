I1=[0x01,0x21,0x25,0x15,0xf0,0x10,0x00,0x02,1,6,0x0a]
# corridors of time: a porta note after a key-off keys on again, which restarts the vibrato table around the new pitch; an instrument past the last one with FM data stops the tables
SPEC=dict(version=11, tempo=50, speed=6, instruments={1: dict(fm=I1), 2: dict(fm=I1), 4: dict(fm=I1)}, vib_macros={1: dict(length=8, speed=1, loop_begin=1, loop_length=8, data=[1,2,3,4,5,4,3,2])}, fm_macros={1: dict(vib=1)}, patterns={0: [(0,0,49,1),(1,0,0xff,0),(2,0,61,0,0x03,0xff),(3,0,0,0,0x03,0x10),(4,0,0,9,0x23,0xf0),(5,0,52,1)]})
