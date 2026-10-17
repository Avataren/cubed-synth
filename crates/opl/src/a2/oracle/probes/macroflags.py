I1=[0x01,0x01,0x2b,0x34,0xf8,0xf0,0xf2,0xf6,0x02,0x06,0x00]
I2=[0x00,0x01,0x37,0x2f,0xfa,0xf3,0xff,0xf7,0x06,0x00,0x00]
D=[0,0,0,0,1,1,1,1,1,1,1,1,0,0,0,0,1,1,1,1,1,1,1,1,0,0,0,1]
def st(f10, dur, ar=0xf8): return dict(fm=[0x01,0x01,0x2b,0x34,ar,0xf0,0xf2,0xf6,0x02,0x06,f10], duration=dur)
# grab bag: FM macro step flags 0x80 (retrigger), 0x40 (envelope restart), 0x20 (zero pitch until the next step);
# on a 4-op pair the first track never retriggers and the second's retrigger restarts the first's tables
M1=dict(length=4, steps=[st(0x80,3), st(0x40,2,0xc3), st(0x20,2), st(0x00,2)])
M2=dict(length=2, steps=[st(0x80,4), st(0x80,1,0xc3)])
SPEC=dict(version=11, four_op=2, speedup=2, tempo=49, speed=4, instruments={1: dict(fm=I1), 2: dict(fm=I2), 3: dict(fm=I1)}, fm_macros={1: M1, 2: M2, 3: M2}, disabled={1: D, 2: D, 3: D}, patterns={0: [(0,0,49,1),(0,2,0,1),(0,3,52,2),(4,0,50,1),(4,2,0,3),(4,3,53,2)]})
