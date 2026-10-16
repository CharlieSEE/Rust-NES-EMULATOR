//! The 2A03's CPU: a 6502 core with the NES's own deviations.
//!
//! Documented opcodes only -- SMB is compiled from them and an undocumented
//! opcode here is a cartridge fault, so `step` panics with the address rather
//! than pretending. Cycle counts are exact: one table of base counts plus the
//! two standard penalties, never baked together.
//!
//! Two deliberate ceilings:
//!
//! * No sub-instruction bus traffic. `BusLike::tick` moves the PPU on, but
//!   `read`/`write` do not advance it, so the dummy read an indexed read does
//!   on a page crossing, and the dummy write a read-modify-write does, are
//!   counted but not performed. Nothing in SMB observes them. Fix by giving
//!   `read`/`write` a tick if a fault ever says otherwise.
//! * No NMI hijack. `BusLike` can only raise an NMI between instructions, so
//!   the earliest sample point is the top of `step`; see the `nmi` comment.

// TEMPORARY: nothing calls these until the bus task exists, so without this
// `cargo build` emits 16 dead_code warnings for the whole CPU. Removed for good
// once `Bus` exists. Same as rom.rs.
#![allow(dead_code)]

/// Everything an instruction needs from the rest of the machine.
///
/// `tick` advances the PPU by **one dot**, not by one CPU cycle. The NES PPU
/// runs three dots per CPU cycle, so `step` calls `tick` three times for every
/// cycle it consumes -- SMB's sprite-0 hit depends on that ratio. A `Bus` whose
/// `tick` runs a whole CPU cycle will put the PPU three times too far ahead.
/// `Bus` implements this once (a later task).
pub trait BusLike {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);
    fn tick(&mut self);
}

/// Cycles an `nmi` or `irq` sequence costs. Both push the stack themselves and
/// **tick the bus 21 dots internally** (see `interrupt`), returning nothing --
/// so a caller must NOT tick for them, only read this constant if it needs to
/// account for the 7 cycles. Ticking on top of that double-advances vblank.
pub const INTERRUPT_CYCLES: u8 = 7;

// Processor status bits. `B` and `U` are not real registers on the 2A03: `B`
// is forced into a pushed status byte and masked out of a pulled one, and bit
// 5 always reads back as 1. Neither is stored in `p`.
const C: u8 = 0x01;
const Z: u8 = 0x02;
const I: u8 = 0x04;
const D: u8 = 0x08;
const B: u8 = 0x10;
const U: u8 = 0x20;
const V: u8 = 0x40;
const N: u8 = 0x80;

/// Addressing mode of an opcode. `Rel` is the branch offset: `branch` consumes
/// it through `operand` like any other operand.
#[derive(Clone, Copy)]
enum Mode {
    Imp,
    Acc,
    Imm,
    Zp,
    ZpX,
    ZpY,
    Abs,
    AbsX,
    AbsY,
    Ind,
    IndX,
    IndY,
    Rel,
}

/// Base cycles per opcode: the documented counts with no page-cross or
/// taken-branch penalty. Those two adjustments are added in `operand` and
/// `branch`, never baked in here, so this table is the reference a human can
/// check against a 6502 datasheet row by row. Every one of the 105
/// undocumented slots is 1 and none is reached -- `step` panics on them, and
/// nothing branches on the value, so the uniform 1 is deliberately unchecked.
#[rustfmt::skip]
const CYCLES: [u8; 256] = [
//    0    1    2    3    4    5    6    7    8    9    a    b    c    d    e    f
      7,  6,  1,  1,  1,  3,  5,  1,  3,  2,  2,  1,  1,  4,  6,  1, // 00
      2,  5,  1,  1,  1,  4,  6,  1,  2,  4,  1,  1,  1,  4,  7,  1, // 10
      6,  6,  1,  1,  3,  3,  5,  1,  4,  2,  2,  1,  4,  4,  6,  1, // 20
      2,  5,  1,  1,  1,  4,  6,  1,  2,  4,  1,  1,  1,  4,  7,  1, // 30
      6,  6,  1,  1,  1,  3,  5,  1,  3,  2,  2,  1,  3,  4,  6,  1, // 40
      2,  5,  1,  1,  1,  4,  6,  1,  2,  4,  1,  1,  1,  4,  7,  1, // 50
      6,  6,  1,  1,  1,  3,  5,  1,  4,  2,  2,  1,  5,  4,  6,  1, // 60
      2,  5,  1,  1,  1,  4,  6,  1,  2,  4,  1,  1,  1,  4,  7,  1, // 70
      1,  6,  1,  1,  3,  3,  3,  1,  2,  1,  2,  1,  4,  4,  4,  1, // 80
      2,  6,  1,  1,  4,  4,  4,  1,  2,  5,  2,  1,  1,  5,  1,  1, // 90
      2,  6,  2,  1,  3,  3,  3,  1,  2,  2,  2,  1,  4,  4,  4,  1, // a0
      2,  5,  1,  1,  4,  4,  4,  1,  2,  4,  2,  1,  4,  4,  4,  1, // b0
      2,  6,  1,  1,  3,  3,  5,  1,  2,  2,  2,  1,  4,  4,  6,  1, // c0
      2,  5,  1,  1,  1,  4,  6,  1,  2,  4,  1,  1,  1,  4,  7,  1, // d0
      2,  6,  1,  1,  3,  3,  5,  1,  2,  2,  2,  1,  4,  4,  6,  1, // e0
      2,  5,  1,  1,  1,  4,  6,  1,  2,  4,  1,  1,  1,  4,  7,  1, // f0
];

