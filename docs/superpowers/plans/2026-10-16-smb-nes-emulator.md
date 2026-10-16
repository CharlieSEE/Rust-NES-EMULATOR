# Playable Super Mario Bros. Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build from scratch a Rust NES emulator that boots the user's real Super Mario Bros. + Duck Hunt cartridge and plays it with picture and keyboard input.

**Architecture:** One crate, six modules. `cpu.rs` executes 6502 instructions and drives `ppu.rs` one PPU dot per CPU cycle via `bus.rs`, which owns address decode and OAM DMA. `rom.rs` is mapper 66 with bus-conflict banking. `main.rs` runs a fixed-timestep frame loop into an SDL2 window.

**Tech Stack:** Rust 1.99 stable, edition 2021, `sdl2` 0.38.0 (system SDL2 2.32.74). No other dependencies.

**Spec:** `docs/superpowers/specs/2026-10-16-smb-nes-emulator-design.md`

## Global Constraints

- Rust edition 2021, stable toolchain 1.99. No nightly.
- Dependencies permitted: `sdl2 = "0.38.0"` with default features only. Nothing else, no dev-dependencies.
- Single crate at the repo root. Tests live inline in each module under `#[cfg(test)] mod tests`.
- Picture only. Reads of `$4010-$4015` return 0; writes to `$4010-$4013` and `$4015` and `$4017` are dropped and must not change machine state.
- OAM DMA via `$4014` stalls the CPU for 513 cycles, plus one more if the write lands on an odd cycle. The CPU tracks cycle parity to get this right.
- PPU is driven one **dot** at a time (341 dots × 262 scanlines). Three dots per CPU cycle. Do not rewrite this as a per-scanline function; the design says why.
- Mapper 66 PRG banking is **bus-conflict gated**. Do not implement it as plain UxROM even though the spec file is what makes it sound optional.
- CHR is 8 KiB of RAM that the game uploads to. The header's 16 KiB CHR ROM claim is ignored.
- Nametable mirroring is vertical, hardwired. Do not read it from the header.
- Colors are `u32` as `0x00RRGGBB`, from the standard 2C02 master palette, indexed by the 6-bit palette RAM value.

## Review Focus

Five failure modes this design implies that a naive implementation gets wrong, most likely first. Each has a test pinned in the owning task.

1. **Bank switching without the bus-conflict check** — the cartridge never advances past its reset vector and the screen stays blank. Symptom: no title screen, ever.
2. **CHR served as ROM rather than the 8 KiB RAM the game uploads to** — the playfield draws as one repeated tile and sprites lose their shapes. Reads and writes must both hit RAM.
3. **`$2002` not clearing the vblank flag on read, or not resetting the `$2006` write latch** — SMB polls `$2002` in tight loops; the game hangs on the title screen or the HUD never updates.
4. **Sprite-0 hit allowed at x=0 or x=255** — the status bar appears at the wrong time or flickers, because SMB uses the exact hit dot as a timing signal.
5. **8×16 sprite pattern fetch not wrapping the tile index at 32 tiles** — a tall Mario or a goomba straddling the bottom edge of the nametable samples the wrong tile.

---

### Task 1: Crate skeleton and an SDL2 window showing a test pattern

Gets the toolchain, the system SDL2 link, and the frame loop proven before any NES logic exists.

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: a running binary. Later tasks add `mod rom; mod cpu; mod ppu; mod bus; mod controller;` to `main.rs`.

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "nes"
version = "0.1.0"
edition = "2021"

[dependencies]
sdl2 = "0.38.0"

[profile.release]
opt-level = 3
```

- [ ] **Step 2: Write `src/main.rs` with a window that fills 256×240 with a known colour**

```rust
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::Color;

const W: usize = 256;
const H: usize = 240;
const SCALE: u32 = 3;

