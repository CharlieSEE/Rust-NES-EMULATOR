# Playable Super Mario Bros. — NES Emulator Design

**Date:** 2026-10-16

## Intent

Boot the user's legally-dumped cartridge, reach the title screen, and play
Super Mario Bros. through 1-1 and beyond. The measure of done is a person
holding the keyboard, moving Mario, jumping, stomping, bumping blocks, and
hitting the flagpole — with no corruption and no desync.

Written from scratch in this directory. An older emulator exists in a sibling
directory; none of its code is copied. Its renderer has a known bug (score
digits spill down the screen centre, palette wrong), and its boot screenshot
is used here as a visual reference for what *broken* looks like, not as a
source.

## Goals

- Playable SMB1 from the real cartridge, picture and input.
- Cycle-accurate enough that the status bar and world render correctly.
- One binary, one argument: the path to a `.nes` file.

## Non-goals

- Audio. The APU is not implemented. SMB's music engine will drift from real
  hardware; gameplay is not audio-timed, so playing is unaffected.
- Other games, other mappers, other NES 2.0 features.
- Save states, rewind, debugger, disassembly, cheats.
- Accuracy for its own sake. Only what this cartridge exercises.

## Anti-goals

- A test-ROM harness, cycle counters, and an oracle to diff against. That is
  real work for a goal nobody asked for.
- A `Mapper` trait with one implementation. Mapper 66 is a struct.

## Constraints

- Rust, edition 2021, stable 1.99.
- `sdl2` crate 0.38.0, default features, system SDL2 2.32.74 (present).
- Picture only. `$4010-$4017` reads return 0 and writes are dropped; the game
  writes there and reads must not fault.
- Single crate. Tests inline in each module.

## Hardware facts about this cartridge

Read off the file, not assumed:

```
4e 45 53 1a   "NES\x1a"
04            4 x 16 KiB PRG = 64 KiB
02            2 x 8 KiB CHR  = 16 KiB claimed
21 48         mapper 0x42 = 66, vertical mirroring, no trainer, no battery
```

Three places where the header lies or the board overrides it, all confirmed by
the game running:

1. **Bus conflicts select the bank.** Writing to `$8000-$FFFF` only changes the
   PRG bank if the byte written already equals the byte the CPU would read at
   that address. Without this rule the mapper is plain UxROM and the cartridge
   never leaves its reset vector. This is the difference between mapper 66 and
   mapper 2, and it is the single fact most likely to waste a day if missed.
2. **CHR is 8 KiB RAM, not the 16 KiB ROM the header claims.** The game uploads
   its own tile data at boot. Serving the header's CHR ROM, or serving RAM and
   ignoring the ROM, both produce a broken playfield.
3. **Nametables are vertical, hardwired.** Header bit 0 agrees. Not configurable.

Bank layout: 16 KiB switchable at `$8000`, last bank fixed at `$C000`. Power-on
leaves the switchable window on the second bank, so the two windows do not
alias.

## Parts

| Part | Responsibility |
|------|----------------|
| `rom.rs` | iNES header parse, mapper 66 bank state, bus conflicts |
| `cpu.rs` | 6502, documented opcodes, exact cycle counts, NMI/IRQ/BRK |
| `ppu.rs` | VRAM, registers, background pipeline, sprites, sprite-0 hit, NMI |
| `bus.rs` | CPU↔PPU address decode, `$4014` OAM DMA, APU stubs |
| `controller.rs` | `$4016` shift register, keyboard mapping |
| `main.rs` | Frame loop, SDL2 window, texture, key events, 60 fps pacing |

## The part that decides whether this works

SMB gates its status bar on sprite-0 hit landing on a specific scanline, and
polls `$2002` in tight loops waiting for it. A PPU that computes the right
pixels but at the wrong time in the frame produces a legible but broken screen.
The PPU is therefore written dot-by-dot against the documented fetch timeline,
not scanline-at-a-time. If 1-1 misbehaves, this is the first place to look.

## Success check

Run the real cartridge, press Start, play 1-1. Then compare a screenshot
against the reference boot image: no column of digits down the centre, no
garbage tiles, correct palette.