/// Addressing mode per opcode, indexed by the same opcode as `CYCLES`. `M` is
/// just a short name for `Mode`, so a row of sixteen stays on one line and the
/// table stays checkable against a datasheet by eye. Both tables carry
/// `Mode::Imp` in the 105 undocumented slots, which `step` panics on before a
/// mode is ever read.
#[rustfmt::skip]
const MODE: [Mode; 256] = [
    M::Imp,  M::IndX, M::Imp,  M::Imp,  M::Imp,  M::Zp,   M::Zp,   M::Imp,  M::Imp,  M::Imm,  M::Acc,  M::Imp,  M::Imp,  M::Abs,  M::Abs,  M::Imp, // 00
    M::Rel,  M::IndY, M::Imp,  M::Imp,  M::Imp,  M::ZpX,  M::ZpX,  M::Imp,  M::Imp,  M::AbsY, M::Imp,  M::Imp,  M::Imp,  M::AbsX, M::AbsX, M::Imp, // 10
    M::Abs,  M::IndX, M::Imp,  M::Imp,  M::Zp,   M::Zp,   M::Zp,   M::Imp,  M::Imp,  M::Imm,  M::Acc,  M::Imp,  M::AbsX, M::Abs,  M::Abs,  M::Imp, // 20
    M::Rel,  M::IndY, M::Imp,  M::Imp,  M::Imp,  M::ZpX,  M::ZpX,  M::Imp,  M::Imp,  M::AbsY, M::Imp,  M::Imp,  M::Imp,  M::AbsX, M::AbsX, M::Imp, // 30
    M::Imp,  M::IndX, M::Imp,  M::Imp,  M::Imp,  M::Zp,   M::Zp,   M::Imp,  M::Imp,  M::Imm,  M::Acc,  M::Imp,  M::Abs,  M::Abs,  M::Abs,  M::Imp, // 40
    M::Rel,  M::IndY, M::Imp,  M::Imp,  M::Imp,  M::ZpX,  M::ZpX,  M::Imp,  M::Imp,  M::AbsY, M::Imp,  M::Imp,  M::Imp,  M::AbsX, M::AbsX, M::Imp, // 50
    M::Imp,  M::IndX, M::Imp,  M::Imp,  M::Imp,  M::Zp,   M::Zp,   M::Imp,  M::Imp,  M::Imm,  M::Acc,  M::Imp,  M::Ind,  M::Abs,  M::Abs,  M::Imp, // 60
    M::Rel,  M::IndY, M::Imp,  M::Imp,  M::Imp,  M::ZpX,  M::ZpX,  M::Imp,  M::Imp,  M::AbsY, M::Imp,  M::Imp,  M::Imp,  M::AbsX, M::AbsX, M::Imp, // 70
    M::Imp,  M::IndX, M::Imp,  M::Imp,  M::Zp,   M::Zp,   M::Zp,   M::Imp,  M::Imp,  M::Imp,  M::Imp,  M::Imp,  M::Abs,  M::Abs,  M::Abs,  M::Imp, // 80
    M::Rel,  M::IndY, M::Imp,  M::Imp,  M::ZpX,  M::ZpX,  M::ZpY,  M::Imp,  M::Imp,  M::AbsY, M::Imp,  M::Imp,  M::Imp,  M::AbsX, M::Imp,  M::Imp, // 90
    M::Imm,  M::IndX, M::Imm,  M::Imp,  M::Zp,   M::Zp,   M::Zp,   M::Imp,  M::Imp,  M::Imm,  M::Imp,  M::Imp,  M::Abs,  M::Abs,  M::Abs,  M::Imp, // a0
    M::Rel,  M::IndY, M::Imp,  M::Imp,  M::ZpX,  M::ZpX,  M::ZpY,  M::Imp,  M::Imp,  M::AbsY, M::Imp,  M::Imp,  M::AbsX, M::AbsX, M::AbsY, M::Imp, // b0
    M::Imm,  M::IndX, M::Imp,  M::Imp,  M::Zp,   M::Zp,   M::Zp,   M::Imp,  M::Imp,  M::Imm,  M::Imp,  M::Imp,  M::Abs,  M::Abs,  M::Abs,  M::Imp, // c0
    M::Rel,  M::IndY, M::Imp,  M::Imp,  M::Imp,  M::ZpX,  M::ZpX,  M::Imp,  M::Imp,  M::AbsY, M::Imp,  M::Imp,  M::Imp,  M::AbsX, M::AbsX, M::Imp, // d0
    M::Imm,  M::IndX, M::Imp,  M::Imp,  M::Zp,   M::Zp,   M::Zp,   M::Imp,  M::Imp,  M::Imm,  M::Imp,  M::Imp,  M::Abs,  M::Abs,  M::Abs,  M::Imp, // e0
    M::Rel,  M::IndY, M::Imp,  M::Imp,  M::Imp,  M::ZpX,  M::ZpX,  M::Imp,  M::Imp,  M::AbsY, M::Imp,  M::Imp,  M::Imp,  M::AbsX, M::AbsX, M::Imp, // f0
];
use Mode as M;

/// Every opcode with a documented, implemented behaviour. Kept next to the
/// tests that walk it: an opcode missing from both is missing from the CPU.
#[cfg(test)]
const DOCUMENTED: &[u8] = &[
    0x00, 0x01, 0x05, 0x06, 0x08, 0x09, 0x0A, 0x0D, 0x0E, 0x10, 0x11, 0x15, 0x16, 0x18, 0x19, 0x1D,
    0x1E, 0x20, 0x21, 0x24, 0x25, 0x26, 0x28, 0x29, 0x2A, 0x2C, 0x2D, 0x2E, 0x30, 0x31, 0x35, 0x36,
    0x38, 0x39, 0x3D, 0x3E, 0x40, 0x41, 0x45, 0x46, 0x48, 0x49, 0x4A, 0x4C, 0x4D, 0x4E, 0x50, 0x51,
    0x55, 0x56, 0x58, 0x59, 0x5D, 0x5E, 0x60, 0x61, 0x65, 0x66, 0x68, 0x69, 0x6A, 0x6C, 0x6D, 0x6E,
    0x70, 0x71, 0x75, 0x76, 0x78, 0x79, 0x7D, 0x7E, 0x81, 0x84, 0x85, 0x86, 0x88, 0x8A, 0x8C, 0x8D,
    0x8E, 0x90, 0x91, 0x94, 0x95, 0x96, 0x98, 0x99, 0x9A, 0x9D, 0xA0, 0xA1, 0xA2, 0xA4, 0xA5, 0xA6,
    0xA8, 0xA9, 0xAA, 0xAC, 0xAD, 0xAE, 0xB0, 0xB1, 0xB4, 0xB5, 0xB6, 0xB8, 0xB9, 0xBA, 0xBC, 0xBD,
    0xBE, 0xC0, 0xC1, 0xC4, 0xC5, 0xC6, 0xC8, 0xC9, 0xCA, 0xCC, 0xCD, 0xCE, 0xD0, 0xD1, 0xD5, 0xD6,
    0xD8, 0xD9, 0xDD, 0xDE, 0xE0, 0xE1, 0xE4, 0xE5, 0xE6, 0xE8, 0xE9, 0xEA, 0xEC, 0xED, 0xEE, 0xF0,
    0xF1, 0xF5, 0xF6, 0xF8, 0xF9, 0xFD, 0xFE,
];

