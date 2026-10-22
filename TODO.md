# Feature backlog

Current state: mappers 0/1/2/66, cycle-based PPU with scanline IRQ support in the
cartridge, APU without DMC, battery saves, launcher, scripted input and frame
dumps. Everything below is not implemented.

Effort: S = hours, M = a day, L = a few days, XL = a week or more.

## Suggested order

1. Mapper 4 (MMC3) — largest library gain.
2. Save states — unlocks rewind, frame advance and better debugging.
3. DMC channel — completes the APU.
4. Unofficial CPU opcodes — removes hard crashes.
5. Zapper — makes the bundled Duck Hunt fully playable.

---

## Unlock more games

- [ ] **Mapper 4 (MMC3)** — SMB3, Kirby, Mega Man 3–6, TMNT, Batman. Bank registers, 8 KiB CHR banking, PRG/CHR modes, scanline IRQ counter, mirroring control. *Effort: L. Highest compatibility payoff.*
- [ ] **Mapper 3 (CNROM)** — CHR bank register with bus conflicts. *Effort: S.*
- [ ] **Mapper 7 (AxROM)** — 32 KiB PRG banking, one-screen mirroring. *Effort: S.*
- [ ] **Mapper 9/10 (MMC2/MMC4)** — Punch-Out!! and Fire Emblem class boards, latch-based CHR switching. *Effort: M.*
- [ ] **Mapper 71 (Camerica/Codemasters)** — simple UxROM variant used by many unlicensed games. *Effort: S.*
- [ ] **Unofficial CPU opcodes** — `cpu.rs` currently panics on undocumented opcodes. Implement the stable illegal-opcode set (SLO, RLA, SRE, RRA, SAX, LAX, DCP, ISC, ANC, ALR, ARR, AXS, and the NOP variants). *Effort: M.*
- [ ] **DMC channel** — `$4010–$4013` are currently dropped. Sample fetching through the bus, output level, IRQ, `$4015` status bits, CPU stall conflicts. *Effort: M.*
- [ ] **PAL support** — 50 Hz timing, PAL APU period tables, PAL palette, header region detection. The bundled European Zelda ROM is a test candidate. *Effort: M.*
- [ ] **NES 2.0 header support** — parse console type, mapper/submapper, PRG/CHR size exponents, RAM sizes. *Effort: S.*
- [ ] **FDS support** — BIOS, disk I/O, disk IRQs, writable media, per-side saves. *Effort: XL.*
- [ ] **MMC5 support** — expansion audio, ExRAM, split-screen mode, large PRG banking. Only if a specific MMC5 game is wanted. *Effort: XL.*

## Player quality of life

- [ ] **Save states** — serialize CPU, PPU, APU, bus, cartridge/mapper and PRG RAM. Version the format and refuse incompatible states cleanly. *Effort: L. Prerequisite for rewind, frame advance and TAS work.*
- [ ] **Quicksave/quickload slots** — hotkeys for 1–9 save slots plus load-latest. *Effort: S, after save states.*
- [ ] **Rewind** — ring buffer of compressed save states sampled every N frames. *Effort: M, after save states.*
- [ ] **Fast-forward** — hold-to-turbo, with audio muted or pitch-shifted. *Effort: S.*
- [ ] **Frame advance** — pause plus single-frame stepping. *Effort: S.*
- [ ] **Zapper + mouse** — light-gun register at `$4017` bit 4, mouse aim, trigger mapping, hit detection against the framebuffer/beam position. Duck Hunt is the test case. *Effort: M.*
- [ ] **Two-player input** — bus already exposes controller 2; map a second keyboard set and a second gamepad, add a per-game player-count override. *Effort: S.*
- [ ] **Remappable controls** — config file for keyboard and gamepad bindings, with defaults matching the current README. *Effort: M.*
- [ ] **Volume control** — hotkeys plus persisted config, separate master and APU balance. *Effort: S.*
- [ ] **Per-game configuration** — remember volume, video filter, cheats and controller count per ROM hash. *Effort: M.*
- [ ] **Recent ROM list** — extend the launcher config beyond the last folder. *Effort: S.*
- [ ] **Live screenshot hotkey** — reuse the `--dump` PPM writer, save next to the ROM or in a screenshots folder. *Effort: S.*
- [ ] **Game Genie cheats** — 6- and 8-letter code parsing, ROM patching, enable/disable at runtime. *Effort: M.*
- [ ] **IPS/BPS patch support** — apply patches at load time, never modify the source ROM. *Effort: M.*

## Accuracy, tooling and presentation

- [ ] **Debugger** — pause/step, breakpoints, CPU registers and disassembly, PPU state, nametable/OAM/palette viewers, memory watch, APU channel state. *Effort: XL; build incrementally on the existing headless harness.*
- [ ] **TAS movie playback** — `.fm2` input format, deterministic emulation, record and replay. *Effort: M, after save states; `--play` is the seed.*
- [ ] **Golden-frame CI tests** — hash selected frames for bundled test ROMs and real games, fail on rendering regressions. *Effort: M.*
- [ ] **NTSC composite filter** — optional RGB→NTSC→RGB shader for authentic color fringing. *Effort: M.*
- [ ] **CRT shader / integer scaling** — optional scaling mode with scanlines and aspect correction. *Effort: M.*
- [ ] **Audio low-pass filter** — tame the APU's harsh high frequencies, optional and off by default. *Effort: S.*
- [ ] **Dynamic rate control** — adjust resampling from queue fill instead of sleeping on queue size, for smoother audio. *Effort: M.*
- [ ] **NSF player** — play `.nsf` music files by driving CPU+APU with a stub bus; needs DMC for full accuracy. *Effort: M.*

---

## Notes

- Every compatibility feature needs a test ROM or a real-game smoke run. Freely
  redistributable test ROMs belong in `test_roms/`; commercial ROMs stay ignored.
- Keep the single-binary, no-runtime-config philosophy; new settings belong in
  the existing config file next to the binary.
- Preserve current behavior unless a feature explicitly changes it. PAL, NTSC
  filter and shaders are opt-in.
