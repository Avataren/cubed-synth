I1=[0x01,0x02,0x10,0x05,0xf3,0xf4,0x0a,0x06,0,0,0x04]
INS={1: dict(fm=I1)}
# crack it: an arpeggio keeps its step across rows with no effect or a one-shot (AT2 last_effect stays an arpeggio); a running effect or a note resets it
SPEC=dict(version=11, tempo=50, speed=4, instruments=INS, patterns={0: [(0,0,49,1,0x00,0x57),(1,0,0,0),(2,0,0,0,0x0c,0x30),(3,0,0,0,0x00,0x57),(4,0,0,0,0x0a,0x01),(5,0,0,0,0x00,0x57),(6,0,0,0),(7,0,51,0),(8,0,0,0,0x00,0x47),(9,0,0,0,0x2a,0x37)]})
