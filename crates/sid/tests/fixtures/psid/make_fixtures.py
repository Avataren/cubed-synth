#!/usr/bin/env python3
"""Writes the synthetic PSID/RSID test tunes of cubed-sid's PSID tests.

They are made here, from a few lines of 6502 each, so that no real C64 tune
(each one is somebody's copyrighted work) is ever committed. `psid.rs` (the
integration test) and `oracle.mjs` (the DAW's own runner, for comparisons) read
the files this writes next to itself; `python3 make_fixtures.py` rewrites them,
and the test `fixtures_are_what_the_generator_writes` checks the bytes.

The assembler below knows only what these tunes use.
"""
import os
import struct
import sys

# mnemonic -> {mode: opcode}
OPS = {
    'lda': {'imm': 0xA9, 'zp': 0xA5, 'abs': 0xAD, 'absx': 0xBD, 'absy': 0xB9, 'zpx': 0xB5},
    'ldx': {'imm': 0xA2, 'zp': 0xA6, 'abs': 0xAE},
    'ldy': {'imm': 0xA0, 'zp': 0xA4, 'abs': 0xAC},
    'sta': {'zp': 0x85, 'abs': 0x8D, 'absx': 0x9D, 'absy': 0x99, 'zpx': 0x95},
    'stx': {'zp': 0x86, 'abs': 0x8E},
    'sty': {'zp': 0x84, 'abs': 0x8C},
    'adc': {'imm': 0x69, 'zp': 0x65, 'abs': 0x6D},
    'sbc': {'imm': 0xE9, 'zp': 0xE5, 'abs': 0xED},
    'and': {'imm': 0x29, 'zp': 0x25},
    'ora': {'imm': 0x09, 'zp': 0x05},
    'eor': {'imm': 0x49, 'zp': 0x45},
    'cmp': {'imm': 0xC9, 'zp': 0xC5, 'abs': 0xCD},
    'cpx': {'imm': 0xE0, 'zp': 0xE4},
    'inc': {'zp': 0xE6, 'abs': 0xEE},
    'dec': {'zp': 0xC6, 'abs': 0xCE},
    'inx': {'imp': 0xE8}, 'iny': {'imp': 0xC8}, 'dex': {'imp': 0xCA}, 'dey': {'imp': 0x88},
    'asl': {'imp': 0x0A, 'zp': 0x06}, 'lsr': {'imp': 0x4A, 'zp': 0x46},
    'tax': {'imp': 0xAA}, 'tay': {'imp': 0xA8}, 'txa': {'imp': 0x8A}, 'tya': {'imp': 0x98},
    'clc': {'imp': 0x18}, 'sec': {'imp': 0x38}, 'cli': {'imp': 0x58}, 'sei': {'imp': 0x78},
    'pha': {'imp': 0x48}, 'pla': {'imp': 0x68},
    'rts': {'imp': 0x60}, 'rti': {'imp': 0x40}, 'nop': {'imp': 0xEA},
    'jmp': {'abs': 0x4C}, 'jsr': {'abs': 0x20},
    'bne': {'rel': 0xD0}, 'beq': {'rel': 0xF0}, 'bcc': {'rel': 0x90}, 'bcs': {'rel': 0xB0},
    'bpl': {'rel': 0x10}, 'bmi': {'rel': 0x30},
}