/// Base address plus an index register, and whether that crosses a page -- the
/// extra cycle an indexed *read* owes. Stores and read-modify-writes ignore it.
fn index(base: u16, reg: u8) -> (u16, bool) {
    let ea = base.wrapping_add(reg as u16);
    (ea, ea & 0xFF00 != base & 0xFF00)
}

pub struct Cpu {
    a: u8,
    x: u8,
    y: u8,
    sp: u8,
    pc: u16,
    /// Processor status, bit 5 forced high. `B` is never stored: see `B`.
    p: u8,
}

impl Cpu {
    /// A powered-off CPU. Nothing runs until `reset` reads the vector, and
    /// SMB's own reset stub sets `sp` again (`LDX #$FF; TXS`) a few bytes in,
    /// so the value here only has to be somewhere legal.
    pub fn new() -> Cpu {
        Cpu {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: 0,
            p: U,
        }
    }

    /// Power-on reset: stack pointer to $FD, interrupts masked, PC from the
    /// vector at `$FFFC`. A and X/Y are not cleared -- the 2A03 leaves them.
    /// Costs no bus ticks: the PPU is held in reset for the first 29658 cycles
    /// anyway, so nothing observable is lost by not counting them here.
    pub fn reset<B: BusLike>(&mut self, bus: &mut B) {
        self.sp = 0xFD;
        self.p = U | I;
        self.pc = self.fetch16(bus, 0xFFFC);
    }

