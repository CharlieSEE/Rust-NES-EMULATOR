//! iNES loader for the boards this emulator knows: mapper 1 (MMC1) for
//! "Legend of Zelda", mapper 2 (UxROM) for "Contra", mapper 66 (UxROM variant
//! with bus conflicts) for "Super Mario Bros. + Duck Hunt", and mapper 0 (NROM)
//! for blargg's APU test ROMs -- NROM-128 and NROM-256, both with the
//! `$6000-$7FFF` PRG RAM the tests need, and the 2005 suite with no CHR at all
//! (the PPU gets RAM instead).
//!
//! Mapper 1 is MMC1: a five-write serial register controlling PRG mode, CHR
//! banking and nametable mirroring. Zelda carries CHR RAM and an 8 KiB
//! battery-backed save. The PRG register's bit 4 disables that RAM on MMC1B;
//! iNES cannot distinguish the revisions, so mapper 1 honours the bit and
//! mapper 155's always-enabled MMC1A is left out of scope.
//!
//! Mapper 2's window is 16 KiB switchable at `$8000-$BFFF` plus the LAST
//! 16 KiB fixed at `$C000-$FFFF`. The bank register decodes any write in
//! `$8000-$FFFF`; iNES mapper 2 is conventionally emulated without bus
//! conflicts, and Contra's own bank table writes the byte already in ROM.
//! Its header asks for 128 KiB PRG, no CHR bytes (CHR RAM) and vertical
//! mirroring, all of which the existing PPU/bus paths already provide.
//!
//! The SMB + Duck Hunt iNES header is honest about that cartridge: its `chr=2`
//! really is 16 KiB of tile data, its `prg=4` really is 64 KiB, and its
//! mirroring bit really is vertical. What the header does NOT carry is any of the below, so every one
//! was measured against the real cartridge image instead:
//!
//! 1. PRG banking is by BUS CONFLICT, not by the written value. A write to
//!    `$8000-$BFFF` only changes the bank when the byte written already equals
//!    the byte the CPU would read at that address. Plain UxROM banking leaves
//!    the cartridge in its reset stub forever.
//! 2. The header's 2x8 KiB of CHR is REAL, and the register's low two bits
//!    (`xxPPxxCC`, NESdev "GxROM") select which 8 KiB is at `$0000-$1FFF`.
//!    Ignoring them was a black screen with the Duck Hunt half of the title
//!    logo rendered as Mario's tiles: the game flips CHR bank mid-frame to
//!    show both games' logos at once. The array lives in the PPU because the
//!    PPU fetches pattern tables every dot; this struct owns the bytes.
//! 3. Nametable mirroring is VERTICAL and hardwired in the board. The header's
//!    mirroring bit agrees, but `Cartridge::load` never reads it -- the PPU
//!    fixes the arrangement itself, because this board wires nametables
//!    vertically and there is nothing to select.
//! 4. PRG is 64 KiB = TWO 32 KiB windows over `$8000-$FFFF`, and the register
//!    is `xxPPxxCC` -- PRG on bits 5-4, CHR on bits 1-0. The header's "four
//!    16 KiB banks" is just the 16 KiB unit iNES counts in, and reading it as
//!    a 16 KiB window with a fixed upper half is what put an illegal opcode at
//!    the game's own `JSR $E5C1`.

use crate::ppu::Mirroring;
use crate::region::Region;

/// The CPU window. NESdev "GxROM" says "CPU $8000-$FFFF: 32 KB switchable
/// PRG ROM bank" -- ONE 32 KiB block over the whole window. There is no
/// separately selectable `$C000-$FFFF` half; `$8000` and `$C000` are both
/// offsets into the same bank, which is why this is 32 KiB and not 16.
const BANK: usize = 0x8000;

/// iNES counts PRG in 16 KiB units, whatever the window size is.
const PRG_UNIT: usize = 0x4000;

/// Every banking unit this cartridge model needs below 8 KiB: MMC1 switches
/// CHR in 4 KiB windows (and GxROM's 8 KiB value splits into two).
const CHR_UNIT: usize = 0x1000;

/// The bank the register holds until something writes it. Measured on this
/// cartridge: the power-on bytes at `$8000` are
/// `78 A9 11 8D 02 80 4C 00 80` -- SEI / LDA #$11 / STA $8002 / JMP $8000 --
/// and bank 0's own `$8002` IS `$11`, so the write conflicts, the register takes
/// `$11`, bits 5-4 are `$01`, and the CPU moves to bank 1. Any other power-on
/// bank drops the CPU in the middle of an unrelated routine.
const POWER_ON_BANK: usize = 0;

/// One 8 KiB CHR bank: the window at `$0000-$1FFF`, selected by bits 1-0.
const CHR_BANK: usize = 0x2000;