class Asm:
    def __init__(self, origin):
        self.origin = origin
        self.lines = []  # (kind, ...)
        self.labels = {}
        self.pc = origin

    def label(self, name):
        self.labels[name] = self.pc

    def emit(self, text):
        text = text.split(';')[0].strip()
        if not text:
            return
        head = text.split(None, 1)[0]
        if head.endswith(':'):
            self.label(head[:-1])
            text = text[len(head):].strip()
            if not text:
                return
        if text.startswith('.byte'):
            vals = [self.num(v) for v in text[5:].split(',')]
            self.lines.append(('raw', bytes(vals)))
            self.pc += len(vals)
            return
        if text.startswith('.word'):
            names = [v.strip() for v in text[5:].split(',')]
            self.lines.append(('words', names))
            self.pc += 2 * len(names)
            return
        parts = text.split(None, 1)
        mn = parts[0].lower()
        arg = parts[1].strip() if len(parts) > 1 else ''
        mode, value = self.mode(mn, arg)
        n = 1 + {'imp': 0, 'imm': 1, 'zp': 1, 'zpx': 1, 'abs': 2, 'absx': 2, 'absy': 2, 'rel': 1}[mode]
        self.lines.append(('op', self.pc, mn, mode, value))
        self.pc += n

    def num(self, s):
        s = s.strip()
        if s.startswith('$'):
            return int(s[1:], 16)
        return int(s)

    def mode(self, mn, arg):
        ops = OPS[mn]
        if arg == '':
            return 'imp', None
        if arg == 'a':
            return 'imp', None
        if arg.startswith('#'):
            return 'imm', arg[1:]
        if 'rel' in ops:
            return 'rel', arg
        idx = None
        if arg.lower().endswith(',x'):
            idx, arg = 'x', arg[:-2]
        elif arg.lower().endswith(',y'):
            idx, arg = 'y', arg[:-2]
        known = arg.startswith('$') and int(arg[1:], 16) < 0x100
        base = 'zp' if known else 'abs'
        mode = base + (idx or '')
        if mode not in ops:
            mode = 'abs' + (idx or '')
        return mode, arg

    def value(self, v):
        if v.startswith('<'):
            return self.value(v[1:]) & 0xFF
        if v.startswith('>'):
            return self.value(v[1:]) >> 8
        if v in self.labels:
            return self.labels[v]
        return self.num(v)

    def assemble(self):
        out = bytearray()
        for line in self.lines:
            if line[0] == 'raw':
                out += line[1]
            elif line[0] == 'words':
                for n in line[1]:
                    out += struct.pack('<H', self.value(n))
            else:
                _, pc, mn, mode, value = line
                code = OPS[mn][mode]
                out.append(code)
                if mode == 'imp':
                    continue
                v = self.value(value)
                if mode == 'rel':
                    d = v - (pc + 2)
                    assert -128 <= d <= 127, (mn, value, d)
                    out.append(d & 0xFF)
                elif mode in ('imm', 'zp', 'zpx'):
                    out.append(v & 0xFF)
                else:
                    out += struct.pack('<H', v)
        return bytes(out)


def build(origin, source):
    a = Asm(origin)
    for line in source.strip().splitlines():
        a.emit(line)
    return a.assemble(), a.labels


def psid(code, load, init, play, *, kind=b'PSID', version=2, songs=1, start=1, speed=0,
         flags=0x14, name='', author='cubed-synth tests', released='', second=0, third=0):
    header_len = 0x76 if version == 1 else 0x7C
    h = bytearray(header_len)
    h[0:4] = kind
    struct.pack_into('>HHHHHHH', h, 4, version, header_len, load, init, play, songs, start)
    struct.pack_into('>I', h, 18, speed)
    for off, text in ((0x16, name), (0x36, author), (0x56, released)):
        h[off:off + 32] = text.encode('latin-1')[:32].ljust(32, b'\0')
    if version >= 2:
        struct.pack_into('>H', h, 0x76, flags)
        h[0x7A] = second
        h[0x7B] = third
    return bytes(h) + code