    /// Runs one instruction and returns every cycle it consumed, penalties
    /// included, ticking the bus three times per cycle.
    pub fn step<B: BusLike>(&mut self, bus: &mut B) -> u8 {
        let op = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        let mode = MODE[op as usize];
        let mut cycles = CYCLES[op as usize];
        match op {
            // --- loads: the only reads that owe a page-cross cycle ---
            0xA9 | 0xA5 | 0xB5 | 0xAD | 0xBD | 0xB9 | 0xB1 | 0xA1 => {
                let (v, crossed) = self.fetch(bus, mode);
                self.a = v;
                self.nz(v);
                cycles += crossed as u8;
            }
            0xA2 | 0xA6 | 0xB6 | 0xAE | 0xBE => {
                let (v, crossed) = self.fetch(bus, mode);
                self.x = v;
                self.nz(v);
                cycles += crossed as u8;
            }
            0xA0 | 0xA4 | 0xB4 | 0xAC | 0xBC => {
                let (v, crossed) = self.fetch(bus, mode);
                self.y = v;
                self.nz(v);
                cycles += crossed as u8;
            }
            // --- stores: no penalty, crossing or not ---
            0x85 | 0x95 | 0x8D | 0x9D | 0x99 | 0x81 | 0x91 => {
                let ea = self.operand(bus, mode).0;
                bus.write(ea, self.a);
            }
            0x86 | 0x96 | 0x8E => {
                let ea = self.operand(bus, mode).0;
                bus.write(ea, self.x);
            }
            0x84 | 0x94 | 0x8C => {
                let ea = self.operand(bus, mode).0;
                bus.write(ea, self.y);
            }
            0xAA => {
                self.x = self.a;
                self.nz(self.x);
            }
            0xA8 => {
                self.y = self.a;
                self.nz(self.y);
            }
            0x8A => {
                self.a = self.x;
                self.nz(self.a);
            }
            0x98 => {
                self.a = self.y;
                self.nz(self.a);
            }
            0xBA => {
                self.x = self.sp;
                self.nz(self.x);
            }
            0x9A => self.sp = self.x, // TXS touches no flags
            // --- stack ---
            0x48 => self.push(bus, self.a),
            0x08 => self.push(bus, self.p | B), // PHP forces B set
            0x68 => {
                self.a = self.pull(bus);
                self.nz(self.a);
            }
            0x28 => self.p = (self.pull(bus) & !B) | U, // PLP: B is not a register
            // --- logic and arithmetic ---
            0x09 | 0x05 | 0x15 | 0x0D | 0x1D | 0x19 | 0x11 | 0x01 => {
                let (v, crossed) = self.fetch(bus, mode);
                self.a |= v;
                self.nz(self.a);
                cycles += crossed as u8;
            }
            0x29 | 0x25 | 0x35 | 0x2D | 0x3D | 0x39 | 0x31 | 0x21 => {
                let (v, crossed) = self.fetch(bus, mode);
                self.a &= v;
                self.nz(self.a);
                cycles += crossed as u8;
            }
            0x49 | 0x45 | 0x55 | 0x4D | 0x5D | 0x59 | 0x51 | 0x41 => {
                let (v, crossed) = self.fetch(bus, mode);
                self.a ^= v;
                self.nz(self.a);
                cycles += crossed as u8;
            }
            0x24 | 0x2C => {
                let (v, crossed) = self.fetch(bus, mode);
                self.set(Z, self.a & v == 0);
                self.set(N, v & 0x80 != 0);
                self.set(V, v & 0x40 != 0);
                cycles += crossed as u8;
            }
            0x69 | 0x65 | 0x75 | 0x6D | 0x7D | 0x79 | 0x71 | 0x61 => {
                let (v, crossed) = self.fetch(bus, mode);
                // The 2A03 has no decimal mode: D is a bit the game sets and
                // forgets, and ADC/SBC stay binary whatever it says.
                self.add(v);
                cycles += crossed as u8;
            }
            0xE9 | 0xE5 | 0xF5 | 0xED | 0xFD | 0xF9 | 0xF1 | 0xE1 => {
                let (v, crossed) = self.fetch(bus, mode);
                self.add(!v); // SBC is ADC with the operand inverted
                cycles += crossed as u8;
            }
            0xC9 | 0xC5 | 0xD5 | 0xCD | 0xDD | 0xD9 | 0xD1 | 0xC1 => {
                let (v, crossed) = self.fetch(bus, mode);
                self.compare(self.a, v);
                cycles += crossed as u8;
            }
            0xE0 | 0xE4 | 0xEC => {
                let (v, crossed) = self.fetch(bus, mode);
                self.compare(self.x, v);
                cycles += crossed as u8;
            }
            0xC0 | 0xC4 | 0xCC => {
                let (v, crossed) = self.fetch(bus, mode);
                self.compare(self.y, v);
                cycles += crossed as u8;
            }
            // --- increments and decrements ---
            0xE8 => {
                self.x = self.x.wrapping_add(1);
                self.nz(self.x);
            }
            0xC8 => {
                self.y = self.y.wrapping_add(1);
                self.nz(self.y);
            }
            0xCA => {
                self.x = self.x.wrapping_sub(1);
                self.nz(self.x);
            }
            0x88 => {
                self.y = self.y.wrapping_sub(1);
                self.nz(self.y);
            }
            0xE6 | 0xF6 | 0xEE | 0xFE => {
                let ea = self.operand(bus, mode).0;
                let v = bus.read(ea).wrapping_add(1);
                bus.write(ea, v);
                self.nz(v);
            }
            0xC6 | 0xD6 | 0xCE | 0xDE => {
                let ea = self.operand(bus, mode).0;
                let v = bus.read(ea).wrapping_sub(1);
                bus.write(ea, v);
                self.nz(v);
            }
            // --- shifts and rotates ---
            0x0A => self.a = self.asl(self.a),
            0x06 | 0x16 | 0x0E | 0x1E => {
                let ea = self.operand(bus, mode).0;
                let v = self.asl(bus.read(ea));
                bus.write(ea, v);
            }
            0x4A => self.a = self.lsr(self.a),
            0x46 | 0x56 | 0x4E | 0x5E => {
                let ea = self.operand(bus, mode).0;
                let v = self.lsr(bus.read(ea));
                bus.write(ea, v);
            }
            0x2A => self.a = self.rol(self.a),
            0x26 | 0x36 | 0x2E | 0x3E => {
                let ea = self.operand(bus, mode).0;
                let v = self.rol(bus.read(ea));
                bus.write(ea, v);
            }
            0x6A => self.a = self.ror(self.a),
            0x66 | 0x76 | 0x6E | 0x7E => {
                let ea = self.operand(bus, mode).0;
                let v = self.ror(bus.read(ea));
                bus.write(ea, v);
            }
            // --- jumps ---
            0x4C => self.pc = self.next16(bus),
            0x6C => {
                let ptr = self.next16(bus);
                // The NMOS 6502 does not carry out of the low byte here: a
                // vector at $xxFF reads its high byte from $xx00.
                let lo = bus.read(ptr) as u16;
                let hi = bus.read((ptr & 0xFF00) | (ptr.wrapping_add(1) & 0xFF)) as u16;
                self.pc = (hi << 8) | lo;
            }
            0x20 => {
                let target = self.next16(bus);
                self.push16(bus, self.pc.wrapping_sub(1));
                self.pc = target;
            }
            0x60 => {
                let ret = self.pull16(bus);
                self.pc = ret.wrapping_add(1);
            }
            0x40 => {
                self.p = (self.pull(bus) & !B) | U;
                self.pc = self.pull16(bus);
            }
            0x00 => {
                // BRK is two bytes: the skipped byte is its signature, and the
                // pushed PC is past it.
                self.push16(bus, self.pc.wrapping_add(1));
                self.push(bus, self.p | B);
                self.p |= I;
                self.pc = self.fetch16(bus, 0xFFFE);
            }
            // --- branches ---
            0x10 => cycles += self.branch(bus, !self.flag(N)),
            0x30 => cycles += self.branch(bus, self.flag(N)),
            0x50 => cycles += self.branch(bus, !self.flag(V)),
            0x70 => cycles += self.branch(bus, self.flag(V)),
            0x90 => cycles += self.branch(bus, !self.flag(C)),
            0xB0 => cycles += self.branch(bus, self.flag(C)),
            0xD0 => cycles += self.branch(bus, !self.flag(Z)),
            0xF0 => cycles += self.branch(bus, self.flag(Z)),
            // --- flags ---
            0x18 => self.p &= !C,
            0x38 => self.p |= C,
            0x58 => self.p &= !I,
            0x78 => self.p |= I,
            0xB8 => self.p &= !V,
            0xD8 => self.p &= !D,
            0xF8 => self.p |= D,
            0xEA => {} // NOP
            _ => panic!(
                "unimplemented opcode ${op:02X} at ${:04X}: documented-only CPU",
                self.pc.wrapping_sub(1)
            ),
        }
        for _ in 0..cycles * 3 {
            bus.tick();
        }
        cycles
    }

    /// Non-maskable interrupt: the game can't prevent it, so the PPU vblank
    /// flag reaching it gets the CPU here. Pushes PC as it stands -- the
    /// address of the next instruction -- then vectors through `$FFFA`.
    pub fn nmi<B: BusLike>(&mut self, bus: &mut B) {
        self.interrupt(bus, 0xFFFA);
    }

    /// Maskable interrupt from `$FFFE`, ignored while `I` is set. An APU that
    /// wants level-triggered IRQs has to keep asking until this lands, because
    /// a request dropped here is dropped for good.
    pub fn irq<B: BusLike>(&mut self, bus: &mut B) {
        if !self.flag(I) {
            self.interrupt(bus, 0xFFFE);
        }
    }

    fn interrupt<B: BusLike>(&mut self, bus: &mut B, vector: u16) {
        self.push16(bus, self.pc);
        self.push(bus, self.p); // B is clear in an interrupt push
        self.p |= I;
        self.pc = self.fetch16(bus, vector);
        for _ in 0..INTERRUPT_CYCLES * 3 {
            bus.tick();
        }
    }

    // --- operand and stack plumbing ---

