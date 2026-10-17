I1=[0x01,0x01,0x3f,0x20,0xf0,0xf0,0x0f,0x0f,0,0,0x04]
INS={1: dict(fm=I1)}
# AdPlug quirk: a tremolo in the second column only ever makes the note louder (column 1 is a full sine); with 0A before it in column 1
SPEC=dict(version=11, tempo=50, speed=6, instruments=INS, patterns={0: [(0,0,49,1)]+[(r,0,0,0,0,0,0x16,0x44) for r in range(1,6)]+[(r,0,0,0,0x0a,0x01,0x16,0x21) for r in range(6,9)]+[(r,0,0,0,0x16,0x44) for r in range(9,14)]})
