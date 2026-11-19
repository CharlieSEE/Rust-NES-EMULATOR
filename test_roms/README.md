# Test ROMs

Freely redistributable diagnostic ROMs, committed so the integration tests run
on a clean checkout and in CI instead of skipping.

- `apu_2005/` — blargg's `apu_2005.07.30` frame-counter and length-counter
  suite, with its readme and the assembly source that shipped alongside it.
- `apu_reset/` — blargg's APU reset tests.
- `blargg_ppu_tests/` — the three PPU tests that are stable across boards:
  palette RAM, sprite RAM/DMA, and VRAM buffering. Their result code is beeped.
- `golden/` — a hand-built mapper-0 fixture with background, sprites and fine
  scroll. `make_graphics.py` rebuilds `graphics.nes`; the golden-frame test
  uses it so CI covers more than text screens.
- `nestest/` — kevtris's `nestest.nes` and its golden CPU trace; the test runs
  every logged instruction and checks PC, A, X, Y, P, SP and cycles.

Game ROMs are **not** committed; `NES_ROMS/` is ignored. Tests that need one
(SMB, Zelda, Mega Man 2) skip when it is absent.
