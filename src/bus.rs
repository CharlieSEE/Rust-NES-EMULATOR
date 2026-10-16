//! The CPU address bus: RAM, PPU registers, controllers, OAM DMA, cartridge.
//!
//! References, all fetched 2026-10-16:
//! - NESdev "CPU memory map" -- the RAM mirrors every `$0800`, the PPU register
//!   mirrors every 8 bytes, `$4000-$4017` being the APU/IO window, and
//!   `$4020-$FFFF` being unmapped-but-for-the-cartridge.
//! - NESdev "PPU programmer reference", OAMDMA -- "the above procedure takes 513
//!   CPU cycles (+1 on odd CPU cycles): first one (or two) idle cycles, and
//!   then 256 pairs of alternating read/write cycles". NESdev "Cycle reference
//!   chart" puts it the same way: "OAM DMA 513 (+1 if starting on CPU get
//!   cycles)".
//! - NESdev "DMA" -- the copy "begins on the cycle immediately after the
//!   write", from a page-aligned source, through `$2004`.
//! - NESdev "Standard controller" -- `$4016` strobes both ports, `$4016`/`$4017`
//!   report controller 1 / controller 2.
//!
//! Two deliberate ceilings:
//!
//! * OAM DMA's stall is COUNTED but NOT ENFORCED. `dma_stall` is only ever
//!   decremented by `tick`; it never becomes extra `ppu.tick()` calls and it
//!   never stops the CPU. So a DMA makes neither clock skip ahead: the CPU
//!   and the PPU are both short by the same 513 (or 514) cycles. The 3:1
//!   dot:cycle ratio and every PPU-versus-CPU-instruction relationship
//!   therefore survive intact, and the cost is a bounded timing skew of
//!   513 of 29 780 cycles per frame -- 89342 dots / 3, so ~1.7% -- not a
//!   desync. SMB's vblank `BIT $2002` poll loop is self-correcting and lands
//!   within one poll iteration of the dot hardware would stop on. Enforce it
//!   when a timing-sensitive case shows: Task 7 gates the CPU on
//!   `dma_stall > 0`.
//! * Unmapped and write-only reads return 0 where hardware returns open bus,
//!   matching the PPU's own choice for `$2002` bits 4-0.

use crate::controller::Controller;
use crate::cpu::BusLike;
use crate::ppu::Ppu;
use crate::rom::Cartridge;

/// CPU cycles an OAM DMA costs on an even cycle: one dummy read/write cycle,
/// 256 read cycles and 256 write cycles (NESdev "PPU programmer reference").
const DMA_STALL: u16 = 513;
/// ...and one more when the write landed on an odd CPU cycle (same page).
const DMA_STALL_ODD: u16 = DMA_STALL + 1;

pub struct Bus {
    pub cart: Cartridge,
    pub cpu_ram: [u8; 2048],
    pub ppu: Ppu,
    pub controllers: [Controller; 2],
    /// CPU cycles the bus has been ticked for. Only its parity is load-bearing:
    /// it decides whether the next `$4014` write costs 513 or 514 cycles.
    cycles: u64,
    /// Dots into the current CPU cycle, 0..3. `tick` is one dot and the DMA
    /// stall is counted in whole cycles, so the conversion lives here.
    dot: u8,
    /// CPU cycles the last OAM DMA cost on hardware. Counted and drained, but
    /// never enforced -- see the module header.
    dma_stall: u16,
}

impl Bus {
    pub fn new(cart: Cartridge) -> Bus {
        Bus {
            cart,
            cpu_ram: [0; 2048],
            ppu: Ppu::new(),
            controllers: [Controller::new(), Controller::new()],
            cycles: 0,
            dot: 0,
            dma_stall: 0,
        }
    }

    /// Power-on state. The 2A03's RAM comes up undefined and the game's own
    /// init code fills everything it reads, so zeroing it is enough; the cycle
    /// count restarts at an even cycle, which is the alignment `$4014` counts
    /// from.
    pub fn reset(&mut self) {
        self.cpu_ram = [0; 2048];
        self.cycles = 0;
        self.dot = 0;
        self.dma_stall = 0;
    }

