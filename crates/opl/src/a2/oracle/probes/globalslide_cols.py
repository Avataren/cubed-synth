I1=[0x01,0x02,0x10,0x05,0xf3,0xf4,0x0a,0x06,0,0,0x04]
INS={1: dict(fm=I1)}
# fmnexus6: a global slide (2E/2F) also runs, for its row, on every later track whose same column is free: empty, an extended command (23/24/29) or a 03 that does not run (AdPlug; current AT2 gives it to every later track)
SPEC=dict(version=11, tempo=50, speed=4, instruments=INS, patterns={0: [(0,0,49,1),(0,1,49,1),(0,2,49,1),(0,3,49,1),(0,4,49,1),(0,5,49,1),(0,6,49,1),
 (1,1,0,0,0x2f,0x08),(1,2,0,0,0x09,0x23),(1,3,0,0,0,0,0x09,0x23),(1,4,0,0,0x0a,0x01),(1,5,53,0),
 (2,0,0,0,0x2e,0x04),(3,0,0,0),(4,3,0,0,0x2f,0x02),(4,4,0,0,0,0,0x2f,0x03),
 (6,0,0,0,0x2f,0x04),(6,1,0,0,0x23,0x23),(6,2,0,0,0x03,0x01),(6,3,0,0,0x24,0x41),(6,4,0,0,0x29,0x11),(6,5,0,0,0x12,0x20)]})