def fixtures():
    out = {}

    # 1. arp.sid: a PSID, two subsongs, play once per video frame. Voice 1 plays a
    # three-note arpeggio, a note every 8 frames (a sawtooth, gate on at the
    # note, off six frames later); the second subsong plays another three notes.
    code, lab = build(0x1000, """
init:   sta $f0
        asl a
        adc $f0
        sta $f3         ; base = 3 * subsong
        sta $f2         ; note index
        adc #3
        sta $f4         ; end of this subsong's notes
        lda #0
        sta $f1         ; frame counter
        lda #$0f
        sta $d418       ; volume
        lda #$09
        sta $d405       ; attack 0, decay 9
        lda #$f0
        sta $d406       ; sustain 15, release 0
        lda #$08
        sta $d403       ; pulse width high
        rts
play:   lda $f1
        and #7
        bne notnote
        ldx $f2
        lda freqlo,x
        sta $d400
        lda freqhi,x
        sta $d401
        lda #$21
        sta $d404       ; sawtooth, gate on
        inx
        cpx $f4
        bne keep
        ldx $f3
keep:   stx $f2
notnote: cmp #6
        bne count
        lda #$20
        sta $d404       ; gate off
count:  inc $f1
        rts
freqlo: .byte $e0, $7d, $ba, $c0, $2c, $85
freqhi: .byte $0b, $0e, $11, $16, $1a, $1e
""")
    out['arp.sid'] = psid(code, 0x1000, lab['init'], lab['play'], songs=2, start=1, name='Arp (test)')

    # 2. irq.sid: an RSID with its own CIA 1 timer A interrupt (no play address):
    # every 20000 cycles the handler steps voice 1's frequency and gates it.
    code, lab = build(0x2000, """
init:   sei
        lda #<handler
        sta $0314
        lda #>handler
        sta $0315
        lda #$7f
        sta $dc0d       ; no CIA interrupts
        lda #$00
        sta $f0
        lda #<20000
        sta $dc04
        lda #>20000
        sta $dc05
        lda #$81
        sta $dc0d       ; timer A interrupt on
        lda #$01
        sta $dc0e       ; start timer A, continuous
        lda #$0f
        sta $d418
        lda #$00
        sta $d405
        lda #$f0
        sta $d406
        cli
        rts
handler: inc $f0
        lda $f0
        asl a
        asl a
        asl a
        sta $d401       ; frequency high: steps by 8 per interrupt
        lda #$11
        sta $d404       ; triangle, gate on
        lda $dc0d       ; acknowledge
        jmp $ea81
""")
    out['irq.sid'] = psid(code, 0x2000, lab['init'], 0, kind=b'RSID', name='IRQ (test)')

    # 3. raster.sid: an RSID whose handler runs on a VIC raster interrupt at line 100.
    code, lab = build(0x2000, """
init:   sei
        lda #<handler
        sta $0314
        lda #>handler
        sta $0315
        lda #$7f
        sta $dc0d       ; CIA 1 interrupts off
        lda #100
        sta $d012
        lda #$1b
        sta $d011       ; raster bit 8 clear
        lda #$01
        sta $d01a       ; raster interrupt on
        lda #$0f
        sta $d418
        cli
        rts
handler: lda #$01
        sta $d019       ; acknowledge
        inc $f0
        lda $f0
        sta $d400
        jmp $ea81
""")
    out['raster.sid'] = psid(code, 0x2000, lab['init'], 0, kind=b'RSID', name='Raster (test)')

    # 4. two.sid: a v3 PSID with a second SID at $D420; each chip plays its own note.
    code, lab = build(0x1000, """
init:   lda #$0f
        sta $d418
        sta $d438       ; volume on both
        lda #$f0
        sta $d406
        sta $d426       ; sustain 15
        lda #$21
        sta $d404
        sta $d424       ; sawtooth, gate on
        lda #0
        sta $f0
        rts
play:   inc $f0
        lda $f0
        sta $d401       ; chip 1 voice 1 freq high
        eor #$ff
        sta $d421       ; chip 2 voice 1 freq high
        rts
""")
    out['two.sid'] = psid(code, 0x1000, lab['init'], lab['play'], version=3, second=0x42,
                          flags=0x14, name='Two SIDs (test)')

    # 5. cia2x.sid: a PSID with speed bit 0: CIA 1 timer A at 9830 cycles, about twice per frame.
    code, lab = build(0x1000, """
init:   lda #<9829
        sta $dc04
        lda #>9829
        sta $dc05
        lda #0
        sta $f0
        rts
play:   inc $f0
        lda $f0
        sta $d400
        rts
""")
    out['cia2x.sid'] = psid(code, 0x1000, lab['init'], lab['play'], speed=1, name='CIA 2x (test)')
    return out


if __name__ == '__main__':
    here = os.path.dirname(os.path.abspath(__file__))
    if len(sys.argv) > 1 and sys.argv[1] == '--check':
        bad = [n for n, b in fixtures().items() if open(os.path.join(here, n), 'rb').read() != b]
        sys.exit('stale: ' + ', '.join(bad) if bad else 0)
    for name, data in fixtures().items():
        with open(os.path.join(here, name), 'wb') as f:
            f.write(data)
        print(name, len(data), 'bytes')