    pub fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            // "$0000-$07FF ... $0800-$0FFF, $1000-$17FF, $1800-$1FFF: mirrors of
            // $0000-$07FF" (NESdev "CPU memory map").
            0x0000..=0x1FFF => self.cpu_ram[addr as usize & 0x07FF],
            // "$2000-$2007 ... $2008-$3FFF: mirrors of $2000-$2007 (repeats
            // every 8 bytes)"; the PPU does the `& 7` itself.
            0x2000..=0x3FFF => self.ppu.cpu_read(addr),
            0x4015 => 0, // APU status: no APU, so no channels and no frame IRQ
            0x4016 => self.controllers[0].read(),
            0x4017 => self.controllers[1].read(),
            // Mapper 66 maps nothing below $8000: no PRG RAM, no registers, so
            // `$4020-$7FFF` is open bus.
            0x8000..=0xFFFF => self.cart.cpu_read(addr),
            _ => 0,
        }
    }

    pub fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x1FFF => self.cpu_ram[addr as usize & 0x07FF] = val,
            0x2000..=0x3FFF => self.ppu.cpu_write(addr, val),
            0x4014 => self.oam_dma(val),
            // One `$4016` write latches BOTH ports: "Though both are polled from
            // a write to $4016, controller 1 is read through $4016, and
            // controller 2 is separately read through $4017" (NESdev
            // "Standard controller").
            0x4016 => {
                for c in &mut self.controllers {
                    c.write_strobe(val);
                }
            }
            // `$4010-$4013`, `$4015` and `$4017` are the APU, and there is no
            // APU; `$4020-$7FFF` is mapper 66's unmapped window. Both drop.
            0x8000..=0xFFFF => self.cart.cpu_write(addr, val),
            _ => {}
        }
    }

    /// Whether an OAM DMA is mid-stall, i.e. whether the CPU must not execute
    /// an instruction. `tick` is what drains it.
    pub fn dma_busy(&self) -> bool {
        self.dma_stall > 0
    }

    /// Advance the PPU by **one dot**. `Cpu::step` calls `tick` three times per
    /// cycle it consumes (see `BusLike`), and SMB's sprite-0 hit depends on
    /// that ratio, so nothing here may add a factor of three.
    pub fn tick(&mut self) {
        self.ppu.tick();
        self.dot += 1;
        if self.dot < 3 {
            return;
        }
        self.dot = 0;
        self.cycles += 1;
        if self.dma_stall > 0 {
            self.dma_stall -= 1;
        }
    }

    /// OAM DMA: copy `$XX00-$XXFF` into OAM through `$2004`, and record the
    /// stall `$4014` costs on hardware. Nothing waits on that stall yet.
    ///
    /// The copy goes in through the PPU's existing registers -- `$2003` to aim,
    /// `$2004` to write and advance -- so no OAM API is added to the PPU, and
    /// it ends with `oam_addr` back at 0 after wrapping, as on hardware.
    ///
    /// Every source byte is read back through `cpu_read`, so a DMA out of
    /// `$20xx` samples PPU registers and one out of `$40xx` the APU window.
    fn oam_dma(&mut self, page: u8) {
        let base = (page as u16) << 8;
        self.ppu.cpu_write(0x2003, 0);
        for offset in 0..256u16 {
            let byte = self.cpu_read(base + offset);
            self.ppu.cpu_write(0x2004, byte);
        }
        self.dma_stall = if self.cycles & 1 == 1 {
            DMA_STALL_ODD
        } else {
            DMA_STALL
        };
    }
}

impl BusLike for Bus {
    fn read(&mut self, addr: u16) -> u8 {
        self.cpu_read(addr)
    }

    fn write(&mut self, addr: u16, val: u8) {
        self.cpu_write(addr, val)
    }

