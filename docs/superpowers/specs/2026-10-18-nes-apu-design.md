# Sound for the NES Emulator — APU Design

**Date:** 2026-10-18

**Update 2026-10-23:** the DMC is no longer cut. `$4010-$4013`, sample
fetching through the CPU bus, the sample-end IRQ and `$4015` bits 4/7 are
implemented, and `apu_reset/works_immediately.nes` now passes (17/17). The DMC
sections below record the original decision and its measured cost.

## Intent

Give the emulator the machine part it never had: sound. Today `$4015` reads 0
and `$4010-$4017` writes are dropped (`bus.rs`), so SMB runs silent and the old
design doc's non-goal — "Audio. The APU is not implemented" — is the work
item now.

The measure of done is twofold and both halves are measurable:

1. blargg's two APU test suites run headless in `cargo test`: the 2005
   frame-counter suite (11 ROMs) passes, and the apu_reset suite (6 ROMs)
   passes 5 of 6 — `works_immediately` is defeated by the DMC cut and is
   asserted as an expected failure, code 2.
2. Super Mario Bros. plays its title music, 1-1 theme, coin/jump/stomp effects,
   and 1-1 still plays exactly as it does now.

No audio capture diffing. No golden WAVs. The ROMs assert themselves.

## Goals

- Both pulse channels (duty, envelope, sweep, length), triangle (linear
  counter, length), noise (15-bit LFSR, envelope, length).
- Frame counter: both modes, the measured step tables and the $4017 jitter
  rule, frame IRQ flag, level-triggered IRQ line.
- Nonlinear mixer per NESdev → 48 kHz s16 stereo → SDL2 audio queue. `sdl2`
  is already the one dependency.
- 16 of 17 test ROMs pass headless, the DMC-dependent one excepted as
  documented; SMB is audibly right and visually unchanged.

## Non-goals

- **DMC, fully cut.** `$4010-$4013` writes are dropped, `$4015` bit 4 and bit
  7 (DMC IRQ flag) read 0, as today. The cut was agreed with the user with its
  cost measured, not assumed: `apu_reset/works_immediately.nes` does poke DMC
  — it writes `$4010=$8F`, `$4013=$01`, enables `$4015=$1F`, then requires
  bit 4 set (bytes remaining) and bit 7 set once the sample ends — so that
  ROM is a documented expected failure, code 2 (see Success check). DMC is
  the one channel with real cost: sample playback plus CPU-cycle-stealing
  DMA. If the bar ever becomes 17/17 the upgrade path is a counter-only DMC
  (rate countdown, bytes-remaining, IRQ flag, `$4015` bits; no audio, no
  DMA), sized by the builder check at ~40 lines; deliberately not built now.
  SMB itself never uses DMC.
- PAL, NSF, cartridge expansion audio, volume/mute UI, audio device selection.
- Other mappers beyond the two this work touches: mapper 66 (the SMB cart,
  unchanged) and mapper 0 (the test ROMs, added — see below).
- Accuracy beyond what the suites and SMB observe.

## Anti-goals

- A resampler, audio callback thread, or DSP pipeline. SDL's queue, an integer
  accumulator, done.
- Sub-instruction CPU timing for its own sake. Only the timing ROMs may demand
  it (see "The part that decides whether this works"), and only after they say
  so with a failure code.
- An audio oracle: no WAV diffing, no recorded reference. The 2005 suite
  reports by beeping; the harness counts beeps off the APU's own sample
  buffer, which also proves the mixer end to end.

## Constraints

- Rust, single crate, tests inline in each module (the house style). The ROM
  harness is a `#[cfg(test)]` module in the binary, not a `tests/` integration
  suite, because the crate is a binary and has no library target.
- `sdl2` 0.38.0 already present; audio via `sdl2::audio`'s queue API.
- NTSC only. Frame = 29780.5 CPU cycles. CPU = 1789772.727 Hz.
- Existing ceilings stay: unmapped reads return 0 rather than open bus; no
  sub-instruction bus traffic unless a ROM forces it; no dummy reads/writes.