    /// Address of the operand, consuming its bytes, and whether an indexed
    /// *read* crosses a page and so owes the extra cycle. Stores and
    /// read-modify-writes ignore the second half of the pair: their counts
    /// never move.
    fn operand<B: BusLike>(&mut self, bus: &mut B, mode: Mode) -> (u16, bool) {
        match mode {
            Mode::Imp | Mode::Acc => (self.pc, false),
            Mode::Imm | Mode::Rel => {
                let at = self.pc;
                self.pc = self.pc.wrapping_add(1);
                (at, false)
            }
            // Zero page indexes wrap inside the page and never cost extra.
            Mode::Zp => {
                let at = bus.read(self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                (at, false)
            }
            Mode::ZpX => {
                let at = (bus.read(self.pc).wrapping_add(self.x)) as u16;
                self.pc = self.pc.wrapping_add(1);
                (at, false)
            }
            Mode::ZpY => {
                let at = (bus.read(self.pc).wrapping_add(self.y)) as u16;
                self.pc = self.pc.wrapping_add(1);
                (at, false)
            }
            Mode::Abs => (self.next16(bus), false),
            Mode::AbsX => index(self.next16(bus), self.x),
            Mode::AbsY => index(self.next16(bus), self.y),
            Mode::IndX => {
                let zp = bus.read(self.pc).wrapping_add(self.x);
                self.pc = self.pc.wrapping_add(1);
                (self.fetch16(bus, zp as u16), false)
            }
            Mode::IndY => {
                // Two operand bytes: the zero-page pointer and the Y index.
                // The index is an operand byte, NOT the Y register.
                let ptr = bus.read(self.pc) as u16;
                let idx = bus.read(self.pc.wrapping_add(1));
                self.pc = self.pc.wrapping_add(2);
                index(self.fetch16(bus, ptr), idx)
            }
            Mode::Ind => (self.next16(bus), false),
        }
    }

    fn fetch<B: BusLike>(&mut self, bus: &mut B, mode: Mode) -> (u8, bool) {
        let (ea, crossed) = self.operand(bus, mode);
        (bus.read(ea), crossed)
    }

    /// Big-endian word at `at`, leaving PC alone: for vectors and for pointers
    /// that live somewhere other than the instruction stream.
    fn fetch16<B: BusLike>(&mut self, bus: &mut B, at: u16) -> u16 {
        let lo = bus.read(at) as u16;
        let hi = bus.read(at.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    /// Big-endian word as the instruction's own operand, advancing PC past it.
    fn next16<B: BusLike>(&mut self, bus: &mut B) -> u16 {
        let v = self.fetch16(bus, self.pc);
        self.pc = self.pc.wrapping_add(2);
        v
    }

    fn push<B: BusLike>(&mut self, bus: &mut B, v: u8) {
        bus.write(0x100 | self.sp as u16, v);
        self.sp = self.sp.wrapping_sub(1);
    }

    fn push16<B: BusLike>(&mut self, bus: &mut B, v: u16) {
        self.push(bus, (v >> 8) as u8);
        self.push(bus, v as u8);
    }

    fn pull<B: BusLike>(&mut self, bus: &mut B) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(0x100 | self.sp as u16)
    }

    fn pull16<B: BusLike>(&mut self, bus: &mut B) -> u16 {
        let lo = self.pull(bus) as u16;
        (self.pull(bus) as u16) << 8 | lo
    }

    /// Consumes the branch offset, moves PC when the branch is taken, and
    /// returns the cycles to add on top of the base 2 in `CYCLES`.
    fn branch<B: BusLike>(&mut self, bus: &mut B, taken: bool) -> u8 {
        let (at, _) = self.operand(bus, Mode::Rel);
        let offset = bus.read(at) as i8 as i16;
        if !taken {
            return 0;
        }
        let target = (self.pc as i16).wrapping_add(offset) as u16;
        let crossed = self.pc & 0xFF00 != target & 0xFF00;
        self.pc = target;
        Self::branch_penalty(true, crossed)
    }

    // --- ALU ---
    //
    // Every read-modify helper takes and returns the value so the same code
    // serves the accumulator forms and the memory forms.

    fn flag(&self, bit: u8) -> bool {
        self.p & bit != 0
    }

    fn set(&mut self, bit: u8, on: bool) {
        if on {
            self.p |= bit;
        } else {
            self.p &= !bit;
        }
    }

    fn nz(&mut self, v: u8) {
        self.set(Z, v == 0);
        self.set(N, v & 0x80 != 0);
    }

    fn compare(&mut self, reg: u8, v: u8) {
        self.set(C, reg >= v);
        self.nz(reg.wrapping_sub(v));
    }

    fn add(&mut self, v: u8) {
        let a = self.a;
        let sum = a as u16 + v as u16 + u16::from(self.flag(C));
        let r = sum as u8;
        self.set(C, sum > 0xFF);
        // Overflow: the operands agreed in sign and the result disagreed.
        self.set(V, (a ^ v) & 0x80 == 0 && (a ^ r) & 0x80 != 0);
        self.a = r;
        self.nz(r);
    }

    fn asl(&mut self, v: u8) -> u8 {
        self.set(C, v & 0x80 != 0);
        let r = v << 1;
        self.nz(r);
        r
    }

    fn lsr(&mut self, v: u8) -> u8 {
        self.set(C, v & 1 != 0);
        let r = v >> 1;
        self.nz(r);
        r
    }

    fn rol(&mut self, v: u8) -> u8 {
        let carry = self.flag(C);
        self.set(C, v & 0x80 != 0);
        let r = (v << 1) | u8::from(carry);
        self.nz(r);
        r
    }

    fn ror(&mut self, v: u8) -> u8 {
        let carry = self.flag(C);
        self.set(C, v & 1 != 0);
        let r = (v >> 1) | (u8::from(carry) << 7);
        self.nz(r);
        r
    }

    /// Cycles a branch costs on top of the base 2 every branch already has.
    /// Shared with the test that pins 2 and 3.
    fn branch_penalty(taken: bool, crossed: bool) -> u8 {
        u8::from(taken) + u8::from(crossed)
    }

    #[cfg(test)]
    fn take_branch_cycles(&self, taken: bool) -> u8 {
        2 + Self::branch_penalty(taken, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat 64 KiB of RAM and a dot counter: the only bus this task has, and
    /// the same three calls `Bus` will make later.
    struct TestBus {
        ram: [u8; 65536],
        dots: u32,
    }

    impl TestBus {
        fn new() -> TestBus {
            TestBus {
                ram: [0; 65536],
                dots: 0,
            }
        }
    }

    impl BusLike for TestBus {
        fn read(&mut self, addr: u16) -> u8 {
            self.ram[addr as usize]
        }
        fn write(&mut self, addr: u16, val: u8) {
            self.ram[addr as usize] = val;
        }
        fn tick(&mut self) {
            self.dots += 1;
        }
    }

    fn load(bus: &mut TestBus, at: u16, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            bus.write(at + i as u16, *b);
        }
    }

    /// Where `cycles*` puts a program. $12FE and not $0000, because
    /// `LDA ($12FF),Y` reads its pointer back out of the zero page, and at
    /// $12FE the instruction's own operand bytes ARE that pointer -- so the
    /// page-crossing case needs no extra memory setup.
    const PROG: u16 = 0x12FE;

    /// The zero page at `$00FF`/`$0100` holds `$01F0`, so the brief's
    /// `LDA ($12FF),Y` case -- whose second operand byte is the index $12 --
    /// is the page crossing it claims to be: `$01F0 + $12 = $0202`. Without it
    /// a zero-page pointer of zeros never crosses and the test would be
    /// measuring nothing.
    const ZP_PTR: u16 = 0x00FF;
    const ZP_PTR_VALUE: [u8; 2] = [0xF0, 0x01];

    fn cycles_at(x: u8, y: u8, program: &[u8]) -> u8 {
        let mut bus = TestBus::new();
        load(&mut bus, ZP_PTR, &ZP_PTR_VALUE);
        load(&mut bus, PROG, program);
        let mut c = Cpu::new();
        c.pc = PROG;
        c.x = x;
        c.y = y;
        c.step(&mut bus)
    }

    /// Cycles taken by the first instruction of `program`, with X and Y at 1:
    /// the indexed cases only cross a page with a non-zero index, and the
    /// "no penalty" test sets its own.
    fn cycles(program: &[u8]) -> u8 {
        cycles_at(1, 1, program)
    }

    #[test]
    fn documented_opcode_cycle_counts() {
        // immediate
        assert_eq!(cycles(b"\xA9\x00"), 2); // LDA #$00
                                            // absolute read
        assert_eq!(cycles(b"\xAD\x34\x12"), 4); // LDA $1234
                                                // absolute,X with a page crossing costs one more
        assert_eq!(cycles(b"\xBD\xFF\x12"), 5); // LDA $12FF,X
                                                // indirect,Y with a page crossing costs one more
                                                // The brief pairs these bytes with 6, but B9 is LDA abs,Y: 4 base, 5
                                                // crossed, and 6 is unreachable for that opcode. B1 is the indirect,Y
                                                // form its comment describes and it takes the brief's 6; both run, so
                                                // neither reading of the case is lost.
        assert_eq!(cycles(b"\xB1\xFF\x12"), 6); // LDA ($FF),Y: $01F0 + $12
        assert_eq!(cycles(b"\xB9\xFF\x12"), 5); // LDA $12FF,Y -> $1300
                                                // stores always take the base count, crossing or not
        assert_eq!(cycles(b"\x9D\xFF\x12"), 5); // STA $12FF,X
                                                // control flow
        assert_eq!(cycles(b"\x4C\x00\x80"), 3); // JMP $8000
        assert_eq!(cycles(b"\x20\x00\x80"), 6); // JSR $8000
        assert_eq!(cycles(b"\x60"), 6); // RTS
        assert_eq!(cycles(b"\x48"), 3); // PHA
        assert_eq!(cycles(b"\xEA"), 2); // NOP
    }

    #[test]
    fn a_taken_branch_costs_one_more_than_a_not_taken_one() {
        let mut bus = TestBus::new();
        load(&mut bus, 0x1000, &[0xD0, 0x02, 0xEA]); // BNE +2, then NOP
        let mut c = Cpu::new();
        c.pc = 0x1000;
        assert_eq!(c.take_branch_cycles(false), 2);
        assert_eq!(c.take_branch_cycles(true), 3);
    }

    #[test]
    fn a_branch_costs_two_cycles_unless_it_is_taken() {
        let mut bus = TestBus::new();
        load(&mut bus, 0x1000, &[0xD0, 0x02, 0xEA]);
        let mut c = Cpu::new();
        c.pc = 0x1000;
        c.p = Z; // Z set: BNE is not taken
        assert_eq!(c.step(&mut bus), 2);
        assert_eq!(c.pc, 0x1002, "not taken: PC is past the operand only");

        let mut bus = TestBus::new();
        load(&mut bus, 0x1000, &[0xD0, 0x02, 0xEA]);
        let mut c = Cpu::new();
        c.pc = 0x1000;
        assert_eq!(c.step(&mut bus), 3);
        assert_eq!(c.pc, 0x1004);
    }

    #[test]
    fn a_taken_branch_that_crosses_a_page_costs_one_more_again() {
        let mut bus = TestBus::new();
        load(&mut bus, 0x10F0, &[0xD0, 0x7F]); // $10F2 + 127 = $1171
        let mut c = Cpu::new();
        c.pc = 0x10F0;
        assert_eq!(c.step(&mut bus), 4);
        assert_eq!(c.pc, 0x1171);
    }

    #[test]
    fn indirect_y_indexes_with_the_operand_byte_not_the_y_register() {
        // ($12),Y spends three bytes and takes the Y byte as an immediate
        // operand, not from the register. Both are wired up here, so a CPU
        // that reads the register instead lands somewhere else entirely.
        let mut bus = TestBus::new();
        load(&mut bus, 0x2000, &[0xB1, 0x00, 0x03]); // LDA ($00),Y
        bus.write(0x0000, 0x00); // the zero-page pointer: $0000
        bus.write(0x0001, 0x00);
        bus.write(0x0003, 0x11); // $0000 + $03, the operand byte
        bus.write(0x0030, 0x22); // $0000 + $30, what Y would have fetched
        let mut c = Cpu::new();
        c.pc = 0x2000;
        c.y = 0x30;
        assert_eq!(c.step(&mut bus), 5);
        assert_eq!(c.a, 0x11, "the index came from the operand byte");
        assert_eq!(c.pc, 0x2003, "three bytes consumed");
    }

    #[test]
    fn an_index_that_does_not_cross_a_page_gets_no_penalty() {
        assert_eq!(cycles(b"\xBD\xFE\x12"), 4); // LDA $12FE,X -> $12FF
        assert_eq!(cycles_at(0, 0, b"\xBD\xFF\x12"), 4); // X = 0 never crosses
                                                         // (zp),Y: pointer $0000, index $01 -> $0001, still page $00.
        assert_eq!(cycles_at(0, 0, b"\xB1\x00\x01"), 5);
    }

    /// Runs one `ADC #v` / `SBC #v` and returns the flags it left behind as
    /// `(C, V, N, Z, A)` -- the whole status byte, so a test that is only after
    /// V still catches a fix that broke a neighbour.
    fn alu(op: u8, a: u8, carry: bool, v: u8) -> (bool, bool, bool, bool, u8) {
        let mut bus = TestBus::new();
        load(&mut bus, PROG, &[op, v]);
        let mut c = Cpu::new();
        c.pc = PROG;
        c.a = a;
        c.p = if carry { C } else { 0 };
        assert_eq!(c.step(&mut bus), 2, "both forms are immediate");
        (c.flag(C), c.flag(V), c.flag(N), c.flag(Z), c.a)
    }

    #[test]
    fn adc_sets_overflow_when_two_positives_go_negative() {
        // V = the operands agreed in sign and the result did not.
        for (a, v, carry, expect_v) in [
            (0x00, 0x7F, true, true),   // $00 + $7F + 1 = $80, out of range
            (0x00, 0x80, false, false), // $00 + $80 = $80, operands disagreed
            (0x50, 0x50, false, true),  // $50 + $50 = $A0
            (0x50, 0x10, false, false), // $50 + $10 = $60, agreed all the way
            (0x7F, 0x01, false, true),  // $7F + $01 = $80
            (0x80, 0x80, true, true),   // $80 + $80 + 1 = $01, out of range
            (0x80, 0xFF, true, false),  // $80 + $FF + 1 = $80, agreed all the way
            (0x80, 0x01, false, false), // $80 + $01 = $81, agreed
        ] {
            let (c, got_v, n, z, r) = alu(0x69, a, carry, v);
            assert_eq!(got_v, expect_v, "ADC #${v:02X} with A=${a:02X}, C={carry}");
            // The other flags must not move as a side effect of the fix.
            assert_eq!(
                c,
                (a as u16 + v as u16 + u16::from(carry)) > 0xFF,
                "C after ADC #${v:02X} with A=${a:02X}, C={carry}"
            );
            assert_eq!(n, r & 0x80 != 0, "N after ADC #${v:02X} with A=${a:02X}");
            assert_eq!(z, r == 0, "Z after ADC #${v:02X} with A=${a:02X}");
        }
    }

    #[test]
    fn sbc_sets_overflow_when_two_negatives_go_positive() {
        for (a, v, carry, expect_v) in [
            (0x00, 0x00, false, false), // $00 - $00 - 1 = $FF, operands disagreed
            (0x00, 0x00, true, false),  // $00 - $00 = $00, disagreed
            (0x80, 0x7F, true, true),   // $80 - $7F = $01, the mirror of $00 + $7F
            (0x80, 0x7F, false, true),  // $80 - $7F - 1 = $00, still underflowed
            (0xA0, 0x70, true, true),   // $A0 - $70 = $30
            (0x50, 0x10, true, false),  // $50 - $10 = $40
            (0x50, 0x30, true, false),  // $50 - $30 = $20, agreed all the way
            (0x80, 0xFF, true, false),  // $80 - $FF = $81, operands disagreed
        ] {
            let (c, got_v, n, z, r) = alu(0xE9, a, carry, v);
            assert_eq!(got_v, expect_v, "SBC #${v:02X} with A=${a:02X}, C={carry}");
            let sum = a as u16 + (!v) as u16 + u16::from(carry);
            assert_eq!(
                c,
                sum > 0xFF,
                "C after SBC #${v:02X} with A=${a:02X}, C={carry}"
            );
            assert_eq!(n, r & 0x80 != 0, "N after SBC #${v:02X} with A=${a:02X}");
            assert_eq!(z, r == 0, "Z after SBC #${v:02X} with A=${a:02X}");
        }
    }

    #[test]
    fn bit_abs_x_owes_the_page_crossing_read_its_penalty() {
        // $2C is an indexed read like any other: $12FF + $01 costs one more
        // than $1200 + $01 does. $24 (zero page) can never cross, so it is
        // covered by the base-count test and stays at 3.
        assert_eq!(cycles(b"\x2C\xFF\x12"), 5); // BIT $12FF,X -> $1300
        assert_eq!(cycles(b"\x2C\x00\x12"), 4); // BIT $1200,X -> $1201
        assert_eq!(cycles(b"\x24\x00"), 3); // BIT $00: zero page wraps, never crosses
    }

    #[test]
    fn and_immediate_consumes_two_bytes_and_two_cycles() {
        // $29's mode is the byte after it. A `MODE[$29]` of `Acc` or `Imp` makes
        // `operand` hand the instruction stream back unadvanced, so the operand
        // is then executed as the next opcode and the stream desyncs.
        let mut bus = TestBus::new();
        load(&mut bus, 0x2000, &[0x29, 0x3C, 0xEA]); // AND #$3C, then a NOP
        let mut c = Cpu::new();
        c.pc = 0x2000;
        c.a = 0xFF; // so the AND is visible: $FF & $3C = $3C
        assert_eq!(c.step(&mut bus), 2);
        assert_eq!(c.pc, 0x2002, "PC is past the operand");
        assert_eq!(c.a, 0x3C);
    }

    #[test]
    fn every_documented_immediate_opcode_consumes_two_bytes() {
        // $29 was one cell of a family. Every documented immediate opcode spends
        // its operand byte and costs two cycles.
        for op in [
            0x09, 0x29, 0x49, 0x69, 0xE9, 0xC9, // ORA AND EOR ADC SBC CMP
            0xA9, 0xA2, 0xA0, // LDA LDX LDY
            0xE0, 0xC0, // CPX CPY
        ] {
            let mut bus = TestBus::new();
            load(&mut bus, 0x2000, &[op, 0x5A, 0xEA]);
            let mut c = Cpu::new();
            c.pc = 0x2000;
            assert_eq!(c.step(&mut bus), 2, "opcode ${op:02X}");
            assert_eq!(c.pc, 0x2002, "opcode ${op:02X} left the operand behind");
        }
    }

    #[test]
    fn and_absolute_costs_four_cycles_and_three_bytes() {
        let mut bus = TestBus::new();
        load(&mut bus, 0x2000, &[0x2D, 0x00, 0x03, 0xEA]); // AND $0300, then a NOP
        bus.write(0x0300, 0x5A);
        let mut c = Cpu::new();
        c.pc = 0x2000;
        c.a = 0xFF; // so the AND is visible: $FF & $5A = $5A
        assert_eq!(c.step(&mut bus), 4);
        assert_eq!(c.pc, 0x2003, "PC is past both operand bytes");
        assert_eq!(c.a, 0x5A);
    }

    #[test]
    fn three_dots_are_ticked_for_every_cycle_consumed() {
        let mut bus = TestBus::new();
        load(&mut bus, PROG, b"\xEA");
        let mut c = Cpu::new();
        c.pc = PROG;
        assert_eq!(c.step(&mut bus), 2);
        assert_eq!(bus.dots, 6);
    }

    #[test]
    fn jsr_pushes_the_return_address_that_rts_pops_back() {
        let mut bus = TestBus::new();
        load(&mut bus, 0x8000, &[0x20, 0x00, 0x90]); // JSR $9000
        load(&mut bus, 0x9000, &[0x60]); // ...where an RTS waits
        let mut c = Cpu::new();
        c.pc = 0x8000;
        c.sp = 0xFD;
        assert_eq!(c.step(&mut bus), 6);
        assert_eq!(c.pc, 0x9000);
        assert_eq!(c.sp, 0xFB, "two bytes went down");
        assert_eq!(c.step(&mut bus), 6); // RTS
        assert_eq!(c.pc, 0x8003, "JSR pushed PC-1, RTS adds one back");
        assert_eq!(c.sp, 0xFD);
    }

    #[test]
    fn reset_reads_the_vector_from_fffc() {
        let mut bus = TestBus::new();
        bus.write(0xFFFC, 0x34);
        bus.write(0xFFFD, 0x12);
        let mut c = Cpu::new();
        c.reset(&mut bus);
        assert_eq!(c.pc, 0x1234);
    }

    #[test]
    fn reset_clears_the_stack_and_masks_interrupts() {
        let mut bus = TestBus::new();
        load(&mut bus, 0xFFFC, &[0x00, 0x80]); // RESET = $8000 on this cart
        let mut c = Cpu::new();
        c.sp = 0x00;
        c.reset(&mut bus);
        assert_eq!(c.sp, 0xFD);
        assert!(c.flag(I), "I is set out of reset");
        assert_eq!(c.pc, 0x8000);
    }

    #[test]
    fn nmi_pushes_pc_and_status_then_vectors() {
        let mut bus = TestBus::new();
        bus.write(0xFFFA, 0x86);
        bus.write(0xFFFB, 0xC0); // SMB's NMI handler
        let mut c = Cpu::new();
        c.pc = 0x1234;
        c.sp = 0xFD;
        c.nmi(&mut bus);
        assert_eq!(c.pc, 0xC086);
        assert_eq!(c.sp, 0xFA, "three bytes went down");
        assert_eq!(bus.read(0x01FC), 0x34, "PC low, pushed first after PC high");
        assert_eq!(bus.read(0x01FD), 0x12, "PC high");
        assert_eq!(bus.read(0x01FB) & B, 0, "an interrupt pushes B clear");
        assert!(c.flag(I));
        assert_eq!(bus.dots, INTERRUPT_CYCLES as u32 * 3);
    }

    #[test]
    fn irq_is_ignored_while_the_interrupt_flag_is_set() {
        let mut bus = TestBus::new();
        bus.write(0xFFFE, 0x00);
        bus.write(0xFFFF, 0xC0);
        let mut c = Cpu::new();
        c.p = I;
        c.irq(&mut bus);
        assert_eq!(c.pc, 0x0000, "the IRQ vector was not taken");
        assert_eq!(bus.dots, 0, "and no cycles were consumed");
    }

    #[test]
    fn a_clear_i_flag_lets_irq_through() {
        let mut bus = TestBus::new();
        bus.write(0xFFFE, 0x00);
        bus.write(0xFFFF, 0xC0);
        let mut c = Cpu::new();
        c.sp = 0xFD;
        c.irq(&mut bus);
        assert_eq!(c.pc, 0xC000);
        assert_eq!(c.sp, 0xFA);
        assert!(c.flag(I));
        assert_eq!(bus.dots, 21);
    }

    /// Every opcode this CPU claims to implement runs, and takes exactly the
    /// base count in `CYCLES`. Zeroed registers mean no index crosses a page,
    /// so the base count is the whole answer -- except for the branches, where
    /// no single status byte leaves all eight untaken (BEQ wants Z, BNE wants
    /// it clear). Those are pinned by running them, in the branch tests.
    #[test]
    fn every_documented_opcode_runs_at_its_base_cycle_count() {
        for &op in DOCUMENTED {
            if matches!(op, 0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0) {
                continue;
            }
            let mut bus = TestBus::new();
            load(&mut bus, PROG, &[op]);
            let mut c = Cpu::new();
            c.pc = PROG;
            assert_eq!(c.step(&mut bus), CYCLES[op as usize], "opcode ${op:02X}");
        }
    }

    #[test]
    #[should_panic(expected = "unimplemented opcode $02")]
    fn an_undocumented_opcode_stops_the_cpu() {
        let mut bus = TestBus::new();
        load(&mut bus, PROG, &[0x02]); // a NOP on the NMOS 6502, undocumented
        let mut c = Cpu::new();
        c.pc = PROG;
        c.step(&mut bus);
    }
}
