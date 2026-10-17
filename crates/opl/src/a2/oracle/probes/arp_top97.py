I1=[0x01,0x21,0x25,0x15,0xf0,0x10,0x00,0x02,1,6,0x0a]
# clear: arpeggios (table and effect) reaching note 97 or beyond play block 7's top (AT2 nFreq caps at 96)
SPEC=dict(version=11, tempo=50, speed=6, instruments={1: dict(fm=I1), 2: dict(fm=I1)}, arp_macros={1: dict(length=4, speed=2, data=[0,24,25,0x80+97])}, fm_macros={1: dict(arp=1)}, patterns={0: [(0,0,73,1),(2,0,85,2,0x00,0xcf),(3,0,0,0,0x00,0xff)]})