- Test ROMs are `.nes`, and `.nes` is gitignored (`rom.rs`'s `real_rom()` test
  already skips when absent). The harness skips the same way. Files were
  fetched from `github.com/christopherpow/nes-test-roms` (folders
  `blargg_apu_2005.07.30/`, `apu_reset/`) into `test_roms/`; the 2005 sources
  actually referenced (`prefix_apu.asm`, `apu_util.asm`) are saved under
  `test_roms/apu_2005/source/`.
- If SDL audio fails to open (no device, CI), the emulator keeps running
  silently rather than failing: the picture is the primary output.

## What the test ROMs demand outside the APU

Measured from the files, not assumed:

- **2005 suite: mapper 0, NROM-128.** Header `4E 45 53 1A 01 00 00 00` — 16 KiB
  PRG, no CHR at all (header byte 5 = 0), so pattern memory must be **CHR
  RAM**. Result: code shown on screen *and* beeped N times; N = result code,
  **1 = pass** (`tests.txt`).
- **apu_reset: mapper 0, NROM-256.** Header `4E 45 53 1A 02 01 01 00` — 32 KiB
  PRG, 8 KiB CHR ROM (the existing `load_chr_rom` copies 16 KiB exactly and
  would panic on 8 KiB). Result **at `$6000`**: `$80` running, `$81` needs a
  reset press (delayed ≥100 ms), `$00-$7F` done — and **0 = pass** here, not 1
  (different framework from 2005; see `apu_reset/readme.txt`). Text output
  lives at `$6004`, terminator moved as it grows. So mapper 0 needs **8 KiB of
  PRG RAM at `$6000-$7FFF`** (mapper 66 keeps today's open-bus behavior there).
- **Warm reset.** apu_reset tests power-on *and* reset behavior, and asks for a
  reset button press mid-test. The emulator has no reset path: `Bus::reset`
  zeroes RAM and is only ever called at startup. Split power-on from reset.
  **Power-on** (`Bus::power_on`, what `main` calls at startup) clears RAM.
  **Reset** (what the harness calls when `$6000` reads `$81`) keeps RAM —
  hardware does — and applies exactly: CPU reset (vector, SP, interrupts
  masked), the apu_reset readme's APU rules (`$4015` cleared, `$4017`
  re-written with its last value, IRQ flag clear, length counters enabled),
  and nothing else. PPU, CHR, mapper and controllers stay as they are:
  hardware's reset line does not re-initialize the PPU, and no ROM here pins
  it either.
- The suites also need the PPU to be good enough to draw their screen text —
  it already is.

## Hardware facts and sources of truth

