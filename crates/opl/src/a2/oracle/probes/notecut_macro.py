I1=[0x01,0x21,0x25,0x15,0xf0,0x10,0x00,0x02,1,6,0x0a]
# oskari leads: a note cut (&3x) is a key-off to the instrument's tables, which go to their key-off position
SPEC=dict(version=11, tempo=50, speed=6, speedup=2, instruments={1: dict(fm=I1)}, fm_macros={1: dict(length=6, loop_begin=1, loop_length=4, keyoff=5, steps=[dict(fm=[0,0,0x10+i,0x30+i,0,0,0,0,0,0,0],duration=2) for i in range(6)])}, patterns={0: [(0,0,49,1),(2,0,51,1,0x24,0x32),(4,0,53,1)]})