/// PRG ROM, CHR ROM, and the mapper registers. The CHR bytes stay here
/// because the file owns them; the PPU holds the banks because it fetches
/// pattern memory every dot, and the registers swap one in for the other.
pub struct Cartridge {
    prg: Vec<u8>,
    chr: Vec<u8>,
    /// Which board this image is, and therefore how `$8000-$FFFF` decodes.
    board: Board,
    /// Which PRG bank is mapped at the switchable window: a 32 KiB bank over
    /// `$8000-$FFFF` on GxROM, a 16 KiB bank at `$8000-$BFFF` on UxROM, and
    /// MMC1's PRG register in 16 KiB units whose meaning follows `control`.
    bank: usize,
    /// The two 4 KiB CHR banks the board maps at `$0000-$0FFF` and
    /// `$1000-$1FFF`. GxROM's 8 KiB value fills both; MMC1 selects them
    /// separately in 4 KiB mode.
    chr_banks: [usize; 2],
    /// MMC1 control register. Power-on/reset value is PRG mode 3 (switch at
    /// `$8000`, last bank fixed), 8 KiB CHR mode, and one-screen lower
    /// nametable mirroring (control bits 0-1 are 0).
    control: u8,
    /// MMC1's five-write serial port: bits received so far and how many.
    shift: u8,
    shift_count: u8,
    /// MMC1B PRG register bit 4: false disables `$6000-$7FFF` until the game
    /// writes a value with the bit clear. Power-on is enabled.
    prg_ram_enabled: bool,
    /// How the board wires the four logical nametables onto 2 KiB of VRAM.
    mirroring: Mirroring,
    /// Whether the header marks PRG RAM as battery-backed, so it should be
    /// written to a `.sav` file next to the ROM.
    battery: bool,
    /// Which console timing the header asks for.
    region: Region,
}

/// The four boards this emulator knows how to read.
enum Board {
    /// NROM, no banking: 16 KiB PRG mirrors across the window, 32 KiB maps
    /// both halves directly. Writes to the window go nowhere.
    Nrom,
    /// MMC1, the Zelda board: serial PRG/CHR banking, mirroring control, and
    /// battery-backed PRG RAM.
    Mmc1,
    /// GxROM, the SMB + Duck Hunt board: bus-conflict banking, see above.
    GxRom,
    /// UxROM, the Contra board: 16 KiB switchable low window, last 16 KiB
    /// fixed high window, and no CHR banking.
    UxRom,
}

impl Cartridge {
    pub fn load(bytes: &[u8]) -> Result<Cartridge, String> {
        if bytes.len() < 16 || &bytes[..4] != b"NES\x1a" {
            return Err("not an iNES image: bad magic".to_string());
        }
        let flags6 = bytes[6];
        // A trainer is 512 bytes between the header and PRG. Rare, but reading
        // it as PRG puts every bank off by 32 KiB, which looks like a bad ROM.
        let start = 16 + if flags6 & 0x04 != 0 { 512 } else { 0 };
        let prg_len = bytes[4] as usize * PRG_UNIT;
        if prg_len == 0 || bytes.len() < start + prg_len {
            return Err(format!("truncated PRG: header claims {prg_len} bytes"));
        }
        let chr_len = bytes[5] as usize * CHR_BANK;
        if bytes.len() < start + prg_len + chr_len {
            return Err(format!("truncated CHR: header claims {chr_len} bytes"));
        }
        let mapper = bytes[7] & 0xF0 | bytes[6] >> 4;
        // NES 2.0 marks itself in flags 7 bits 2-3 and puts the CPU/PPU timing
        // in byte 12: 0 NTSC, 1 licensed PAL, 2 multiple-region, 3 Dendy. A
        // plain iNES header only has flags 9 bit 0 and flags 10 bits 0-1, and
        // those bytes are padding in every other header variant -- including
        // the "DiskDude!" corruption, whose 's' in flags 9 would otherwise
        // claim PAL. Only read them when flags 7 says plain iNES.
        //
        // ponytail: Dendy and multiple-region fall back to NTSC. Both are a
        // mixed clock this emulator has no model for; add a Region variant
        // when a Dendy game turns up.
        let flags7 = bytes[7];
        let nes20 = flags7 & 0x0C == 0x08;
        let region = if nes20 {
            match bytes[12] & 0x03 {
                1 => Region::Pal,
                _ => Region::Ntsc,
            }
        } else if flags7 & 0x0C == 0x00 && (bytes[9] & 0x01 != 0 || bytes[10] & 0x03 == 0x02) {
            Region::Pal
        } else {
            Region::Ntsc
        };
        let board = match mapper {
            0 => Board::Nrom,
            1 => Board::Mmc1,
            2 => Board::UxRom,
            66 => Board::GxRom,
            other => return Err(format!("mapper {other}, only 0, 1, 2 and 66 are supported")),
        };
        // NROM, MMC1 and UxROM start with bank 0. GxROM's power-on bank is
        // measured, and `POWER_ON_BANK % (prg_len / BANK)` would divide by
        // zero for a 16 KiB image.
        let bank = match board {
            Board::Nrom | Board::Mmc1 | Board::UxRom => 0,
            Board::GxRom => POWER_ON_BANK % (prg_len / BANK).max(1),
        };
        // MMC1's control register powers up at $0C: PRG mode 3, 8 KiB CHR
        // mode, and one-screen lower nametable arrangement. GxROM is hardwired
        // vertical. NROM and UxROM follow the header bit.
        let mirroring = match board {
            Board::Mmc1 => Mirroring::SingleLower,
            Board::GxRom => Mirroring::Vertical,
            Board::Nrom | Board::UxRom if flags6 & 0x01 != 0 => Mirroring::Vertical,
            Board::Nrom | Board::UxRom => Mirroring::Horizontal,
        };
        Ok(Cartridge {
            prg: bytes[start..start + prg_len].to_vec(),
            chr: bytes[start + prg_len..start + prg_len + chr_len].to_vec(),
            board,
            bank,
            chr_banks: [0, 1],
            control: 0x0C,
            shift: 0,
            shift_count: 0,
            prg_ram_enabled: true,
            mirroring,
            battery: flags6 & 0x02 != 0,
            region,
        })
    }