fn main() -> Result<(), String> {
    let sdl = sdl2::init().map_err(|e| e.to_string())?;
    let video = sdl.video().map_err(|e| e.to_string())?;
    let window = video
        .window("NES", (W as u32) * SCALE, (H as u32) * SCALE)
        .position_centered()
        .allow_highdpi()
        .build()
        .map_err(|e| e.to_string())?;
    let mut canvas = window
        .into_canvas()
        .map_err(|e| e.to_string())?;
    canvas.set_logical_size(W as u32, H as u32).map_err(|e| e.to_string())?;

    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture(sdl2::pixels::PixelFormatEnum::RGB24, W as u32, H as u32)
        .map_err(|e| e.to_string())?;

    let mut buffer = vec![0u32; W * H];
    for p in buffer.iter_mut() {
        *p = 0x00_3A_6E_32;
    }
    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;
    let mut running = true;
    while running {
        for event in events.poll_iter() {
            if matches!(event, Event::Quit { .. } | Event::KeyDown { keycode: Keycode::Escape, .. }) {
                running = false;
            }
        }
        texture.update(None, &buffer, W * 4).map_err(|e| e.to_string())?;
        canvas.clear();
        canvas.copy(&texture, None, None).map_err(|e| e.to_string())?;
        canvas.present();
        ::std::thread::sleep(::std::time::Duration::from_micros(16_667));
    }
    Ok(())
}
```

- [ ] **Step 3: Build and run it**

Run: `cargo build 2>&1 | tail -5`
Expected: compiles, links against system SDL2, prints no error.

Run: `cargo run`
Expected: a 768×720 window filled flat green. Escape closes it.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "feat: SDL2 window with frame pacing"
```

---

### Task 2: iNES loader and mapper 66 with bus conflicts

The highest-risk module in the project. If the conflict rule is wrong the cart never boots and every later debugging session is wasted.

