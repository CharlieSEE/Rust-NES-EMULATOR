//! iNES loader and mapper 66 (UxROM variant with bus conflicts) for
//! "Super Mario Bros. + Duck Hunt (USA)".
//!
//! The iNES header is honest about this cartridge: its `chr=2` really is 16 KiB
//! of tile data, its `prg=4` really is 64 KiB, and its mirroring bit really is
//! vertical. What the header does NOT carry is any of the below, so every one
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

/// PRG ROM, CHR ROM, and the mapper 66 bank register. The CHR bytes stay here
/// because the file owns them; the PPU holds the banks because it fetches
/// pattern memory every dot, and the register writes swap one in for the other.
pub struct Cartridge {
    prg: Vec<u8>,
    chr: Vec<u8>,
    /// Which 32 KiB bank is mapped at `$8000-$FFFF`.
    bank: usize,
    /// Which 8 KiB CHR bank is mapped at `$0000-$1FFF`.
    chr_bank: usize,
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
        if mapper != 66 {
            return Err(format!("mapper {mapper}, only mapper 66 is supported"));
        }
        Ok(Cartridge {
            prg: bytes[16..16 + prg_len].to_vec(),
            chr: bytes[16 + prg_len..].to_vec(),
            bank: POWER_ON_BANK % (prg_len / BANK),
            chr_bank: 0,
        })
    }

    /// The CHR ROM image, for the PPU to hold. Borrowed, not copied.
    pub fn chr_rom(&self) -> &[u8] {
        &self.chr
    }

    /// Which 8 KiB CHR bank is mapped at `$0000-$1FFF`.
    pub fn chr_bank(&self) -> usize {
        self.chr_bank
    }

    /// One 32 KiB window: `$8000` is the bank base and `$FFFF` its last byte.
    pub fn cpu_read(&self, addr: u16) -> u8 {
        self.prg[self.bank * BANK + (addr as usize - 0x8000)]
    }

    /// Bus conflict: the bank register only moves when the byte written is
    /// already the byte the CPU reads at that address.
    ///
    /// The register is `xxPPxxCC`, so PRG is BITS 5-4 (NESdev "GxROM": "the
    /// board only supports 64KB PRG", hence two banks and one select bit) and
    /// CHR is bits 1-0.
    pub fn cpu_write(&mut self, addr: u16, val: u8) {
        // The register decodes `$8000-$BFFF` only. `$C000-$FFFF` is the upper
        // half of the same window, not a second register, and reset writes
        // `$FFFD` there for the stack scratch byte; that byte happens to be the
        // very value written, so a conflict check spanning the whole
        // `$8000-$FFFF` range would take bits 5-4 of `$80` mid-reset.
        if !(0x8000..0xC000).contains(&addr) {
            return;
        }
        if val != self.cpu_read(addr) {
            return;
        }
        // `saturating_sub`, not `-`: the iNES header can claim more PRG or CHR
        // than the file carries, and a bank count of 0 must not underflow.
        self.bank = ((val >> 4) as usize) & (self.prg.len() / BANK).saturating_sub(1);
        self.chr_bank = (val & 3) as usize & (self.chr.len() / CHR_BANK).saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16-byte iNES header: mapper 66, vertical mirroring, battery, and 2x8 KiB
    /// of CHR ROM, which is honest -- the file really does carry 16 KiB of
    /// tile data and the register's low two bits bank it.
    fn ines_header(prg_banks: u8, chr_banks: u8) -> [u8; 16] {
        let mut h = [0u8; 16];
        h[..4].copy_from_slice(b"NES\x1a");
        h[4] = prg_banks;
        h[5] = chr_banks;
        h[6] = 0x21; // vertical mirroring + battery
        h[7] = 0x48; // mapper 66
        h
    }

    /// Two 32 KiB PRG banks where bank `i` is filled with the byte `i`, so the
    /// mapped bank is observable through `cpu_read`. `patch` is written into
    /// the power-on bank's `addr` so a matching write carries a value the
    /// power-on bank does not already select.
    fn banks_filled_with_their_own_index(addr: usize, patch: u8) -> Cartridge {
        let mut bytes = ines_header(4, 2).to_vec();
        for fill in 0..2u8 {
            bytes.extend(std::iter::repeat_n(fill, BANK));
        }
        bytes.extend(std::iter::repeat_n(0, CHR_BANK * 2));
        bytes[16 + POWER_ON_BANK * BANK + addr - 0x8000] = patch;
        Cartridge::load(&bytes).expect("synthetic image must load")
    }

    /// Two 32 KiB PRG banks, filled with `a` and `b`, and two 8 KiB CHR banks.
    fn two_banks_filled(a: u8, b: u8) -> Cartridge {
        let mut bytes = ines_header(4, 2).to_vec();
        for fill in [a, b] {
            bytes.extend(std::iter::repeat_n(fill, BANK));
        }
        bytes.extend(std::iter::repeat_n(0, CHR_BANK * 2));
        Cartridge::load(&bytes).expect("synthetic image must load")
    }

    /// The real cartridge, or `None` when it is not on disk: the ROM is
    /// gitignored, so a test that needs its actual bytes has to be able to skip.
    fn real_rom() -> Option<Vec<u8>> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/Super Mario Bros. + Duck Hunt (USA).nes"
        );
        match std::fs::read(path) {
            Ok(b) => Some(b),
            Err(_) => {
                println!("note: {path} not present, skipping");
                None
            }
        }
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
        let mut other_mapper = ines_header(4, 2);
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
}