    /// Whether the CPU can reach PRG RAM right now. NROM's test images and
    /// MMC1's battery save both use the window; the SMB and Contra boards do
    /// not. NROM has no register, while MMC1's PRG register bit 4 is MMC1B's
    /// disable bit: writes with it set make `$6000-$7FFF` open bus until the
    /// game clears it again.
    pub fn prg_ram_enabled(&self) -> bool {
        matches!(self.board, Board::Nrom | Board::Mmc1) && self.prg_ram_enabled
    }

    /// Whether the cartridge has battery-backed PRG RAM to persist.
    pub fn has_battery(&self) -> bool {
        self.battery
    }

    /// The console timing the header selected.
    pub fn region(&self) -> Region {
        self.region
    }

    /// The CHR ROM image, for the PPU to hold. Borrowed, not copied.
    pub fn chr_rom(&self) -> &[u8] {
        &self.chr
    }

    /// The eight 1 KiB CHR slots at `$0000-$1FFF`. Slots 0-3 are the first
    /// 4 KiB bank and slots 4-7 the second; MMC1's 8 KiB mode points both
    /// halves at one aligned pair.
    pub fn chr_map(&self) -> [usize; 8] {
        let banks = self.chr_bank_count();
        let (first, second) = match self.board {
            Board::Mmc1 if self.control & 0x10 == 0 => {
                let base = self.chr_banks[0] & !1;
                (base % banks, (base + 1) % banks)
            }
            _ => (self.chr_banks[0] % banks, self.chr_banks[1] % banks),
        };
        [
            first * 4,
            first * 4 + 1,
            first * 4 + 2,
            first * 4 + 3,
            second * 4,
            second * 4 + 1,
            second * 4 + 2,
            second * 4 + 3,
        ]
    }

    /// How many 4 KiB CHR banks exist. A CHR RAM cartridge has the 8 KiB the
    /// PPU provides; a short or absent CHR image must not divide by zero.
    fn chr_bank_count(&self) -> usize {
        (self.chr.len() / CHR_UNIT).max(2)
    }

    /// How the board currently wires the nametables.
    pub fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// The CPU's `$8000-$FFFF` window. NROM has no register: 16 KiB images
    /// mirror their single block, 32 KiB ones map both halves directly, and
    /// `& (len - 1)` does both. GxROM maps one 32 KiB bank across the whole
    /// window. UxROM maps a 16 KiB bank at `$8000-$BFFF` and pins the last
    /// 16 KiB of the file at `$C000-$FFFF`. MMC1 follows its PRG mode.
    pub fn cpu_read(&self, addr: u16) -> u8 {
        match self.board {
            Board::Nrom => self.prg[addr as usize & (self.prg.len() - 1)],
            Board::GxRom => self.prg[self.bank * BANK + (addr as usize - 0x8000)],
            Board::UxRom if addr < 0xC000 => {
                self.prg[self.bank * PRG_UNIT + (addr as usize - 0x8000)]
            }
            Board::UxRom => self.prg[self.prg.len() - PRG_UNIT + (addr as usize - 0xC000)],
            Board::Mmc1 => {
                let (low, high) = self.mmc1_prg_banks();
                let bank = if addr < 0xC000 { low } else { high };
                self.prg[bank * PRG_UNIT + (addr as usize & 0x3FFF)]
            }
        }
    }

    /// The two 16 KiB PRG banks MMC1 maps at `$8000` and `$C000`. Control bits
    /// 2-3 select the mode: 0/1 use one 32 KiB bank, 2 fixes the first bank
    /// and switches the top, 3 switches the bottom and fixes the last.
    fn mmc1_prg_banks(&self) -> (usize, usize) {
        let count = (self.prg.len() / PRG_UNIT).max(1);
        let bank = self.bank % count;
        match self.control & 0x0C {
            0x08 => (0, bank),
            0x0C => (bank, count - 1),
            _ => {
                let base = bank & !1;
                (base % count, (base + 1) % count)
            }
        }
    }