**Files:**
- Create: `src/rom.rs`
- Modify: `src/main.rs` (add `mod rom;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub struct Cartridge { pub prg: Vec<u8>, pub chr_ram: Vec<u8>, bank: usize }
  impl Cartridge {
      pub fn load(bytes: &[u8]) -> Result<Cartridge, String>;
      pub fn cpu_read(&self, addr: u16) -> u8;
      pub fn cpu_write(&mut self, addr: u16, val: u8);
      pub fn ppu_read(&self, addr: u16) -> u8;
      pub fn ppu_write(&mut self, addr: u16, val: u8);
  }
  ```

- [ ] **Step 1: Write the failing test for the bus conflict**

In `src/rom.rs`, under `#[cfg(test)] mod tests`, build a synthetic 4×16 KiB PRG image where bank 0 is all `0xAA` and bank 3 is all `0xBB`, then:

```rust
#[test]
fn a_write_that_does_not_match_the_rom_does_not_switch_banks() {
    let mut c = four_banks_filled(0xAA, 0xBB);
    let before = c.cpu_read(0x8000);
    c.cpu_write(0x8000, 0x01);           // 0x01 != 0xAA
    assert_eq!(c.cpu_read(0x8000), before);
}

#[test]
fn a_write_that_matches_the_rom_switches_banks() {
    let mut c = four_banks_filled(0xAA, 0xBB);
    // $8000 currently reads bank 1 (power-on); aim at an address holding 0xAA.
    c.cpu_write(0x8000, 0xAA);           // matches -> bank 0 selected
    assert_eq!(c.cpu_read(0x8000), 0xAA);
}
```

Add helpers `four_banks_filled(a: u8, b: u8) -> Cartridge` and a 16-byte iNES header builder in the same test module.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test rom:: 2>&1 | tail -15`
Expected: compile error, `Cartridge` is undefined.

- [ ] **Step 3: Implement `Cartridge`**

Approach: `load` validates the magic and reads the header. Store `prg` as the full PRG image. Set `chr_ram` to exactly 8192 zero bytes, ignoring the header's CHR count. Set `bank = 1` at construction — power-on leaves the switchable window on the second bank, so the `$8000` and `$C000` windows do not alias.

`cpu_read` maps `$8000-$BFFF` to `bank * 0x4000 + (addr & 0x3FFF)` and `$C000-$FFFF` to `(n - 1) * 0x4000 + (addr & 0x3FFF)` where `n = prg.len() / 0x4000`.

`cpu_write` to `$8000-$FFFF` reads the byte that `cpu_read` would return at that same address; if it differs from `val`, return without changing state. Otherwise set `bank = val as usize % n`. This is the bus conflict, and it is the only correct behaviour for this board.

`ppu_read` and `ppu_write` index `chr_ram` at `addr & 0x1FFF`. Reads and writes both hit RAM.

- [ ] **Step 4: Add a test that CHR is writable RAM, not the header's ROM**

```rust
#[test]
fn chr_is_writable_ram_ignoring_the_headers_chr_rom_claim() {
    let mut c = four_banks_filled(0xAA, 0xBB);   // header claims 2x8 KiB CHR ROM
    assert_eq!(c.ppu_read(0x0000), 0x00);
    c.ppu_write(0x0000, 0x7B);
    assert_eq!(c.ppu_read(0x0000), 0x7B);
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test rom:: 2>&1 | tail -15`
Expected: all three PASS.

- [ ] **Step 6: Verify against the real cartridge's header**

Run: `cargo test -- --nocapture` and add a temporary `#[test]` that calls `Cartridge::load` on `Super Mario Bros. + Duck Hunt (USA).nes` read from `std::fs::read`, asserting `prg.len() == 65536` and `chr_ram.len() == 8192`. Confirm it PASSes, then delete the temporary test.

- [ ] **Step 7: Commit**

```bash
git add src/rom.rs src/main.rs
git commit -m "feat: mapper 66 with bus-conflict PRG banking and CHR RAM"
```

---

### Task 3: 6502 CPU with exact cycle counts

**Files:**
- Create: `src/cpu.rs`
- Modify: `src/main.rs` (add `mod cpu;`)

**Interfaces:**
- Consumes: a `&mut dyn` bus, but in practice `Bus` from Task 6. To keep Task 3 standalone, give `step` a generic parameter over two traits defined in `cpu.rs`:
  ```rust
  pub trait CpuRead { fn read(&mut self, addr: u16) -> u8; }
  pub trait CpuWrite { fn write(&mut self, addr: u16, val: u8); }
  pub trait PpuTick { fn tick(&mut self); }
  ```
  `Bus` implements all three later. `step` signature:
  ```rust
  impl Cpu {
      pub fn new() -> Cpu;
      pub fn reset(&mut self);
      pub fn step<R: CpuRead, W: CpuWrite, P: PpuTick>(&mut self, r: &mut R, w: &mut W, p: &mut P) -> u8;
      pub fn nmi<R: CpuRead, W: CpuWrite, P: PpuTick>(&mut self, r: &mut R, w: &mut W, p: &mut P);
      pub fn irq<R: CpuRead, W: CpuWrite, P: PpuTick>(&mut self, r: &mut R, w: &mut W, p: &mut P);
  }
  ```
  Returns total cycles consumed by the instruction, including any page-cross or branch penalty, and ticks the PPU 3× per cycle inside `step`.

- [ ] **Step 1: Write the failing cycle-count tests**

```rust
#[test]
fn documented_opcode_cycle_counts() {
    // immediate
    assert_eq!(cycles(b"\xA9\x00"), 2);              // LDA #$00
    // absolute read
    assert_eq!(cycles(b"\xAD\x34\x12"), 4);          // LDA $1234
    // absolute,X with a page crossing costs one more
    assert_eq!(cycles(b"\xBD\xFF\x12"), 5);          // LDA $12FF,X
    // indirect,Y with a page crossing costs one more
    assert_eq!(cycles(b"\xB9\xFF\x12"), 6);          // LDA ($12FF),Y
    // stores always take the base count, crossing or not
    assert_eq!(cycles(b"\x9D\xFF\x12"), 5);          // STA $12FF,X
    // control flow
    assert_eq!(cycles(b"\x4C\x00\x80"), 3);          // JMP $8000
    assert_eq!(cycles(b"\x20\x00\x80"), 6);          // JSR $8000
    assert_eq!(cycles(b"\x60"), 6);                  // RTS
    assert_eq!(cycles(b"\x48"), 3);                  // PHA
    assert_eq!(cycles(b"\xEA"), 2);                  // NOP
}

#[test]
fn a_taken_branch_costs_one_more_than_a_not_taken_one() {
    let mut c = Cpu::new();
    c.pc = 0x1000;
    c.ram_write_probe(0x1000, 0xD0); // BNE
    c.ram_write_probe(0x1001, 0x02); // offset +2
    c.ram_write_probe(0x1002, 0xEA);
    assert_eq!(c.take_branch_cycles(false), 2);
    assert_eq!(c.take_branch_cycles(true), 3);
}
```

`cycles()` is a test helper that builds a `Cpu` with a flat 64 KiB RAM probe and steps the given byte slice. `take_branch_cycles` is a `#[cfg(test)]` method returning the base 2 plus the page-cross/taken penalty.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test cpu:: 2>&1 | tail -15`
Expected: compile error, `Cpu` is undefined.

- [ ] **Step 3: Implement `Cpu`**

Registers: `a, x, y: u8`, `sp: u8` starting at `0xFD`, `pc: u16`, and a status byte with explicit bit constants (`C=0x01 Z=0x02 I=0x04 D=0x08 B=0x10 U=0x20 V=0x40 N=0x80`).

Build the opcode dispatch as a `match` on the opcode byte over **documented opcodes only**. SMB uses no undocumented opcodes, so do not implement them; if the cartridge later faults on one, that is the moment to add it, not before.

Cycle table: one `const CYCLES: [u8; 256]` with the documented base count for every opcode. Apply the two standard adjustments on top of the table, never inside it:
- +1 when an indexed read crosses a page boundary
- +1 for a taken branch, +1 more if it crosses a page boundary

`step` reads the opcode, dispatches, and ticks the PPU 3 times per consumed cycle so PPU and CPU stay in lockstep. Handle the NMI hijack: check for a pending NMI before fetching each opcode, and again before the last cycle of an instruction, pushing a 7-cycle interrupt sequence.

`nmi` pushes PC and the status with B clear, sets I, loads PC from `$FFFA`, consumes 7 cycles. `irq` does the same from `$FFFE` but only when I is clear. `reset` loads PC from `$FFFC` and sets `sp = 0xFD`, I set, B set.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test cpu:: 2>&1 | tail -15`
Expected: all CPU tests PASS.

- [ ] **Step 5: Add a test that the reset vector is what actually runs**

```rust
#[test]
fn reset_reads_the_vector_from_fffc() {
    let mut c = Cpu::new();
    c.ram_write_probe(0xFFFC, 0x34);
    c.ram_write_probe(0xFFFD, 0x12);
    c.reset();
    assert_eq!(c.pc, 0x1234);
}
```

- [ ] **Step 6: Commit**

```bash
git add src/cpu.rs src/main.rs
git commit -m "feat: 6502 CPU with exact cycle counts"
```

---

### Task 4: PPU registers, VRAM, and the background pipeline

Deliberately dot-by-dot. The design explains why.

**Files:**
- Create: `src/ppu.rs`
- Modify: `src/main.rs` (add `mod ppu;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub struct Ppu { /* private fields */ }
  impl Ppu {
      pub fn new() -> Ppu;
      pub fn tick(&mut self);
      pub fn cpu_read(&mut self, addr: u16) -> u8;
      pub fn cpu_write(&mut self, addr: u16, val: u8);
      pub fn frame_ready(&self) -> bool;
      pub fn framebuffer(&self) -> &[u32];
      pub fn take_frame(&mut self) -> Vec<u32>;
      pub fn nmi_pending(&self) -> bool;
      pub fn take_nmi(&mut self);
      pub fn rendering_enabled(&self) -> bool;
  }
  ```
  Framebuffer is 256×240 `u32` in `0x00RRGGBB`, row-major. `cpu_read`/`cpu_write` cover `$2000-$3FFF`, including the `$3F20-$3FFF` palette mirror and the nametable mirroring.

- [ ] **Step 1: Write the failing register tests**

```rust
#[test]
fn reading_2002_clears_vblank_and_resets_the_2006_latch() {
    let mut p = Ppu::new();
    p.cpu_write(0x2002, 0);
    assert!(p.status() & 0x80 != 0, "writing 2002 does not set vblank");
    p.cpu_write(0x2006, 0x21);   // high byte
    p.cpu_write(0x2006, 0x08);   // low byte
    let _ = p.cpu_read(0x2002);
    p.cpu_write(0x2006, 0x21);   // latch reset: this is the FIRST write again
    p.cpu_write(0x2006, 0x08);
    assert_eq!(p.vram_addr(), 0x2108, "a 2002 read must reset the write latch");
}

#[test]
fn a_2002_read_clears_the_vblank_flag() {
    let mut p = Ppu::new();
    p.set_status_for_test(0x80);
    assert_eq!(p.cpu_read(0x2002) & 0x80, 0x80);
    assert_eq!(p.cpu_read(0x2002) & 0x80, 0x00);
}

#[test]
fn writing_2006_twice_latches_the_full_address() {
    let mut p = Ppu::new();
    p.cpu_write(0x2006, 0x03);
    p.cpu_write(0x2006, 0x40);
    assert_eq!(p.vram_addr(), 0x0340);
}

#[test]
fn nametables_are_vertically_mirrored() {
    let mut p = Ppu::new();
    p.cpu_write(0x2006, 0x20); p.cpu_write(0x2006, 0x00);
    p.cpu_write(0x2007, 0xAB);
    p.cpu_write(0x2006, 0x28); p.cpu_write(0x2006, 0x00);   // $2800 == $2000
    assert_eq!(p.cpu_read(0x2007), 0xAB);
}
```

`status()`, `set_status_for_test()`, `vram_addr()` are `#[cfg(test)]` accessors.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ppu:: 2>&1 | tail -20`
Expected: compile error, `Ppu` is undefined.

- [ ] **Step 3: Implement registers and VRAM**

State: `vram: [u8; 2048]`, `palette: [u8; 32]`, `oam: [u8; 256]`, `secondary_oam: [u8; 32]`, `ctrl: u8`, `mask: u8`, `status: u8`, `oam_addr: u8`, and the 15-bit `vram_addr` with its paired `write_latch: bool`.

`cpu_read($2002)` returns `status` with the low 5 bits open bus masked to 0, then clears vblank, clears `write_latch`, and clears the PPU's pending NMI. `cpu_write($2000/$2001/$2005/$2006)` behave per the register docs; `$2006` alternates high/low using `write_latch`. `cpu_write($2007)` writes to VRAM at `vram_addr` with the nametable mirroring rule applied — vertical, so `$2000/$2400` are distinct and `$2800/$2C00` alias them. `$3F10/$3F14/$3F18/$3F1C` mirror `$3F00/$3F04/$3F08/$3F0C`, and `$3F20+` mirrors `$3F00+`. Writes increment `vram_addr` by 1, or by 32 when `ctrl` bit 2 is set.

- [ ] **Step 4: Write the failing background-fetch test**

```rust
#[test]
fn a_tile_is_fetched_and_shifted_into_the_framebuffer() {
    let mut p = Ppu::new();
    // CHR bank 0 is written directly by the caller in these tests.
    p.chr_write_for_test(0x0000, 0xFF);          // tile 0, low plane: all pixels set
    p.chr_write_for_test(0x0001, 0x00);          // tile 0, high plane: all clear
    p.upload_tile_for_test(0, 0x00, 0x00, 0);     // tile 0 at nametable (0,0), palette 0
    p.cpu_write(0x2001, 0x1E);                   // show bg+sprites, no clipping
    p.run_to(&[21]);                             // stop at the first visible scanline
    for x in 0..8 {
        assert_eq!(p.pixel(x, 0), 0x00_FF_FF_FF, "tile bit not shifted to the right pixel");
    }
    for x in 8..16 {
        assert_eq!(p.pixel(x, 0), BACKDROP, "tile bled past its 8 pixels");
    }
}

const BACKDROP: u32 = 0x00_00_00_00;
```

`chr_write_for_test`, `upload_tile_for_test`, `run_to(&[u32])`, and `pixel(x, y)` are `#[cfg(test)]` helpers. `run_to` ticks until the PPU has completed each listed scanline.

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test ppu:: 2>&1 | tail -20`
Expected: FAIL — `Ppu` has no background rendering.

- [ ] **Step 6: Implement the dot-by-dot background pipeline**

On visible scanlines 0–239 and on the pre-render line 261, when `mask` bit 3 (show background) is set, run the standard 8-dot fetch cycle. At `dot % 8`:

- `1` — fetch the nametable byte at `(vram_addr & 0x0FFF)`
- `3` — fetch the attribute byte, shifted by `(fine_x & 4) ? 2 : 0` and `(fine_y & 4) ? 4 : 0`
- `5` — fetch pattern low from the background pattern table
- `7` — fetch pattern high
- at every `% 8 == 0`, reload the 16-bit shift registers `bg_lo`/`bg_hi` from the latch registers and reload the attribute latches

At `dot == 256`, increment Y: if `vram_addr` is in the range that wraps vertically, set `fine_y = 0` and increment coarse_y, else increment `fine_y`. At `dot == 257`, reset coarse_x to 0 and copy horizontal bits from `t` (the `$2005`/`$2006` temp register) into `vram_addr`. During dots 280–304 of the pre-render line, copy the vertical bits from `t`.

`pixel(x, y)` indexes `framebuffer[y * 256 + x]`. Apply palette lookup with the backdrop short-circuit: if the pattern bits are both zero, emit the backdrop colour without a palette read, which is what makes colour 0 transparent in practice.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test ppu:: 2>&1 | tail -20`
Expected: all PPU tests PASS.

- [ ] **Step 8: Commit**

```bash
git add src/ppu.rs src/main.rs
git commit -m "feat: PPU registers, VRAM, and dot-by-dot background pipeline"
```

---

### Task 5: Sprite pipeline and sprite-0 hit

**Files:**
- Modify: `src/ppu.rs`

**Interfaces:**
- Consumes: `Ppu` from Task 4, same module, same fields.

- [ ] **Step 1: Write the failing sprite tests**

```rust
#[test]
fn an_8x16_sprite_wraps_its_tile_index_at_32_tiles() {
    let mut p = Ppu::new();
    // Sprite at fine_y 30 in 8x16 mode: tile index must wrap, not run into tile 1.
    p.chr_write_for_test(0x03F8, 0x00);   // tile 0 row 0 (addr $0000)
    p.chr_write_for_test(0x03F9, 0xFF);
    p.chr_write_for_test(0x0400, 0x00);   // tile 1 row 0 (addr $0010) — must NOT be read
    p.chr_write_for_test(0x0401, 0xFF);
    p.put_sprite_for_test(0, /* y */ 30, /* tile */ 1, /* attr */ 0x00);
    p.cpu_write(0x2000, 0x20);  // sprites from $1000, 8x16
    p.cpu_write(0x2001, 0x1E);
    p.run_to(&[21]);
    // Row 30 of the sprite maps to pattern table address $03F8 + (30 & 15) * 16
    assert!(p.any_pixel_set_in_row(30), "8x16 pattern fetch did not wrap at 32 tiles");
}

#[test]
fn sprite_zero_hit_is_suppressed_at_x_0_and_x_255() {
    let mut p = Ppu::new();
    p.setup_sprite0_over_background_for_test(/* x */ 0);
    p.cpu_write(0x2001, 0x1E);
    p.run_to(&[21]);
    assert!(!p.sprite0_hit(), "sprite-0 hit must not be reported at x=0");
}

#[test]
fn sprite_zero_hit_is_reported_when_opaque_pixels_overlap() {
    let mut p = Ppu::new();
    p.setup_sprite0_over_background_for_test(/* x */ 100);
    p.cpu_write(0x2001, 0x1E);
    p.run_to(&[21]);
    assert!(p.sprite0_hit());
}

#[test]
fn sprite_zero_hit_is_cleared_by_a_2002_read() {
    let mut p = Ppu::new();
    p.setup_sprite0_over_background_for_test(100);
    p.cpu_write(0x2001, 0x1E);
    p.run_to(&[21]);
    assert!(p.sprite0_hit());
    let _ = p.cpu_read(0x2002);
    assert!(!p.sprite0_hit());
}
```

`sprite0_hit()`, `any_pixel_set_in_row()`, `put_sprite_for_test()`, and `setup_sprite0_over_background_for_test()` are `#[cfg(test)]` helpers. The last one writes an opaque background tile and an opaque sprite-0 at the given x.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ppu:: 2>&1 | tail -20`
Expected: FAIL — sprite 0 hit is not modelled.

- [ ] **Step 3: Implement sprite evaluation and the sprite pipeline**

At `dot == 257` of visible scanlines 0–239 and on the pre-render line, evaluate `oam` for the next scanline: scan all 64 sprites, copy up to 8 into `secondary_oam` with their Y, tile, attributes, X, and an index, set the overflow flag in `status` bit 5 if a 9th was found, and stop.

During dots 1–64 of each visible scanline, fetch the pattern for `secondary_oam[dot - 1]`. In 8×8 mode the pattern address is `pattern_base + tile * 16 + fine_y`. In 8×16 mode (`ctrl` bit 5) the table base is forced to `$1000`, the tile index's low bit is used to pick the half, and the row index wraps: `addr = 0x1000 + (tile & 1) * 0x1000 + ((tile >> 1) * 16 + fine_y) % 256` — the modulo is the wrap the first test pins.

Shift sprite X counters down each dot. Set the sprite-0 hit flag when sprite 0's counters run out on a dot where its pixel is opaque **and** the background pixel there is opaque **and** the dot is not 0 and not 255. Those two suppressed dots are load-bearing for SMB's HUD timing, which is why the tests above exist.

Apply the priority bit from the attribute: a priority-1 sprite hides the background behind it. A transparent sprite pixel never sets the hit and never hides the background.

- [ ] **Step 4: Implement vblank and NMI timing**

At scanline 241, dot 1: set `status` bit 7 (vblank), clear sprite-0 hit, clear overflow, and set the pending-NMI flag if `ctrl` bit 7 is set. At scanline 261, dot 1: clear vblank, sprite-0 hit, and overflow. `nmi_pending()` reports the flag; `take_nmi()` clears it. `frame_ready()` goes true at scanline 241 dot 1 and `take_frame()` hands over a copy of the 256×240 buffer, which is what the window in Task 7 presents.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test ppu:: 2>&1 | tail -20`
Expected: all PPU tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/ppu.rs
git commit -m "feat: sprite pipeline, sprite-0 hit, and vblank NMI timing"
```

---

### Task 6: Bus, OAM DMA, controller, and APU stubs

**Files:**
- Create: `src/bus.rs`
- Create: `src/controller.rs`
- Modify: `src/main.rs` (add `mod bus; mod controller;`)

**Interfaces:**
- Consumes: `Cartridge` (Task 2), `Cpu` (Task 3), `Ppu` (Task 4/5).
- Produces:
  ```rust
  pub struct Bus { pub cart: Cartridge, pub cpu_ram: [u8; 2048], pub ppu: Ppu, pub controllers: [Controller; 2] }
  impl Bus {
      pub fn new(cart: Cartridge) -> Bus;
      pub fn reset(&mut self);
      pub fn cpu_read(&mut self, addr: u16) -> u8;
      pub fn cpu_write(&mut self, addr: u16, val: u8);
      pub fn tick(&mut self);          // one CPU cycle: 3 PPU dots, plus DMA stall
  }
  impl CpuRead for Bus { ... }
  impl CpuWrite for Bus { ... }
  impl PpuTick for Bus { fn tick(&mut self) { Bus::tick(self) } }

  pub struct Controller { buttons: u8, shift: u8, strobe: bool }
  impl Controller {
      pub fn new() -> Controller;
      pub fn set_buttons(&mut self, mask: u8);
      pub fn read(&mut self) -> u8;
      pub fn write_strobe(&mut self, val: u8);
  }
  ```
  Button bit order in `set_buttons` mask: `0x01` A, `0x02` B, `0x04` Select, `0x08` Start, `0x10` Up, `0x20` Down, `0x40` Left, `0x80` Right.

- [ ] **Step 1: Write the failing OAM DMA test**

```rust
#[test]
fn writing_4014_copies_256_bytes_and_stalls_513_cycles_plus_odd_alignment() {
    let mut b = Bus::new(test_cart());
    for i in 0..256u16 { b.cpu_write(i, 0xC0 + (i & 0x0F) as u8); }
    b.write_dma_from(0x0200);
    for i in 0..256usize { assert_eq!(b.oam()[i], 0xC0 + (i & 0x0F) as u8); }
    assert_eq!(b.dma_cycles_remaining(), 513);
}

#[test]
fn an_odd_aligned_dma_write_stalls_one_cycle_longer() {
    let mut b = Bus::new(test_cart());
    b.set_cycle_parity(true);          // CPU cycle count is odd
    b.write_dma_from(0x0200);
    assert_eq!(b.dma_cycles_remaining(), 514);
}
```

`test_cart()`, `write_dma_from()`, `oam()`, `dma_cycles_remaining()`, and `set_cycle_parity()` are `#[cfg(test)]` helpers.

- [ ] **Step 2: Write the failing controller test**

```rust
#[test]
fn the_controller_shifts_one_button_per_read_after_a_strobe() {
    let mut c = Controller::new();
    c.set_buttons(0x01 | 0x80);          // A and Right held
    c.write_strobe(1);                   // load, then release
    c.write_strobe(0);
    assert_eq!(c.read() & 1, 1, "A should read first");
    assert_eq!(c.read() & 1, 0, "second button was not B");
    assert_eq!(c.read() & 1, 0);
    // ... seven more reads elide ...
    assert_eq!(c.read() & 1, 1, "Right should read eighth");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test bus:: controller:: 2>&1 | tail -20`
Expected: compile error, `Bus` and `Controller` are undefined.

- [ ] **Step 4: Implement `Controller`**

`write_strobe(1)` sets `strobe = true` and reloads `shift = buttons`; while strobe is high every read returns bit 0 of `buttons` directly. `write_strobe(0)` clears `strobe`. Each `read()` returns bit 0 of `shift` and shifts right by one; after 8 reads return 1.

- [ ] **Step 5: Implement `Bus` address decode**

`cpu_read`:
- `$0000-$1FFF` — 2 KiB RAM, mirrored every `$0800`
- `$2000-$3FFF` — `ppu.cpu_read(addr & 0x3FFF)`
- `$4016` — shift register for controller 0; `$4017` returns open bus
- `$4015` — returns 0 (APU not implemented; SMB reads this and must not fault)
- `$4020-$FFFF` — `cart.cpu_read(addr)`

`cpu_write`:
- `$0000-$1FFF` — RAM
- `$2000-$3FFF` — `ppu.cpu_write`
- `$4014` — OAM DMA from `addr`, copy 256 bytes from CPU space into OAM, set the stall counter
- `$4016` — strobe
- `$4010-$4013`, `$4015`, `$4017` — dropped, no state change
- `$8000-$FFFF` — `cart.cpu_write`

`tick()` advances the PPU three dots and, if a DMA is in flight, decrements the stall counter. Keep a running CPU cycle counter so the odd-alignment case can be honoured.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test 2>&1 | tail -20`
Expected: all tests PASS.

- [ ] **Step 7: Commit**

```bash
git add src/bus.rs src/controller.rs src/main.rs
git commit -m "feat: address decode, OAM DMA, controller, and APU stubs"
```

---

### Task 7: Wire it together and boot the real cartridge

The first task whose output is a person seeing a title screen.

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: `cargo run -- "Super Mario Bros. + Duck Hunt (USA).nes"` shows the title screen.

- [ ] **Step 1: Wire the reset sequence and frame loop**

In `main.rs`, read the ROM path from `std::env::args().nth(1)`, error out with a usage message if absent. Build `Bus::new(Cartridge::load(&bytes)?)`, call `bus.reset()`, then loop:

```rust
loop {
    // Present the previous frame's buffer if one is ready, then produce the next.
    bus.cpu.nmi(&mut bus, &mut bus, &mut bus);
    bus.cpu.step(&mut bus, &mut bus, &mut bus);
    if bus.ppu.frame_ready() {
        frame = bus.ppu.take_frame();
        bus.ppu.mark_frame_taken();
    }
}
```

Before each `step`, check `bus.ppu.nmi_pending()` and call `nmi` if set, so the interrupt is serviced at an instruction boundary rather than mid-instruction.

- [ ] **Step 2: Run it headless and dump a PNG**

Run: `cargo run -- "Super Mario Bros. + Duck Hunt (USA).nes"` with Escape wired to quit, after 120 frames write `frame` to a PNG via a `write_png` helper built from the raw buffer: each `u32` is `0x00RRGGBB`, so emit `R`, `G`, `B` per pixel into an RGB8 buffer and add the `png` crate **only as a dev-dependency for this check**, or hand-roll a PPM and convert outside the crate. Prefer converting outside the crate — no new dependency.

Run: `cargo run -- "Super Mario Bros. + Duck Hunt (USA).nes" > /tmp/frame.ppm` then convert with ImageMagick if present.

- [ ] **Step 3: Look at the screenshot and compare against the known-good shape**

Expected: the "GAME A 1 DUCK / GAME B 2 DUCKS / GAME C CLAY SHOOTING" menu with a copyright line, on a black background. Not the reference image's failure modes — no column of digits down the centre, no garbage tiles in the top-right cloud.

If the screen is blank, the bus-conflict rule in Task 2 is the first thing to re-check. If the playfield is one repeated tile, CHR RAM handling is. Do not start on PPU timing until one of those two is ruled out.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: boot the cartridge to its title screen"
```

---

### Task 8: Keyboard input and frame pacing

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Controller::set_buttons` from Task 6, the SDL2 event pump from Task 1.
- Produces: arrow keys and Z/X mapped to NES buttons, 60 fps pacing.

- [ ] **Step 1: Add key state tracking and mapping**

Hold a `let mut held: u8 = 0;` alongside the event loop and set it from these keys on `KeyDown`, clear the bit on `KeyUp` or on window focus loss:

| Key | Bit |
|-----|-----|
| Left | `0x40` |
| Right | `0x80` |
| Up | `0x10` |
| Down | `0x20` |
| Z | `0x01` (A) |
| X | `0x02` (B) |
| Enter | `0x08` (Start) |
| Shift | `0x04` (Select) |

Call `bus.controllers[0].set_buttons(held)` every frame, not only on key events — the controller latches its shift register on strobe writes during emulation, so the held mask must be current at emulation time.

- [ ] **Step 2: Add fixed-timestep pacing**

Emulate exactly `29780.5` CPU cycles per frame — 262 scanlines × 341 dots ÷ 3, rounded down — rather than sleeping per frame and hoping. Sleep to keep the frame period at 16.667 ms using `Instant::now()` and a per-frame budget. Drop accumulated frames rather than chasing them if the process falls behind, so a slow frame does not cascade.

- [ ] **Step 3: Play it**

Run: `cargo run --release -- "Super Mario Bros. + Duck Hunt (USA).nes"`
Expected: press Enter on the title screen, then reach "WORLD 1-1" and control Mario with the arrow keys and Z/X. Move, jump, stomp a goomba, bump a block, hit the flagpole.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: keyboard input and fixed-timestep frame pacing"
```

---

### Task 9: Final check against the whole cartridge

**Files:**
- none, unless a defect is found.

- [ ] **Step 1: Run the full test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: all tests PASS.

- [ ] **Step 2: Play past 1-1**

Reach the end of 1-1, walk into 1-2, and start a game of Duck Hunt by choosing Game B on the title screen. Duck Hunt exercises the other PRG bank and a different CHR upload, which is the cheapest available test that bank switching survives past the first switch.

- [ ] **Step 3: Record the remaining known gaps**

Write down, in the commit message, what is not implemented: APU, unofficial opcodes, other mappers, save states, and the fact that SMB's music will drift. These are deliberate, not oversights.
