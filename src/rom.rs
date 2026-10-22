//! iNES loader for the three boards this emulator knows: mapper 2 (UxROM)
//! for "Contra (USA)", mapper 66 (UxROM variant with bus conflicts) for
//! "Super Mario Bros. + Duck Hunt (USA)", and mapper 0 (NROM) for blargg's
//! APU test ROMs -- NROM-128 and NROM-256, both with the `$6000-$7FFF` PRG RAM
//! the tests need, and the 2005 suite with no CHR at all (the PPU gets RAM
//! instead).
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

/// The CPU window. NESdev "GxROM" says "CPU $8000-$FFFF: 32 KB switchable
/// PRG ROM bank" -- ONE 32 KiB block over the whole window. There is no
/// separately selectable `$C000-$FFFF` half; `$8000` and `$C000` are both
/// offsets into the same bank, which is why this is 32 KiB and not 16.
const BANK: usize = 0x8000;

/// iNES counts PRG in 16 KiB units, whatever the window size is.
const PRG_UNIT: usize = 0x4000;

/// The bank the register holds until something writes it. Measured on this
/// cartridge: the power-on bytes at `$8000` are
/// `78 A9 11 8D 02 80 4C 00 80` -- SEI / LDA #$11 / STA $8002 / JMP $8000 --
/// and bank 0's own `$8002` IS `$11`, so the write conflicts, the register takes
/// `$11`, bits 5-4 are `$01`, and the CPU moves to bank 1. Any other power-on
/// bank drops the CPU in the middle of an unrelated routine.
const POWER_ON_BANK: usize = 0;

/// One 8 KiB CHR bank: the window at `$0000-$1FFF`, selected by bits 1-0.
const CHR_BANK: usize = 0x2000;

/// PRG ROM, CHR ROM, and the mapper bank register. The CHR bytes stay here
/// because the file owns them; the PPU holds the banks because it fetches
/// pattern memory every dot, and the register writes swap one in for the other.
pub struct Cartridge {
    prg: Vec<u8>,
    chr: Vec<u8>,
    /// Which board this image is, and therefore how `$8000-$FFFF` decodes.
    board: Board,
    /// Which PRG bank is mapped at the switchable window: a 32 KiB bank over
    /// `$8000-$FFFF` on GxROM, a 16 KiB bank at `$8000-$BFFF` on UxROM.
    bank: usize,
    /// Which 8 KiB CHR bank is mapped at `$0000-$1FFF` (GxRom only).
    chr_bank: usize,
}