    /// Mapper register writes.
    ///
    /// GxROM is a bus-conflict board: the bank register only moves when the
    /// byte written is already the byte the CPU reads at that address. Its
    /// register is `xxPPxxCC`, so PRG is BITS 5-4 (NESdev "GxROM": "the board
    /// only supports 64KB PRG", hence two banks and one select bit) and CHR is
    /// bits 1-0.
    ///
    /// UxROM selects a 16 KiB bank on any write in `$8000-$FFFF`. iNES mapper
    /// 2 is conventionally emulated without bus conflicts, and Contra's bank
    /// table writes the byte already in ROM anyway.
    ///
    /// MMC1 receives five serial bits per register write, least significant
    /// first; bit 7 of a write resets the shift register instead.
    pub fn cpu_write(&mut self, addr: u16, val: u8) {
        match self.board {
            // NROM has no registers at all: every write to the window is
            // ignored.
            Board::Nrom => {}
            Board::Mmc1 => self.mmc1_write(addr, val),
            Board::GxRom => {
                // The register decodes the whole `$8000-$FFFF` window. Bus
                // conflicts still decide whether the write lands: the value
                // must equal the byte the ROM currently drives.
                if addr < 0x8000 || val != self.cpu_read(addr) {
                    return;
                }
                // `saturating_sub`, not `-`: the iNES header can claim more PRG
                // or CHR than the file carries, and a bank count of 0 must not
                // underflow.
                self.bank = ((val >> 4) as usize) & (self.prg.len() / BANK).saturating_sub(1);
                let banks = (self.chr.len() / CHR_BANK).max(1);
                let bank = (val & 3) as usize % banks;
                self.chr_banks = [bank * 2, bank * 2 + 1];
            }
            Board::UxRom => {
                if addr >= 0x8000 {
                    // The 74HC161 counts modulo the ROM's bank count, so a
                    // select past the end wraps rather than reading out of
                    // bounds.
                    self.bank = (val as usize) % (self.prg.len() / PRG_UNIT);
                }
            }
        }
    }

    fn mmc1_write(&mut self, addr: u16, val: u8) {
        if addr < 0x8000 {
            return;
        }
        // Bit 7 clears the shift register and forces PRG mode 3; the other
        // control bits (mirroring, CHR mode) keep their values.
        if val & 0x80 != 0 {
            self.shift = 0;
            self.shift_count = 0;
            self.control |= 0x0C;
            return;
        }
        self.shift |= (val & 1) << self.shift_count;
        self.shift_count += 1;
        if self.shift_count < 5 {
            return;
        }
        let register = ((addr >> 13) & 3) as usize;
        let value = self.shift;
        self.shift = 0;
        self.shift_count = 0;
        match register {
            0 => {
                self.control = value & 0x1F;
                self.mirroring = match value & 0x03 {
                    0 => Mirroring::SingleLower,
                    1 => Mirroring::SingleUpper,
                    2 => Mirroring::Vertical,
                    _ => Mirroring::Horizontal,
                };
            }
            1 => self.chr_banks[0] = (value & 0x1F) as usize,
            2 => self.chr_banks[1] = (value & 0x1F) as usize,
            _ => {
                self.bank = (value & 0x0F) as usize;
                self.prg_ram_enabled = value & 0x10 == 0;
            }
        }
    }
}

/// 16-byte iNES header for a synthetic image, shared by every image builder in
/// the crate. `mapper` is split the way iNES has it: low nibble in byte 6, high
/// nibble in byte 7. 66 is the SMB + Duck Hunt GxROM board (vertical mirroring),
/// 1 is MMC1 (battery, horizontal header bits), 2 is UxROM, 0 is plain NROM.
#[cfg(test)]
pub(crate) fn ines_header(prg_banks: u8, chr_banks: u8, mapper: u8) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[..4].copy_from_slice(b"NES\x1a");
    h[4] = prg_banks;
    h[5] = chr_banks;
    h[6] = (mapper & 0x0F) << 4
        | match mapper {
            66 => 0x01,
            1 => 0x02,
            _ => 0x00,
        };
    h[7] = mapper & 0xF0;
    h
}