- `test_roms/apu_2005/readme.txt` is the authority on frame counter timing: it
  is blargg's *measured* correction to the NESdev wiki and contradicts the
  wiki in places. Mode 0/1 step tables, the "changes to the mode by writing
  `$4017` only occur on even internal APU clocks" jitter rule, length-halt and
  length-reload edge cases, and the reset behavior ("as if `$00` were written
  to `$4017` from 9 to 12 clocks before the first instruction") are all in it.
- `test_roms/apu_2005/tests.txt` says what each ROM tests and what each
  failure code means. It is the debugging guide when a code other than 1
  appears.
- `test_roms/apu_reset/readme.txt` is the authority on power-on vs reset
  differences (`$4015` cleared, `$4017` re-written with its last value at
  reset, IRQ inhibit flag sometimes cleared, length counters enabled).
- NESdev wiki pages (APU Pulse/Triangle/Noise, APU Frame Counter, APU Mixer)
  for register bit layouts and channel formulas not pinned by blargg:
  envelope and sweep semantics, linear counter, LFSR taps, mixer curves.
- SMB does **not** need the frame IRQ: this cartridge writes `$4017 = $C0`
  (bank 1, sound init) and `$FF` (bank 0), both mode 1 with the IRQ inhibit
  bit set — verified in the image. SMB's audio runs off its NMI handler. The
  IRQ line is still implemented because 06/07/08 of the 2005 suite test it, and
  because `cpu.rs`'s own comment says the current once-per-frame `$4015` poll
  must become a level-triggered line.

## Parts

| Part | Responsibility |
|------|----------------|
| `apu.rs` (new) | Pulse ×2, triangle, noise; envelope/sweep/length/linear; frame counter + IRQ flag; nonlinear mixer; sample queue |
| `bus.rs` | `$4000-$4017` routed to the APU; `$6000-$7FFF` PRG RAM when the cart has it; APU IRQ line query; per-CPU-cycle `Apu::tick` from the existing dot counter |
| `rom.rs` | mapper 0 NROM (16 KiB mirrored / 32 KiB direct, CHR ROM or RAM) and a "has PRG RAM" flag; mapper 66 path untouched |
| `ppu.rs` | CHR RAM mode: `$2007` writes to `$0000-$1FFF` land in pattern memory when the cart carries no CHR; 8 KiB CHR images load |
| `main.rs` | SDL2 audio device opens with the window (failure = silent run); drain the APU queue once per frame; IRQ checked before each `cpu.step` instead of the per-frame `$4015` poll |
| `apu_rom_test.rs` (new, `#[cfg(test)]`) | Headless harness: load a ROM, run frames, poll results; skip when ROM absent |

## The part that decides whether this works

Two timing-sensitive spots, both staged so the cheap version is tried first:

1. **When the APU is clocked inside an instruction.** `Cpu::step` batches its
   ticks at the instruction's end (`for _ in 0..cycles * 3 { bus.tick() }`).
   The APU ticks per CPU cycle on that existing signal, so a `$4015` read can
   land up to one instruction earlier than its hardware cycle. ROMs 04-08
   detect exactly this ("too soon" / "too late" codes). If they fail, the
   fallback is **APU-only cycle placement**: let the APU see cycle-ordered
   accesses (its per-cycle clock stepped at the read/write points inside
   `step`, e.g. through a new per-access hook on `BusLike`) while the PPU
   keeps today's batched three-dots-per-cycle ticking. The picture's md5
   gates then cannot move, and only the timing under test changes. Ticking
   the PPU at access points too would move every accepted picture baseline;
   that is a last resort and needs the human asked first. Do not
   pre-emptively restructure the CPU; the suite will say.
2. **IRQ dispatch latency.** `cpu.irq()` checked before each `step` is
   instruction-boundary polling: up to one instruction later than hardware's
   penultimate-cycle poll. ROM 08 (`irq_timing`) needs the handler invoked
   "at minimum 29833 clocks after writing $00 to $4017" — i.e. ~2 clocks after
   the flag. The line is level-triggered and `cpu.irq` no-ops while `I` is
   set, so no request is ever lost; `cpu.rs` documents that contract. If 08
   says "Too late", it is the same APU-only placement as (1) — the flag's
   low edge has to be observable earlier, which is a placement question, not
   a CPU-rewrite question.

Guessing these two placements wrong is what leaves the timing ROMs red, so
each has a named fix rather than a rewrite.

## Success check

- `cargo test` — 16 of 17 ROMs pass. `works_immediately` is asserted as an
  expected failure with **code 2**, so a different code (or an accidental
  pass) is flagged rather than ignored; the harness prints name and code for
  anything else. `tests.txt`/`readme.txt` decode a code.
- Unit tests per module: register decode and per-channel semantics, length
  table, envelope/sweep edge cases, frame counter step cycles, mixer output
  sanity (silence in, silence out; full-scale bounded), `$6000` RAM routing,
  CHR RAM writes, power-on vs reset APU differences.
- Manual: SMB — title music, 1-1 theme, coin/jump/stomp effects; script a run
  through 1-1 (`--play`) and check it against today's frame dumps that the
  picture did not move. If 1-1 changed, the IRQ path is the first suspect.
