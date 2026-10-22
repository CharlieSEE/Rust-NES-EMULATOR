//! nestest, the CPU's golden-log test: run every instruction from `$C000` and
//! compare PC, A, X, Y, P, SP and the cycle count against kevtris's log.
//!
//! The CPU runs on a minimal NROM bus rather than `Bus`: the golden log has no
//! PPU or APU in it, and a frame IRQ or vblank NMI would move a register the
//! log expects to stay put.

use crate::cpu::{BusLike, Cpu};
use crate::rom::{test_rom, Cartridge};

/// The 2 KiB of CPU RAM plus the cartridge. Everything else in nestest's map is
/// unused, so reads return open-bus zero and writes are dropped.
struct CpuBus {
    ram: [u8; 2048],
    cart: Cartridge,
}

impl BusLike for CpuBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[addr as usize & 0x07FF],
            0x8000..=0xFFFF => self.cart.cpu_read(addr),
            _ => 0,
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        if let 0x0000..=0x1FFF = addr {
            self.ram[addr as usize & 0x07FF] = val;
        }
    }

    fn tick(&mut self) {}
}

/// One parsed line of the golden log. Only the fields the CPU owns are kept;
/// the disassembly and PPU column are ignored.
#[derive(Debug, PartialEq)]
struct Line {
    pc: u16,
    a: u8,
    x: u8,
    y: u8,
    p: u8,
    sp: u8,
    cycles: u64,
}

fn hex_u8(s: &str) -> u8 {
    u8::from_str_radix(s, 16).expect("hex byte")
}

fn parse(line: &str) -> Option<Line> {
    let pc = u16::from_str_radix(line.split_whitespace().next()?, 16).ok()?;
    let mut out = Line {
        pc,
        a: 0,
        x: 0,
        y: 0,
        p: 0,
        sp: 0,
        cycles: 0,
    };
    for token in line.split_whitespace() {
        if let Some(v) = token.strip_prefix("A:") {
            out.a = hex_u8(v);
        } else if let Some(v) = token.strip_prefix("X:") {
            out.x = hex_u8(v);
        } else if let Some(v) = token.strip_prefix("Y:") {
            out.y = hex_u8(v);
        } else if let Some(v) = token.strip_prefix("P:") {
            out.p = hex_u8(v);
        } else if let Some(v) = token.strip_prefix("SP:") {
            out.sp = hex_u8(v);
        } else if let Some(v) = token.strip_prefix("CYC:") {
            out.cycles = v.parse().ok()?;
        }
    }
    Some(out)
}

#[test]
fn nestest_matches_the_golden_log_from_c000_to_the_end() {
    let Some(rom) = test_rom("test_roms/nestest/nestest.nes") else {
        return;
    };
    let log = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test_roms/nestest/nestest.log"
    ))
    .expect("nestest.log ships beside the ROM");

    let mut bus = CpuBus {
        ram: [0; 2048],
        cart: Cartridge::load(&rom).expect("nestest.nes must load"),
    };
    let mut cpu = Cpu::new();
    cpu.set_registers_for_test(0xC000, 0xFD, 0x24);

    // The first log line is the state before `$C000` runs, with reset's seven
    // cycles already on the hardware clock.
    let mut cycles = 7u64;
    let mut checked = 0usize;
    for (number, line) in log.lines().enumerate() {
        checked += 1;
        let want =
            parse(line).unwrap_or_else(|| panic!("line {}: cannot parse {line:?}", number + 1));
        let (pc, a, x, y, p, sp) = cpu.registers();
        assert_eq!(
            (pc, a, x, y, p, sp),
            (want.pc, want.a, want.x, want.y, want.p, want.sp),
            "line {} (${:04X}), before {:?}",
            number + 1,
            want.pc,
            line
        );
        assert_eq!(cycles, want.cycles, "line {} cycle count", number + 1);
        // The core deliberately stops at documented opcodes; the golden log
        // switches to kevtris's `*NOP`-style illegal opcodes here. The state
        // before that instruction is still part of the trace and was checked
        // above, but stepping into it would panic by design.
        if line.split_whitespace().any(|t| t.starts_with('*')) {
            break;
        }
        cycles += cpu.step(&mut bus) as u64;
    }
    // The shipped log has 5003 documented lines before the first illegal one.
    // An empty or truncated file must fail instead of finishing the loop with
    // nothing checked, which is exactly what a vacuous pass looked like.
    assert!(
        checked >= 5_000,
        "only {checked} lines were checked; nestest.log is incomplete"
    );
}
