I1=[0x01,0x02,0x10,0x05,0xf3,0xf4,0x0a,0x06,0,0,0x04]
INS={1: dict(fm=I1, fine=3)}
# 03 without a note runs only when the column's last effect (AT2 last_effect) was 03: not after a one-shot row (09), yes after an empty one
SPEC=dict(version=11, tempo=50, speed=4, instruments=INS, patterns={0: [(0,0,49,1),(1,0,55,0,0x03,0x01),(2,0,0,0,0x09,0x20),(3,0,0,0,0x03,0x01),(4,0,61,0,0x03,0x02),(5,0,0,0),(6,0,0,0,0x03,0x00),(7,0,0,0,0x03,0x00)]})