    fn tick(&mut self) {
        Bus::tick(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::Cpu;

    const BANK: usize = 0x4000;
    const POWER_ON_BANK: usize = 2;

    /// 16-byte iNES header: mapper 66, vertical mirroring, 2x8 KiB of CHR ROM
    /// claimed by a board that actually carries 8 KiB of CHR RAM.
    fn ines_header() -> [u8; 16] {
        let mut h = [0u8; 16];
        h[..4].copy_from_slice(b"NES\x1a");
        h[4] = 4; // 64 KiB PRG, four 16 KiB banks
        h[5] = 2;
        h[6] = 0x21;
        h[7] = 0x48;
        h
    }

    /// A cartridge whose power-on bank holds `code` at `$8000` and whose RESET
    /// vector points there, so `Cpu::reset` lands on real code.
    fn cart_running(code: &[u8]) -> Cartridge {
        let mut bytes = ines_header().to_vec();
        for fill in 0..4u8 {
            bytes.extend(std::iter::repeat_n(fill, BANK));
        }
        let at = 16 + POWER_ON_BANK * BANK;
        bytes[at..at + code.len()].copy_from_slice(code);
        // The vectors live in the FIXED window, i.e. the last bank, not in the
        // power-on bank: `Cpu::reset` reads $FFFC through `cpu_read`, and
        // `Cartridge` pins $C000-$FFFF to the last bank.
        let fixed = 16 + 3 * BANK + (0xFFFC & 0x3FFF);
        bytes[fixed..fixed + 2].copy_from_slice(&0x8000u16.to_le_bytes());
        Cartridge::load(&bytes).expect("synthetic image must load")
    }

    fn blank_cart() -> Cartridge {
        cart_running(&[])
    }

    /// Same image, except the power-on bank's `$8000` byte is patched to 1. Each
    /// bank is filled with its own index, so writing that 1 matches the ROM and
    /// banks `1 % 4 = 1` -- a bank that reads 1 at `$8001` instead of 2. A bus
    /// that drops cartridge writes leaves the bank at 2.
    fn cart_bankable_at_8000() -> Cartridge {
        let mut bytes = ines_header().to_vec();
        for fill in 0..4u8 {
            bytes.extend(std::iter::repeat_n(fill, BANK));
        }
        bytes[16 + POWER_ON_BANK * BANK] = 1;
        let fixed = 16 + 3 * BANK + (0xFFFC & 0x3FFF);
        bytes[fixed..fixed + 2].copy_from_slice(&0x8000u16.to_le_bytes());
        Cartridge::load(&bytes).expect("synthetic image must load")
    }

    impl Bus {
        /// `$4014` the way the game does it: only the high byte of the source
        /// address reaches the register, so a low byte here is ignored and the
        /// copy is page aligned.
        fn write_dma_from(&mut self, addr: u16) {
            self.cpu_write(0x4014, (addr >> 8) as u8);
        }

        /// OAM as it stands, read back through the PPU's own registers: `$2003`
        /// selects the byte and `$2004` returns it. `$2004` does not advance
        /// `OAMADDR` on hardware -- only a *write* does -- so this helper has
        /// to walk `OAMADDR` itself, and restores it afterwards.
        fn oam(&mut self) -> [u8; 256] {
            let mut oam = [0u8; 256];
            for (i, byte) in oam.iter_mut().enumerate() {
                self.ppu.cpu_write(0x2003, i as u8);
                *byte = self.ppu.cpu_read(0x2004);
            }
            self.ppu.cpu_write(0x2003, 0);
            oam
        }

        fn dma_cycles_remaining(&self) -> u16 {
            self.dma_stall
        }
    }

    #[test]
    fn ram_is_mirrored_every_0800() {
        let mut b = Bus::new(blank_cart());
        b.cpu_write(0x0000, 0x11);
        b.cpu_write(0x0400, 0x22);
        for (mirror, expect) in [
            (0x0000, 0x11),
            (0x0400, 0x22),
            (0x0800, 0x11),
            (0x0C00, 0x22),
            (0x1000, 0x11),
            (0x1400, 0x22),
            (0x1800, 0x11),
            (0x1C00, 0x22),
        ] {
            assert_eq!(b.cpu_read(mirror), expect, "${mirror:04X}");
        }
    }

    #[test]
    fn the_apu_window_reads_as_open_bus_and_writes_are_dropped() {
        let mut b = Bus::new(blank_cart());
        assert_eq!(b.cpu_read(0x4015), 0, "$4015 with no APU");
        b.cpu_write(0x4015, 0x0F); // would raise a frame IRQ if there were an APU
        assert_eq!(b.cpu_read(0x4015), 0);
        b.cpu_write(0x4010, 0x0F); // DMC channel
        b.cpu_write(0x4017, 0x40); // frame counter
        assert_eq!(b.cpu_read(0x4015), 0);
    }

    /// `$2000-$3FFF` writes must land on the PPU register the address names,
    /// not in RAM. Two cheap round trips prove it without ticking a frame:
    /// `$2003`/`$2004` (OAMADDR, then read the byte it points at) and
    /// `$2006`/`$2007` (point `v` at a palette entry, write it, read it back).
    /// A RAM-swallowing bus leaves OAMADDR at 0 and `v` at 0, so both read 0.
    #[test]
    fn writes_to_2000_3fff_reach_the_ppu_register_the_address_names() {
        let mut b = Bus::new(blank_cart());
        b.cpu_write(0x2003, 0x40);
        b.cpu_write(0x2004, 0xAA);
        b.cpu_write(0x2003, 0x40); // the write above moved OAMADDR on to $41
        assert_eq!(
            b.cpu_read(0x2004),
            0xAA,
            "$2003/$2004 writes never reached the PPU"
        );

        b.cpu_write(0x2006, 0x3F); // v high
        b.cpu_write(0x2006, 0x00); // v low -> $3F00, palette entry 0
        b.cpu_write(0x2007, 0x21);
        b.cpu_write(0x2006, 0x3F); // the write above moved v on to $3F01
        b.cpu_write(0x2006, 0x00);
        assert_eq!(
            b.cpu_read(0x2007) & 0x3F,
            0x21,
            "$2006/$2007 writes never reached the PPU"
        );
    }

    /// RAM mirrors on the WRITE side too, or a game that sets up its zero page
    /// through `$0800` reads back nothing.
    #[test]
    fn ram_writes_are_mirrored_every_0800() {
        let mut b = Bus::new(blank_cart());
        b.cpu_write(0x0800, 0x5A);
        assert_eq!(b.cpu_read(0x0000), 0x5A, "written at $0800, read at $0000");
        b.cpu_write(0x1000, 0xA5);
        assert_eq!(b.cpu_read(0x0000), 0xA5, "written at $1000, read at $0000");
        b.cpu_write(0x1800, 0x3C);
        assert_eq!(b.cpu_read(0x0000), 0x3C, "written at $1800, read at $0000");
    }

    /// Mapper 66 banks by bus conflict, so a matching write to `$8000-$BFFF`
    /// must reach the cartridge from the bus. The synthetic image fills each
    /// bank with its own index and patches bank 2, so writing the byte the CPU
    /// already reads there selects a different bank -- visible through
    /// `cpu_read`.
    #[test]
    fn a_bus_conflicting_write_reaches_the_cartridge_and_switches_banks() {
        let mut b = Bus::new(cart_bankable_at_8000());
        assert_eq!(b.cpu_read(0x8001), 2, "power-on bank 2 reads 2 at $8001");
        // A matching write banks 1 % 4 = 1, and bank 1 reads 1 at $8001. If the
        // bus dropped the write, this still reads 2.
        b.cpu_write(0x8000, 1);
        assert_eq!(
            b.cpu_read(0x8001),
            1,
            "a matching write must reach the cartridge"
        );
        // A NON-matching write must be ignored, which also proves the value
        // reached the cartridge's conflict check rather than being banked
        // blindly. Bank 1 reads 1 at $8000, so $EE does not match.
        b.cpu_write(0x8000, 0xEE);
        assert_eq!(b.cpu_read(0x8001), 1, "a non-matching write must not bank");
    }

    #[test]
    fn reading_4016_shifts_controller_one() {
        let mut b = Bus::new(blank_cart());
        b.controllers[0].set_buttons(0x01 | 0x08);
        b.cpu_write(0x4016, 1);
        b.cpu_write(0x4016, 0);
        assert_eq!(b.cpu_read(0x4016) & 1, 1, "A");
        assert_eq!(b.cpu_read(0x4016) & 1, 0, "B");
        assert_eq!(b.cpu_read(0x4016) & 1, 0, "Select");
        assert_eq!(b.cpu_read(0x4016) & 1, 1, "Start");
    }

    /// `$4014` takes a PAGE, not an address: NESdev "2A03" says "$4014 OAMDMA:
    /// OAM DMA: Copy 256 bytes from $xx00-$xxFF into OAM via OAMDATA ($2004)",
    /// and "PPU OAM" says the source address "always starts page-aligned
    /// (ending in $00)". Only the high byte of the written value is kept.
    #[test]
    fn a_4014_write_uses_only_the_high_byte_as_the_source_page() {
        let mut b = Bus::new(blank_cart());
        for i in 0..256u16 {
            b.cpu_write(0x0500 + i, 0x80u8.wrapping_add(i as u8));
        }
        b.cpu_write(0x4014, 0x05); // $0500-$05FF
        assert_eq!(b.oam()[0], 0x80, "and DMA does not read below $0500");
        assert_eq!(b.oam()[255], 0x80u8.wrapping_add(255));
        assert_eq!(b.dma_cycles_remaining(), 513);
    }

    /// NESdev "Standard controller": "Though both are polled from a write to
    /// $4016, controller 1 is read through $4016, and controller 2 is separately
    /// read through $4017". The `$4016` strobe latches both ports, so a game that
    /// strobes once can read both players.
    #[test]
    fn controller_two_is_read_through_4017_and_both_ports_share_the_strobe() {
        let mut b = Bus::new(blank_cart());
        b.controllers[0].set_buttons(0x01); // player 1 holds A
        b.controllers[1].set_buttons(0x08); // player 2 holds Start
        b.cpu_write(0x4016, 1);
        b.cpu_write(0x4016, 0);
        assert_eq!(b.cpu_read(0x4016) & 1, 1, "player 1 A");
        assert_eq!(b.cpu_read(0x4017) & 1, 0, "player 2 B");
        assert_eq!(b.cpu_read(0x4017) & 1, 0, "player 2 Select");
        assert_eq!(b.cpu_read(0x4017) & 1, 0, "player 2 Up");
        assert_eq!(b.cpu_read(0x4017) & 1, 1, "player 2 Start, the fourth bit");
    }

    #[test]
    fn writing_4014_copies_256_bytes_and_stalls_513_cycles() {
        let mut b = Bus::new(blank_cart());
        for i in 0..256u16 {
            b.cpu_write(0x0200 + i, 0xC0 + (i & 0x0F) as u8);
        }
        b.write_dma_from(0x0200);
        for i in 0..256usize {
            assert_eq!(b.oam()[i], 0xC0 + (i & 0x0F) as u8, "OAM byte {i}");
        }
        assert_eq!(b.dma_cycles_remaining(), 513);
    }

    /// `$4014` costs 513 cycles, or 514 when the write lands on an odd CPU
    /// cycle. These two reach that parity through the real path -- `tick`, then
    /// `cpu_write($4014, page)` -- rather than poking the counter, so a bus
    /// that stopped counting cycles altogether (or never advanced them past 0)
    /// fails instead of silently reading 513 twice.
    #[test]
    fn a_dma_stalled_on_an_odd_cycle_costs_one_more_cycle() {
        let mut b = Bus::new(blank_cart());
        for _ in 0..3 {
            b.tick(); // exactly one whole CPU cycle -> `cycles` is now 1, odd
        }
        b.write_dma_from(0x0200);
        assert_eq!(b.dma_cycles_remaining(), 514);
    }

    #[test]
    fn a_dma_stalled_on_an_even_cycle_costs_513_cycles() {
        let mut b = Bus::new(blank_cart());
        b.tick(); // one dot: `cycles` is still 0, even
        b.write_dma_from(0x0200);
        assert_eq!(b.dma_cycles_remaining(), 513);
    }

    #[test]
    fn oam_dma_reads_its_source_through_the_cpu_address_decode() {
        // RAM is nowhere zero, and the power-on bank of PRG is filled with its
        // own index, 2, so RAM and cartridge are distinguishable across all 256
        // bytes. A copy that indexed `cpu_ram` directly gets every non-RAM
        // source page wrong.
        let mut b = Bus::new(blank_cart());
        for i in 0..0x2000u16 {
            b.cpu_write(i, 0xAA);
        }
        b.write_dma_from(0x0000);
        assert!(
            b.oam().iter().all(|&v| v == 0xAA),
            "a DMA out of RAM must read RAM"
        );
        b.write_dma_from(0x8000);
        assert!(
            b.oam().iter().all(|&v| v == 2),
            "a DMA out of $8000 must read PRG, not the RAM mirror"
        );
        b.write_dma_from(0x7F00); // unmapped: open bus
        assert!(
            b.oam().iter().all(|&v| v == 0),
            "a DMA out of unmapped space must not read RAM"
        );
    }

    /// The copy goes in through `$2003`/`$2004`, so it lands at OAM byte 0
    /// whatever `OAMADDR` was left at, and the wrap leaves `OAMADDR` at 0 -- so a
    /// stale pointer cannot shear a sprite frame.
    #[test]
    fn oam_dma_starts_at_oam_byte_zero_whatever_oamaddr_was() {
        let mut b = Bus::new(blank_cart());
        for i in 0..256u16 {
            b.cpu_write(0x0300 + i, i as u8);
        }
        b.cpu_write(0x2003, 0x40); // point OAMADDR into the middle of OAM
        b.write_dma_from(0x0300);
        let oam = b.oam();
        assert_eq!(oam[0], 0x00, "source byte 0 went to OAM byte 0");
        assert_eq!(oam[1], 0x01);
        assert_eq!(oam[0x3F], 0x3F);
        assert_eq!(
            oam[0x40], 0x40,
            "and the rest of the page followed it round"
        );
    }

    /// A DMA out of `$20xx` must read PPU registers, because every source byte
    /// goes back through the CPU decode. Reading `$2004` returns the OAM byte at
    /// the current `OAMADDR` and the write that follows puts the same byte back,
    /// so the copy is idempotent and exactly the offsets that land on a
    /// register read carry data; everything else reads back as 0.
    #[test]
    fn oam_dma_out_of_20xx_reads_ppu_registers() {
        let mut b = Bus::new(blank_cart());
        // Load OAM with a distinctive pattern through a RAM DMA.
        for i in 0..256u16 {
            b.cpu_write(0x0400 + i, 0x40u8.wrapping_add(i as u8));
        }
        b.write_dma_from(0x0400);
        // Now DMA out of the PPU window itself.
        b.write_dma_from(0x2000);
        let oam = b.oam();
        for (i, &byte) in oam.iter().enumerate() {
            if i % 8 == 4 {
                assert_eq!(byte, 0x40u8.wrapping_add(i as u8), "OAM byte {i} via $2004");
            } else {
                assert_eq!(byte, 0, "OAM byte {i} came from a write-only register");
            }
        }
    }

    #[test]
    fn the_dma_stall_costs_cycles_not_dots() {
        let mut b = Bus::new(blank_cart());
        b.write_dma_from(0x0200);
        let mut ticks = 0;
        while b.dma_cycles_remaining() > 0 {
            b.tick();
            ticks += 1;
        }
        assert_eq!(ticks, 513 * 3, "513 cycles is 1539 dots");
    }

    #[test]
    fn tick_advances_one_ppu_dot_not_three() {
        // `Ppu::new` starts on the pre-render line at dot 0. A line is 341 dots,
        // so scanlines 0..240 take 241 * 341 dots, and `Ppu::tick` raises the
        // vblank flag -- the only public "a frame is over" signal -- at the top
        // of the tick that *begins* on the second dot of scanline 241. That is
        // one tick after the PPU first arrives there.
        let mut b = Bus::new(blank_cart());
        let mut ticks = 0;
        while !b.ppu.frame_ready() {
            b.tick();
            ticks += 1;
            assert!(ticks < 200_000, "frame never completed");
        }
        assert_eq!(ticks, 341 + 241 * 341 + 2);
    }

    /// The stall is what the run loop gates the CPU on, so it has to be
    /// observable from outside: true the instant `$4014` is written, false the
    /// moment the last counted cycle has been ticked away, and false before any
    /// DMA at all.
    #[test]
    fn dma_busy_is_set_by_a_4014_write_and_cleared_when_the_stall_drains() {
        let mut b = Bus::new(blank_cart());
        assert!(!b.dma_busy(), "no DMA has run, so the CPU is not stalled");
        b.write_dma_from(0x0200);
        assert!(b.dma_busy(), "$4014 must stall the CPU");
        while b.dma_busy() {
            b.tick();
        }
        assert!(!b.dma_busy(), "the stall drained, so the CPU runs again");
        assert_eq!(b.dma_cycles_remaining(), 0);
    }

    #[test]
    fn reset_clears_ram_and_any_dma_stall() {
        let mut b = Bus::new(blank_cart());
        b.cpu_write(0x0000, 0x42);
        b.write_dma_from(0x0200);
        b.reset();
        assert_eq!(b.cpu_read(0x0000), 0);
        assert_eq!(b.dma_cycles_remaining(), 0);
    }

    #[test]
    fn the_cpu_runs_for_thousands_of_cycles_against_a_real_cartridge() {
        // SEI / LDX #FF / TXS, then forever: LDA #AA; STA $0200; LDA #02;
        // STA $4014; JMP loop. Every pass OAM-DMAs a page into the PPU.
        let code = [
            0x78, 0xA2, 0xFF, 0x9A, 0xA9, 0xAA, 0x8D, 0x00, 0x02, 0xA9, 0x02, 0x8D, 0x14, 0x40,
            0x4C, 0x04, 0x80,
        ];
        let mut b = Bus::new(cart_running(&code));
        let mut cpu = Cpu::new();
        cpu.reset(&mut b);
        // Enough instructions for the PPU to clear a whole frame: a frame is
        // 341 * 262 = 89342 dots and each cycle buys three, so ~30 000 cycles
        // is nearly a second of emulated time at one DMA per loop.
        for _ in 0..30_000 {
            cpu.step(&mut b);
        }
        assert_eq!(b.oam()[0], 0xAA, "the CPU ran DMA into the PPU");
        assert!(b.ppu.frame_ready(), "the PPU never reached vblank");
    }
}