/// A ROM under the manifest directory, or `None` when it is not on disk. The
/// ROM images are gitignored, so every test that needs one must be able to skip
/// on a clean checkout.
#[cfg(test)]
pub(crate) fn test_rom(rel: &str) -> Option<Vec<u8>> {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel);
    match std::fs::read(&path) {
        Ok(b) => Some(b),
        Err(_) => {
            println!("note: {path} not present, skipping");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two 32 KiB PRG banks where bank `i` is filled with the byte `i`, so the
    /// mapped bank is observable through `cpu_read`. `patch` is written into
    /// the power-on bank's `addr` so a matching write carries a value the
    /// power-on bank does not already select.
    fn banks_filled_with_their_own_index(addr: usize, patch: u8) -> Cartridge {
        let mut bytes = ines_header(4, 2, 66).to_vec();
        for fill in 0..2u8 {
            bytes.extend(std::iter::repeat_n(fill, BANK));
        }
        bytes.extend(std::iter::repeat_n(0, CHR_BANK * 2));
        bytes[16 + POWER_ON_BANK * BANK + addr - 0x8000] = patch;
        Cartridge::load(&bytes).expect("synthetic image must load")
    }

    /// The real cartridge, or `None` when it is not on disk: the ROM is
    /// gitignored, so a test that needs its actual bytes has to be able to skip.
    fn real_rom() -> Option<Vec<u8>> {
        test_rom("Super Mario Bros. + Duck Hunt (USA).nes")
    }

    /// A synthesized UxROM image: `prg_banks` 16 KiB blocks, each filled with
    /// its own index, no CHR bytes (so the PPU gets CHR RAM), and mapper 2.
    fn uxrom_cart(prg_banks: u8) -> Cartridge {
        let mut bytes = ines_header(prg_banks, 0, 2).to_vec();
        for bank in 0..prg_banks {
            bytes.extend(std::iter::repeat_n(bank, PRG_UNIT));
        }
        Cartridge::load(&bytes).expect("synthetic UxROM image must load")
    }

    /// The real Contra cartridge, or `None` when it is not on disk.
    fn contra_rom() -> Option<Vec<u8>> {
        test_rom("Contra (USA).nes")
    }

    /// A synthesized NROM image: 16 KiB PRG block `i` filled with
    /// `0x11 * (i + 1)` so the mapped block is observable through `cpu_read`,
    /// and `chr_banks` 8 KiB CHR banks filled with zero.
    fn nrom_cart(prg_banks: u8, chr_banks: u8) -> Cartridge {
        let mut bytes = ines_header(prg_banks, chr_banks, 0).to_vec();
        for i in 0..prg_banks {
            bytes.extend(std::iter::repeat_n(0x11 * (i + 1), PRG_UNIT));
        }
        bytes.extend(std::iter::repeat_n(0u8, chr_banks as usize * CHR_BANK));
        Cartridge::load(&bytes).expect("synthetic NROM image must load")
    }

    // The conflict tests read at $8001, not $8000: $8000 holds the patched byte,
    // so reading it back could return the value written rather than the identity
    // of the bank now mapped.

    #[test]
    fn a_write_that_does_not_match_the_rom_does_not_switch_banks() {
        let mut c = banks_filled_with_their_own_index(0x8000, 0x11);
        assert_eq!(c.cpu_read(0x8001), 0);
        c.cpu_write(0x8000, 0x01); // the ROM reads 0x11 there
        assert_eq!(c.cpu_read(0x8001), 0);
    }

    #[test]
    fn a_write_that_matches_the_rom_switches_banks() {
        let mut c = banks_filled_with_their_own_index(0x8000, 0x11);
        assert_eq!(c.cpu_read(0x8001), 0);
        c.cpu_write(0x8000, 0x11); // matches -> bits 5-4 = 0b01 -> bank 1
        assert_eq!(c.cpu_read(0x8001), 1);
    }

    /// The window is 32 KiB, so `$C000-$FFFF` is the upper half of the SAME
    /// bank as `$8000-$BFFF` and moves with it. A model that pins the top half
    /// to the last bank fails this, and that pinning is what put an illegal
    /// opcode at `$E5C1` in the game's own `JSR $E5C1`.
    #[test]
    fn the_top_half_of_the_window_is_part_of_the_switchable_bank() {
        let mut c = banks_filled_with_their_own_index(0x8000, 0x11);
        assert_eq!(c.cpu_read(0xC001), 0, "power-on bank 0 covers $C000 too");
        c.cpu_write(0x8000, 0x11);
        assert_eq!(c.cpu_read(0x8001), 1, "bank 1 landed");
        assert_eq!(c.cpu_read(0xC001), 1, "...and it took $C000 with it");
    }

    /// GxROM's register decodes the whole `$8000-$FFFF` window, not just the
    /// lower half. A matching write in the upper half therefore moves the same
    /// 32 KiB bank the lower half is reading.
    #[test]
    fn a_matching_write_to_the_upper_half_moves_the_bank() {
        let mut c = banks_filled_with_their_own_index(0xFFFD, 0x11);
        assert_eq!(c.cpu_read(0xC001), 0, "power-on bank 0 covers $C000");
        c.cpu_write(0xFFFD, 0x11); // matches -> bits 5-4 = 0b01 -> bank 1
        assert_eq!(c.cpu_read(0x8001), 1, "the whole window moved");
        assert_eq!(c.cpu_read(0xC001), 1);
    }

    #[test]
    fn a_bad_image_is_rejected() {
        assert!(Cartridge::load(b"nope").is_err(), "bad magic");
        assert!(Cartridge::load(b"NES\x1a").is_err(), "truncated");
        let mut other_mapper = ines_header(4, 2, 4).to_vec();
        other_mapper.resize(16 + 4 * PRG_UNIT + 2 * CHR_BANK, 0);
        let err = match Cartridge::load(&other_mapper) {
            Ok(_) => panic!("mapper 4 is not supported"),
            Err(e) => e,
        };
        assert!(
            err.contains("mapper 4"),
            "error must name the mapper: {err}"
        );
    }

    /// The game's own boot handshake, executed against the real cartridge
    /// bytes. Bank 0's `$8000` is `78 A9 11 8D 02 80 4C 00 80`
    /// (SEI / LDA #$11 / STA $8002 / JMP $8000) and bank 0's `$8002` IS `$11`,
    /// so that write conflicts, the register takes `$11`, and bits 5-4 move the
    /// window to bank 1 -- whose NMI vector is `$C086`, against bank 0's
    /// `$8082`. This is the real ROM, not a fixture: a synthetic stand-in would
    /// only prove the fixture back.
    #[test]
    fn the_boot_stub_banks_from_bank_zero_into_bank_one() {
        let Some(bytes) = real_rom() else { return };
        let mut c = Cartridge::load(&bytes).expect("real cartridge must load");
        let nmi = |c: &Cartridge| u16::from_le_bytes([c.cpu_read(0xFFFA), c.cpu_read(0xFFFB)]);
        // Power-on: bank 0. RESET = $8000, NMI = $8082.
        assert_eq!(c.cpu_read(0x8000), 0x78, "power-on bank 0 begins with SEI");
        assert_eq!(c.cpu_read(0xFFFA), 0x82);
        assert_eq!(c.cpu_read(0xFFFB), 0x80);
        // The stub writes $11 to $8002, where the ROM already reads $11.
        c.cpu_write(0x8002, 0x11);
        // Bank 1: SEI / LDA #$80 / STA $FFFD / CLD / LDX #$FF / TXS, and the
        // vectors are NMI $C086, RESET $8000, IRQ $C000.
        assert_eq!(c.cpu_read(0x8000), 0x78);
        assert_eq!(nmi(&c), 0xC086, "bits 5-4 of $11 selected bank 1");
        assert_eq!(
            u16::from_le_bytes([c.cpu_read(0xFFFC), c.cpu_read(0xFFFD)]),
            0x8000
        );
        assert_eq!(
            u16::from_le_bytes([c.cpu_read(0xFFFE), c.cpu_read(0xFFFF)]),
            0xC000
        );
    }

    /// Power-on bank 0 and the RESET vector it holds, plus the two banks' bank
    /// table at `$BF00` that the game's banking helper writes back into the
    /// register: `$10` selects bank 1 and `$00` selects bank 0, which is how the
    /// game moves between them once past the stub.
    #[test]
    fn power_on_is_bank_zero_and_the_bank_table_round_trips() {
        let Some(bytes) = real_rom() else { return };
        let mut c = Cartridge::load(&bytes).expect("real cartridge must load");
        assert_eq!(c.prg.len(), 65536);
        assert_eq!(
            u16::from_le_bytes([c.cpu_read(0xFFFC), c.cpu_read(0xFFFD)]),
            0x8000,
            "power-on RESET vector, read out of bank 0"
        );
        c.cpu_write(0x8002, 0x11); // -> bank 1
        assert_eq!(
            [0xBF00, 0xBF01, 0xBF02, 0xBF03].map(|a| c.cpu_read(a)),
            [0x00, 0x01, 0x10, 0x11],
            "bank 1's bank table at $BF00: each byte is written back as itself"
        );
        c.cpu_write(0xBF02, 0x10); // the table byte read back at $BF02
        assert_eq!(
            u16::from_le_bytes([c.cpu_read(0xFFFA), c.cpu_read(0xFFFB)]),
            0xC086
        );
        c.cpu_write(0xBF00, 0x00); // bits 5-4 = 0 -> back to bank 0
        assert_eq!(
            u16::from_le_bytes([c.cpu_read(0xFFFA), c.cpu_read(0xFFFB)]),
            0x8082
        );
    }

    /// UxROM switches the lower 16 KiB window and pins the top half to the
    /// last bank. A model that keeps the old 32 KiB GxROM window leaves the
    /// whole `$8000-$FFFF` range on the same bank and fails both boundary
    /// checks.
    #[test]
    fn uxrom_switches_the_low_16k_window_and_keeps_the_last_bank_fixed() {
        let mut c = uxrom_cart(4);
        assert_eq!(c.cpu_read(0x8000), 0, "power-on bank 0");
        assert_eq!(c.cpu_read(0xBFFF), 0, "last byte of the switchable window");
        assert_eq!(c.cpu_read(0xC000), 3, "top half is the last bank");
        assert_eq!(c.cpu_read(0xFFFF), 3);

        c.cpu_write(0x8000, 2);
        assert_eq!(c.cpu_read(0x8000), 2, "bank select moved the low window");
        assert_eq!(c.cpu_read(0xBFFF), 2);
        assert_eq!(c.cpu_read(0xC000), 3, "and never moved the fixed bank");

        // UxROM's register decodes every write in the whole PRG window, not
        // just the first 16 KiB like GxROM.
        c.cpu_write(0xFFD0, 1);
        assert_eq!(c.cpu_read(0x8000), 1);
        assert_eq!(c.cpu_read(0xC000), 3);
    }

    /// The real Contra cartridge: mapper 2, 128 KiB PRG (eight 16 KiB banks),
    /// CHR RAM, no PRG RAM. Its reset vector lives in the fixed last bank, and
    /// its bank table at `$FFD0` is written back to select a bank.
    #[test]
    fn contra_loads_as_mapper_2_and_banks_without_moving_the_fixed_bank() {
        let Some(bytes) = contra_rom() else { return };
        let mut c = Cartridge::load(&bytes).expect("real Contra cartridge must load");
        assert_eq!(c.prg.len(), 8 * PRG_UNIT, "128 KiB of PRG");
        assert!(c.chr_rom().is_empty(), "Contra carries no CHR bytes");
        assert!(!c.prg_ram_enabled(), "UxROM has no $6000-$7FFF RAM");
        assert_eq!(c.cpu_read(0x8000), 0, "bank 0 starts with its own index");
        assert_eq!(c.cpu_read(0xC000), 7, "fixed bank is bank 7");
        assert_eq!(
            u16::from_le_bytes([c.cpu_read(0xFFFC), c.cpu_read(0xFFFD)]),
            0xC001,
            "RESET vector is in the fixed bank"
        );

        // Contra's bank table at $FFD0 holds 0..7 and is written back to
        // itself; bank 3's first byte is 3.
        c.cpu_write(0xFFD3, 3);
        assert_eq!(c.cpu_read(0x8000), 3, "bank 3 is mapped low");
        assert_eq!(c.cpu_read(0xC000), 7, "the fixed bank did not move");
    }

    /// NROM-128 (16 KiB PRG) mirrors the single block across the whole
    /// `$8000-$FFFF` window; the byte at an offset must appear at that offset
    /// in BOTH halves, so a model that stops reading after 16 KiB fails.
    #[test]
    fn nrom_mirrors_16k_prg_across_the_whole_window() {
        let mut bytes = ines_header(1, 0, 0).to_vec();
        bytes.extend((0..PRG_UNIT).map(|o| o as u8)); // byte at offset o is o & $FF
        let c = Cartridge::load(&bytes).expect("16 KiB NROM image must load");
        assert_eq!(c.cpu_read(0x8000 + 0x1234), 0x34, "lower half");
        assert_eq!(c.cpu_read(0xC000 + 0x1234), 0x34, "upper half mirrors it");
        assert_eq!(c.cpu_read(0xFFFF), 0xFF, "last byte of the window");
        assert!(c.prg_ram_enabled(), "NROM carries the $6000-$7FFF RAM");
        assert!(
            c.chr_rom().is_empty(),
            "no CHR bytes in the file -> CHR RAM"
        );
    }

    /// NROM-256 (32 KiB PRG) has two distinct 16 KiB blocks, and no bank
    /// register: a write that would conflict on GxROM must change nothing.
    #[test]
    fn nrom_32k_prg_is_not_mirrored_and_writes_do_not_bank() {
        let mut c = nrom_cart(2, 1); // 32 KiB PRG, 8 KiB CHR ROM
        assert_eq!(c.prg.len(), 2 * PRG_UNIT);
        assert_eq!(c.cpu_read(0x8000), 0x11, "first 16 KiB block");
        assert_eq!(c.cpu_read(0xC000), 0x22, "second block, not a mirror");
        c.cpu_write(0x8000, 0x11); // NROM has no bank register
        assert_eq!(c.cpu_read(0xC000), 0x22, "the write changed nothing");
        assert!(!c.chr_rom().is_empty(), "8 KiB of CHR ROM in the file");
        assert!(c.prg_ram_enabled());
    }

    /// The real test suites must load: apu_reset is NROM-256 with 8 KiB CHR,
    /// apu_2005 is NROM-128 with no CHR at all (CHR RAM).
    #[test]
    fn the_real_test_roms_load() {
        if let Some(bytes) = test_rom("test_roms/apu_reset/4015_cleared.nes") {
            let c = Cartridge::load(&bytes).expect("apu_reset image must load");
            assert_eq!(c.prg.len(), 2 * PRG_UNIT, "32 KiB NROM-256");
            assert!(c.prg_ram_enabled());
            assert!(!c.chr_rom().is_empty());
            assert_eq!(c.chr_rom().len(), 8192, "8 KiB of CHR ROM");
        }
        if let Some(bytes) = test_rom("test_roms/apu_2005/01.len_ctr.nes") {
            let c = Cartridge::load(&bytes).expect("apu_2005 image must load");
            assert_eq!(c.prg.len(), PRG_UNIT, "16 KiB NROM-128");
            assert!(c.prg_ram_enabled());
            assert!(
                c.chr_rom().is_empty(),
                "no CHR bytes -> the PPU must get RAM"
            );
        }
    }

    /// Four 16 KiB PRG banks, each filled with its own index, plus `chr_banks`
    /// 8 KiB CHR banks whose 4 KiB halves are filled with 0x80+index.
    fn mmc1_cart(prg_banks: u8, chr_banks: u8) -> Cartridge {
        let mut bytes = ines_header(prg_banks, chr_banks, 1).to_vec();
        for bank in 0..prg_banks {
            bytes.extend(std::iter::repeat_n(bank, PRG_UNIT));
        }
        for bank in 0..chr_banks * 2 {
            bytes.extend(std::iter::repeat_n(0x80 + bank, CHR_UNIT));
        }
        Cartridge::load(&bytes).expect("synthetic MMC1 image must load")
    }

    /// MMC1 takes five bits per register, least significant first.
    fn mmc1_write(c: &mut Cartridge, addr: u16, value: u8) {
        for bit in 0..5 {
            c.cpu_write(addr, (value >> bit) & 1);
        }
    }

    #[test]
    fn mmc1_powers_up_in_prg_mode_3_with_the_last_bank_fixed() {
        let mut c = mmc1_cart(4, 0);
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000)),
            (0, 3),
            "reset: switchable $8000, last bank fixed at $C000"
        );
        mmc1_write(&mut c, 0xE000, 1);
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000)),
            (1, 3),
            "PRG register 1 moves the low window only"
        );
    }

    #[test]
    fn mmc1_mode_2_fixes_the_first_bank_and_switches_the_top() {
        let mut c = mmc1_cart(4, 0);
        mmc1_write(&mut c, 0x8000, 0x08); // PRG mode 2
        mmc1_write(&mut c, 0xE000, 2);
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000)),
            (0, 2),
            "mode 2 pins bank 0 at $8000 and maps bank 2 at $C000"
        );
    }

    #[test]
    fn mmc1_mode_0_uses_one_32k_bank_and_ignores_the_low_prg_bit() {
        let mut c = mmc1_cart(4, 0);
        mmc1_write(&mut c, 0x8000, 0x00); // PRG mode 0
        mmc1_write(&mut c, 0xE000, 2);
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000)),
            (2, 3),
            "one 32 KiB bank: the register's low bit is ignored"
        );
    }

    #[test]
    fn mmc1_control_bits_0_and_1_choose_mirroring() {
        for (bits, want) in [
            (0x00, Mirroring::SingleLower),
            (0x01, Mirroring::SingleUpper),
            (0x02, Mirroring::Vertical),
            (0x03, Mirroring::Horizontal),
        ] {
            let mut c = mmc1_cart(2, 0);
            mmc1_write(&mut c, 0x8000, bits);
            assert_eq!(c.mirroring(), want, "control bits {bits:02b}");
        }
    }

    #[test]
    fn mmc1_chr_registers_map_four_kib_windows() {
        let mut c = mmc1_cart(2, 1); // 8 KiB CHR ROM = two 4 KiB banks
        mmc1_write(&mut c, 0x8000, 0x10); // CHR mode: 4 KiB windows
        mmc1_write(&mut c, 0xA000, 1); // first window -> bank 1
        mmc1_write(&mut c, 0xC000, 0); // second window -> bank 0
        assert_eq!(
            c.chr_map(),
            [4, 5, 6, 7, 0, 1, 2, 3],
            "slots 0-3 are CHR bank 1, slots 4-7 are bank 0"
        );
    }

    #[test]
    fn mmc1_eight_kib_chr_mode_ignores_the_low_chr_bit() {
        let mut c = mmc1_cart(2, 1);
        mmc1_write(&mut c, 0x8000, 0x00); // CHR mode: 8 KiB, both halves
        mmc1_write(&mut c, 0xA000, 1); // low bit ignored -> aligned pair 0/1
        assert_eq!(c.chr_map(), [0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn mmc1_powers_up_in_one_screen_lower_mirroring() {
        let c = mmc1_cart(2, 0);
        assert_eq!(
            c.mirroring(),
            Mirroring::SingleLower,
            "control $0C has nametable bits 0-1 clear"
        );
    }

    #[test]
    fn mmc1_prg_ram_bit_four_disables_the_six_thousand_window() {
        let mut c = mmc1_cart(2, 0);
        assert!(c.prg_ram_enabled(), "power-on PRG RAM is enabled");
        mmc1_write(&mut c, 0xE000, 0x10);
        assert!(!c.prg_ram_enabled(), "MMC1B bit 4 disables PRG RAM");
        assert_eq!(c.cpu_read(0x8000), 0, "and the PRG bank is still bank 0");
        mmc1_write(&mut c, 0xE000, 0x01);
        assert!(c.prg_ram_enabled(), "clearing bit 4 enables it again");
    }

    /// Region comes from the header. NES 2.0's byte 12 is the authority when
    /// its signature is present; otherwise iNES flags 9 bit 0 and flags 10
    /// bits 0-1 are the only region fields iNES has.
    #[test]
    fn the_header_selects_the_region() {
        let load = |h: [u8; 16]| {
            let mut bytes = h.to_vec();
            bytes.extend(std::iter::repeat_n(0u8, PRG_UNIT));
            Cartridge::load(&bytes).expect("synthetic region image must load")
        };

        let mut flags9 = ines_header(1, 0, 0);
        flags9[9] = 1;
        assert_eq!(load(flags9).region(), Region::Pal, "iNES flags 9 bit 0");

        let mut flags10 = ines_header(1, 0, 0);
        flags10[10] = 0x02;
        assert_eq!(
            load(flags10).region(),
            Region::Pal,
            "iNES flags 10 says PAL"
        );

        // NES 2.0: byte 7 bits 2-3 are the signature, byte 12 bits 0-1 the
        // timing mode (1 = RP2C07, licensed PAL).
        let mut nes20 = ines_header(1, 0, 0);
        nes20[7] |= 0x08;
        nes20[12] = 0x01;
        assert_eq!(load(nes20).region(), Region::Pal, "NES 2.0 byte 12");

        assert_eq!(
            load(ines_header(1, 0, 0)).region(),
            Region::Ntsc,
            "a plain header still boots NTSC"
        );

        // flags 9's PAL bit is only meaningful in an otherwise valid iNES
        // header. "DiskDude!"-corrupted headers put 's' (0x73) there; flags 7
        // bits 2-3 are the tell.
        let mut junk = ines_header(1, 0, 0);
        junk[7] = 0x04;
        junk[9] = 0x01;
        assert_eq!(
            load(junk).region(),
            Region::Ntsc,
            "reserved flags 7 bits do not claim PAL"
        );
    }

    #[test]
    fn the_real_zelda_cartridge_loads_as_mmc1_with_a_battery() {
        let Some(bytes) = test_rom("Legend of Zelda, The (Europe) (Rev 1).nes") else {
            return;
        };
        let c = Cartridge::load(&bytes).expect("Zelda must load");
        assert!(matches!(c.board, Board::Mmc1), "mapper 1 is MMC1");
        assert_eq!(
            c.region(),
            Region::Pal,
            "the European ROM marks RP2C07 in NES 2.0 byte 12"
        );
        assert!(c.has_battery(), "Zelda saves to battery-backed PRG RAM");
        assert!(c.chr_rom().is_empty(), "Zelda uploads its own tiles");
        assert_eq!(
            c.mirroring(),
            Mirroring::SingleLower,
            "MMC1 powers up with control $0C"
        );
    }
}
