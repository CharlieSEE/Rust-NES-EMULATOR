#!/usr/bin/env python3
"""Build `graphics.nes`, the golden-frame fixture that exercises rendering.

The ROM is a 16 KiB mapper-0 image with one 8 KiB CHR bank. It fills the
nametable with four tiles, sets a fine horizontal scroll, places two sprites,
and then enables background and sprite rendering forever. That gives the
golden-frame test a CI-covered picture with background, palette, sprite and
scroll output without committing a copyrighted game.

Run from this directory: `python3 make_graphics.py`.
"""

from pathlib import Path

# Hand-assembled 6502, loaded at $C000. The 16 KiB PRG is mirrored over
# $8000-$FFFF, so the reset vector points at $C000.
CODE = bytes([
    0x78,              # SEI
    0xD8,              # CLD
    0xA2, 0x40,        # LDX #$40
    0x8E, 0x17, 0x40,  # STX $4017    ; APU frame IRQ off
    0xA2, 0xFF,        # LDX #$FF
    0x9A,              # TXS
    0xE8,              # INX          ; X = 0
    0x8E, 0x00, 0x20,  # STX $2000    ; NMI off
    0x8E, 0x01, 0x20,  # STX $2001    ; rendering off
    0x8E, 0x10, 0x40,  # STX $4010    ; DMC IRQ off
    0x2C, 0x02, 0x20,  # BIT $2002
    0x10, 0xFB,        # BPL -5       ; first vblank
    0x2C, 0x02, 0x20,  # BIT $2002
    0x10, 0xFB,        # BPL -5       ; second vblank
    0xA9, 0x3F,        # LDA #$3F
    0x8D, 0x06, 0x20,  # STA $2006
    0xA9, 0x00,        # LDA #$00
    0x8D, 0x06, 0x20,  # STA $2006
    0xA2, 0x00,        # LDX #$00
    0xBD, 0x00, 0xC1,  # LDA $C100,X  ; palette
    0x8D, 0x07, 0x20,  # STA $2007
    0xE8,              # INX
    0xE0, 0x20,        # CPX #$20
    0xD0, 0xF5,        # BNE -11
    0xA9, 0x20,        # LDA #$20
    0x8D, 0x06, 0x20,  # STA $2006
    0xA9, 0x00,        # LDA #$00
    0x8D, 0x06, 0x20,  # STA $2006
    0xA2, 0x04,        # LDX #$04     ; four pages: nametable + attributes
    0xA0, 0x00,        # LDY #$00
    0x98,              # TYA
    0x29, 0x03,        # AND #$03     ; tile 0..3
    0x8D, 0x07, 0x20,  # STA $2007
    0xC8,              # INY
    0xD0, 0xF7,        # BNE -9
    0xCA,              # DEX
    0xD0, 0xF2,        # BNE -14
    0xA9, 0x03,        # LDA #$03
    0x8D, 0x05, 0x20,  # STA $2005    ; fine X scroll
    0xA9, 0x00,        # LDA #$00
    0x8D, 0x05, 0x20,  # STA $2005
    0xA9, 0x00,        # LDA #$00
    0x8D, 0x03, 0x20,  # STA $2003
    0xA9, 0xF0,        # LDA #$F0
    0xA2, 0x00,        # LDX #$00
    0x8D, 0x04, 0x20,  # STA $2004    ; hide all sprites
    0xE8,              # INX
    0xD0, 0xFA,        # BNE -6
    0xA9, 0x00,        # LDA #$00
    0x8D, 0x03, 0x20,  # STA $2003
    0xA9, 0x50,        # LDA #80
    0x8D, 0x04, 0x20,  # STA $2004    ; sprite 0 Y
    0xA9, 0x02,        # LDA #2
    0x8D, 0x04, 0x20,  # STA $2004    ; tile
    0xA9, 0x00,        # LDA #0
    0x8D, 0x04, 0x20,  # STA $2004    ; palette 0
    0xA9, 0x64,        # LDA #100
    0x8D, 0x04, 0x20,  # STA $2004    ; X
    0xA9, 0x78,        # LDA #120
    0x8D, 0x04, 0x20,  # STA $2004    ; sprite 1 Y
    0xA9, 0x03,        # LDA #3
    0x8D, 0x04, 0x20,  # STA $2004    ; tile
    0xA9, 0x01,        # LDA #1
    0x8D, 0x04, 0x20,  # STA $2004    ; palette 1
    0xA9, 0xA0,        # LDA #160
    0x8D, 0x04, 0x20,  # STA $2004    ; X
    0xA9, 0x00,        # LDA #$00
    0x8D, 0x00, 0x20,  # STA $2000
    0xA9, 0x1E,        # LDA #$1E
    0x8D, 0x01, 0x20,  # STA $2001    ; background and sprites on
    0x4C, 0x9F, 0xC0,  # JMP $C09F
])

PALETTE = bytes([
    0x0F, 0x01, 0x11, 0x21,
    0x0F, 0x06, 0x16, 0x26,
    0x0F, 0x09, 0x19, 0x29,
    0x0F, 0x04, 0x14, 0x24,
    0x0F, 0x05, 0x15, 0x25,
    0x0F, 0x0A, 0x1A, 0x2A,
    0x0F, 0x0B, 0x1B, 0x2B,
    0x0F, 0x07, 0x17, 0x27,
])


def tile(chr_data: bytearray, index: int, low: bytes, high: bytes) -> None:
    chr_data[index * 16 : index * 16 + 8] = low
    chr_data[index * 16 + 8 : index * 16 + 16] = high


def main() -> None:
    prg = bytearray(16 * 1024)
    prg[0x0000 : len(CODE)] = CODE
    prg[0x0100 : 0x0100 + len(PALETTE)] = PALETTE
    for offset in (0x3FFA, 0x3FFC, 0x3FFE):
        prg[offset : offset + 2] = (0xC000).to_bytes(2, "little")

    chr_data = bytearray(8 * 1024)
    tile(chr_data, 0, bytes([0xAA, 0x55] * 4), bytes(8))
    tile(chr_data, 1, bytes(8), bytes([0xFF] * 8))
    tile(chr_data, 2, bytes([0x18, 0x3C, 0x7E, 0xFF, 0xFF, 0x7E, 0x3C, 0x18]), bytes(8))
    tile(chr_data, 3, bytes([0xFF, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0xFF]), bytes(8))

    header = b"NES\x1a" + bytes([1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    rom = header + prg + chr_data
    out = Path(__file__).with_name("graphics.nes")
    out.write_bytes(rom)
    print(f"wrote {out} ({len(rom)} bytes)")


if __name__ == "__main__":
    main()
