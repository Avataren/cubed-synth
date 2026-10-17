I1=[0x01,0x02,0x10,0x05,0xf3,0xf4,0x0a,0x06,0,0,0x04]
INS={1: dict(fm=I1)}
# oskari wins: a key-off or a note on a row without an arpeggio keeps the arpeggio's step (only a key-off or note on its own row resets it)
SPEC=dict(version=11, tempo=50, speed=4, instruments=INS, patterns={0: [(0,0,49,1,0x00,0x22),(1,0,0xff,0),(2,0,51,0),(3,0,0,0,0x00,0x22),(4,0,0xff,0,0x00,0x22),(5,0,0,0,0x00,0x22)]})
