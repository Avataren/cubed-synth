I1=[0x01,0x02,0x10,0x05,0xf3,0xf4,0x0a,0x06,0,0,0x04]
INS={1: dict(fm=I1), 2: dict(fm=I1), 3: dict(fm=I1)}
# fm63b: a note on the first track of a 4-op pair (AT2 output_note) also becomes the note of the track before it, clearing its key-off: that track's next porta note does not key on again
SPEC=dict(version=11, tempo=50, speed=4, four_op=2, instruments=INS, patterns={0: [(0,1,60,1),(1,1,0xff,0),(2,2,51,2),(2,3,51,3),(3,1,58,0,0x03,0x0f),(4,1,0,0,0x03,0x0f),(5,1,0xff,0),(6,1,55,0,0x03,0x0f)]})