/// The three boards this emulator knows how to read.
enum Board {
    /// NROM, no banking: 16 KiB PRG mirrors across the window, 32 KiB maps
    /// both halves directly. Writes to the window go nowhere.
    Nrom,
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
        let prg_len = bytes[4] as usize * PRG_UNIT;
        if prg_len == 0 || bytes.len() < 16 + prg_len {
            return Err(format!("truncated PRG: header claims {prg_len} bytes"));
        }
        let mapper = bytes[7] & 0xF0 | bytes[6] >> 4;
        let board = match mapper {
            0 => Board::Nrom,
            2 => Board::UxRom,
            66 => Board::GxRom,
            other => return Err(format!("mapper {other}, only 0, 2 and 66 are supported")),
        };
        // NROM and UxROM start with bank 0. GxROM's power-on bank is measured,
        // and `POWER_ON_BANK % (prg_len / BANK)` would divide by zero for a
        // 16 KiB image.
        let bank = match board {
            Board::Nrom | Board::UxRom => 0,
            Board::GxRom => POWER_ON_BANK % (prg_len / BANK),
        };
        Ok(Cartridge {
            prg: bytes[16..16 + prg_len].to_vec(),
            chr: bytes[16 + prg_len..].to_vec(),
            board,
            bank,
            chr_bank: 0,
        })
    }

    /// Whether `$6000-$7FFF` is populated. Both NROM images this emulator
    /// loads carry the 8 KiB PRG RAM the APU tests use for reporting; neither
    /// the SMB board nor UxROM populates it.
    pub fn has_prg_ram(&self) -> bool {
        matches!(self.board, Board::Nrom)
    }

    /// Whether pattern memory is RAM. True for images that carry no CHR bytes
    /// at all -- the 2005 suite -- where the game uploads its own tiles.
    pub fn chr_is_ram(&self) -> bool {
        self.chr.is_empty()
    }

    /// The CHR ROM image, for the PPU to hold. Borrowed, not copied.
    pub fn chr_rom(&self) -> &[u8] {
        &self.chr
    }

    /// Which 8 KiB CHR bank is mapped at `$0000-$1FFF`.
    pub fn chr_bank(&self) -> usize {
        self.chr_bank
    }

    /// The CPU's `$8000-$FFFF` window. NROM has no register: 16 KiB images
    /// mirror their single block, 32 KiB ones map both halves directly, and
    /// `& (len - 1)` does both. GxROM maps one 32 KiB bank across the whole
    /// window. UxROM maps a 16 KiB bank at `$8000-$BFFF` and pins the last
    /// 16 KiB of the file at `$C000-$FFFF`.
    pub fn cpu_read(&self, addr: u16) -> u8 {
        match self.board {
            Board::Nrom => self.prg[addr as usize & (self.prg.len() - 1)],
            Board::GxRom => self.prg[self.bank * BANK + (addr as usize - 0x8000)],
            Board::UxRom if addr < 0xC000 => {
                self.prg[self.bank * PRG_UNIT + (addr as usize - 0x8000)]
            }
            Board::UxRom => self.prg[self.prg.len() - PRG_UNIT + (addr as usize - 0xC000)],
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
    pub fn cpu_write(&mut self, addr: u16, val: u8) {
        match self.board {
            // NROM has no registers at all: every write to the window is
            // ignored.
            Board::Nrom => {}
            Board::GxRom => {
                // The register decodes `$8000-$BFFF` only. `$C000-$FFFF` is
                // the upper half of the same window, not a second register,
                // and reset writes `$FFFD` there for the stack scratch byte;
                // that byte happens to be the very value written, so a conflict
                // check spanning the whole `$8000-$FFFF` range would take bits
                // 5-4 of `$80` mid-reset.
                if !(0x8000..0xC000).contains(&addr) || val != self.cpu_read(addr) {
                    return;
                }
                // `saturating_sub`, not `-`: the iNES header can claim more PRG
                // or CHR than the file carries, and a bank count of 0 must not
                // underflow.
                self.bank = ((val >> 4) as usize) & (self.prg.len() / BANK).saturating_sub(1);
                self.chr_bank = (val & 3) as usize & (self.chr.len() / CHR_BANK).saturating_sub(1);
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
}

/// 16-byte iNES header for a synthetic image, shared by every image builder in
/// the crate. `mapper` is split the way iNES has it: low nibble in byte 6, high
/// nibble in byte 7. 66 is the SMB + Duck Hunt GxROM board, which this repo also
/// declares vertical mirroring and battery for; 2 is UxROM and 0 is plain NROM.
#[cfg(test)]
pub(crate) fn ines_header(prg_banks: u8, chr_banks: u8, mapper: u8) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[..4].copy_from_slice(b"NES\x1a");
    h[4] = prg_banks;
    h[5] = chr_banks;
    h[6] = (mapper & 0x0F) << 4 | if mapper == 66 { 0x01 } else { 0x00 };
    h[7] = mapper & 0xF0 | if mapper == 66 { 0x08 } else { 0x00 };
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

    /// Two 32 KiB PRG banks, filled with `a` and `b`, and two 8 KiB CHR banks.
    fn two_banks_filled(a: u8, b: u8) -> Cartridge {
        let mut bytes = ines_header(4, 2, 66).to_vec();
        for fill in [a, b] {
            bytes.extend(std::iter::repeat_n(fill, BANK));
        }
        bytes.extend(std::iter::repeat_n(0, CHR_BANK * 2));
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

    /// The conflict check must stop at `$BFFF`. Bank 1's own reset stub writes
    /// `$FFFD` with `$80`, which is the byte the ROM holds there, so a check
    /// spanning `$8000-$FFFF` would bank-switch mid-reset on the second bank.
    #[test]
    fn a_matching_write_to_the_upper_half_never_moves_the_bank() {
        let mut c = two_banks_filled(0xAA, 0xBB);
        c.cpu_write(0x8000, 0xAA); // matches -> bank 0
        let before = c.cpu_read(0xC000);
        c.cpu_write(0xFFFD, c.cpu_read(0xFFFD));
        assert_eq!(c.cpu_read(0xC000), before);
    }

    #[test]
    fn a_bad_image_is_rejected() {
        assert!(Cartridge::load(b"nope").is_err(), "bad magic");
        assert!(Cartridge::load(b"NES\x1a").is_err(), "truncated");
        let mut other_mapper = ines_header(4, 2, 66);
        other_mapper[7] = 0x43; // mapper 4, MMC3
        assert!(Cartridge::load(&other_mapper).is_err(), "wrong mapper");
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
        assert!(c.chr_is_ram(), "Contra carries no CHR bytes");
        assert!(!c.has_prg_ram(), "UxROM has no $6000-$7FFF RAM");
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
        assert!(c.has_prg_ram(), "NROM carries the $6000-$7FFF RAM");
        assert!(c.chr_is_ram(), "no CHR bytes in the file -> CHR RAM");
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
        assert!(!c.chr_is_ram(), "8 KiB of CHR ROM in the file");
        assert!(c.has_prg_ram());
    }

    /// The real test suites must load: apu_reset is NROM-256 with 8 KiB CHR,
    /// apu_2005 is NROM-128 with no CHR at all (CHR RAM).
    #[test]
    fn the_real_test_roms_load() {
        if let Some(bytes) = test_rom("test_roms/apu_reset/4015_cleared.nes") {
            let c = Cartridge::load(&bytes).expect("apu_reset image must load");
            assert_eq!(c.prg.len(), 2 * PRG_UNIT, "32 KiB NROM-256");
            assert!(c.has_prg_ram());
            assert!(!c.chr_is_ram());
            assert_eq!(c.chr_rom().len(), 8192, "8 KiB of CHR ROM");
        }
        if let Some(bytes) = test_rom("test_roms/apu_2005/01.len_ctr.nes") {
            let c = Cartridge::load(&bytes).expect("apu_2005 image must load");
            assert_eq!(c.prg.len(), PRG_UNIT, "16 KiB NROM-128");
            assert!(c.has_prg_ram());
            assert!(c.chr_is_ram(), "no CHR bytes -> the PPU must get RAM");
        }
    }
}
