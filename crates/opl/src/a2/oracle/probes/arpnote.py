I1=[0x01,0x02,0x10,0x00,0xf3,0xf4,0x0a,0x06,0,0,0x04]
INS={1: dict(fm=I1)}
# intro-tune coop: an arpeggio, a note with no effect, an empty row, then the arpeggio again with no note
SPEC=dict(tempo=50, speed=6, instruments=INS, patterns={0: [(0,0,49,1,0x00,0x37),(1,0,0,0,0x00,0x37),(2,0,37,1),(4,0,0,0,0x00,0x37),(5,0,0,0,0x00,0x37),(6,0,49,0),(7,0,0,0,0,0x37)]})
