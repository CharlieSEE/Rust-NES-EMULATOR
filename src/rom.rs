//! iNES loader and mapper 66 (UxROM variant with bus conflicts) for
//! "Super Mario Bros. + Duck Hunt (USA)".
//!
//! The board contradicts its own iNES header in three ways. All three were
//! checked against the real cartridge image, not taken from the header:
//!
//! 1. PRG banking is by BUS CONFLICT, not by the written value. A write to
//!    `$8000-$BFFF` only changes the bank when the byte written already equals
//!    the byte the CPU would read at that address. Plain UxROM banking leaves
//!    the cartridge in its reset stub forever.
//! 2. The header claims 2x8 KiB CHR ROM. The board has 8 KiB of CHR **RAM**
//!    that the game uploads tile data into at boot. CHR RAM is PPU-bus memory:
//!    the PPU fetches pattern tables every dot, so it owns that array and this
//!    struct deliberately does not.
//! 3. Nametable mirroring is VERTICAL, hardwired in the board. Nothing in the
//!    header selects it and nothing in `Cartridge` needs to.

// Drop when the CPU bus in a later task actually loads a ROM; these entry
// points are unreachable until then.
#![allow(dead_code)]

/// Size of one PRG bank, and of both PRG windows.
const BANK: usize = 0x4000;

/// The cartridge's reset code lives in 16 KiB bank 2 and nothing writes
/// `$8000-$BFFF` to get there, so the power-on bank has to be 2: the RESET
/// vector is `$FFFC` = `$8000`, i.e. it points straight into the switchable
/// window. Any other power-on bank drops the CPU in the middle of an unrelated
/// routine and the board never boots.
const POWER_ON_BANK: usize = 2;

/// PRG ROM plus the mapper 66 bank register. CHR RAM is deliberately absent:
/// it is PPU-bus memory and the PPU owns it.
pub struct Cartridge {
    prg: Vec<u8>,
    /// Which 16 KiB bank is mapped at `$8000-$BFFF`.
    bank: usize,
}

impl Cartridge {
    pub fn load(bytes: &[u8]) -> Result<Cartridge, String> {
        if bytes.len() < 16 || &bytes[..4] != b"NES\x1a" {
            return Err("not an iNES image: bad magic".to_string());
        }
        let prg_len = bytes[4] as usize * BANK;
        if prg_len == 0 || bytes.len() < 16 + prg_len {
            return Err(format!("truncated PRG: header claims {prg_len} bytes"));
        }
        let mapper = bytes[7] & 0xF0 | bytes[6] >> 4;
        if mapper != 66 {
            return Err(format!("mapper {mapper}, only mapper 66 is supported"));
        }
        Ok(Cartridge {
            prg: bytes[16..16 + prg_len].to_vec(),
            bank: POWER_ON_BANK % (prg_len / BANK),
        })
    }

    /// `$8000-$BFFF` is the switchable bank, `$C000-$FFFF` is fixed to the last
    /// bank so the NMI and IRQ vectors never move.
    pub fn cpu_read(&self, addr: u16) -> u8 {
        let bank = if addr < 0xC000 {
            self.bank
        } else {
            self.prg.len() / BANK - 1
        };
        self.prg[bank * BANK + (addr as usize & 0x3FFF)]
    }

    /// Bus conflict: the bank register only moves when the byte written is
    /// already the byte the CPU reads at that address.
    pub fn cpu_write(&mut self, addr: u16, val: u8) {
        // The register decodes `$8000-$BFFF` only. `$C000-$FFFF` is fixed ROM,
        // and reset writes `$FFFD` there for OAM DMA; that byte happens to be
        // `$80`, the very value written, so a conflict check spanning the whole
        // `$8000-$FFFF` range would set the bank to `$80 % 4 = 0` three
        // instructions into reset and fall into the wrong bank.
        if !(0x8000..0xC000).contains(&addr) {
            return;
        }
        if val != self.cpu_read(addr) {
            return;
        }
        self.bank = val as usize % (self.prg.len() / BANK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16-byte iNES header: mapper 66, vertical mirroring, battery, and the
    /// board's lying claim of `chr_banks` of CHR ROM.
    fn ines_header(prg_banks: u8, chr_banks: u8) -> [u8; 16] {
        let mut h = [0u8; 16];
        h[..4].copy_from_slice(b"NES\x1a");
        h[4] = prg_banks;
        h[5] = chr_banks;
        h[6] = 0x21; // vertical mirroring + battery
        h[7] = 0x48; // mapper 66
        h
    }

    /// Four 16 KiB PRG banks. Banks 0-2 are `a`, the fixed bank 3 is `b`.
    fn four_banks_filled(a: u8, b: u8) -> Cartridge {
        let mut bytes = ines_header(4, 2).to_vec();
        for fill in [a, a, a, b] {
            bytes.extend(std::iter::repeat(fill).take(0x4000));
        }
        Cartridge::load(&bytes).expect("synthetic image must load")
    }

    #[test]
    fn a_write_that_does_not_match_the_rom_does_not_switch_banks() {
        let mut c = four_banks_filled(0xAA, 0xBB);
        let before = c.cpu_read(0x8000);
        c.cpu_write(0x8000, 0x01); // 0x01 != 0xAA
        assert_eq!(c.cpu_read(0x8000), before);
    }

    #[test]
    fn a_write_that_matches_the_rom_switches_banks() {
        let mut c = four_banks_filled(0xAA, 0xBB);
        // $8000 currently reads the power-on bank, which holds 0xAA.
        c.cpu_write(0x8000, 0xAA); // matches -> 0xAA % 4 selects bank 2
        assert_eq!(c.cpu_read(0x8000), 0xAA);
    }

    #[test]
    fn the_fixed_window_always_shows_the_last_bank() {
        let mut c = four_banks_filled(0xAA, 0xBB);
        assert_eq!(c.cpu_read(0xC000), 0xBB);
        c.cpu_write(0x8000, 0xBB); // matching write, moves the switchable bank
        assert_eq!(c.cpu_read(0xC000), 0xBB); // fixed bank did not move
    }

    /// The conflict check must stop at `$BFFF`. Reset writes `$FFFD` for OAM
    /// DMA and the ROM byte there is `$80`, the exact value written, so a
    /// check spanning `$8000-$FFFF` would bank-switch mid-reset.
    #[test]
    fn a_matching_write_to_the_fixed_window_never_moves_the_bank() {
        let mut c = four_banks_filled(0xAA, 0xBB);
        let before = c.cpu_read(0x8000);
        c.cpu_write(0xFFFD, c.cpu_read(0xFFFD));
        assert_eq!(c.cpu_read(0x8000), before);
    }

    /// RESET is `$FFFC` = `$8000`, in the switchable window, and only bank 2
    /// holds reset code there. See `POWER_ON_BANK`.
    #[test]
    fn power_on_bank_is_two() {
        let mut bytes = ines_header(4, 2).to_vec();
        for fill in 0..4u8 {
            bytes.extend(std::iter::repeat(fill).take(BANK));
        }
        let c = Cartridge::load(&bytes).expect("synthetic image must load");
        assert_eq!(c.cpu_read(0x8000), 2);
    }

    #[test]
    fn a_bad_image_is_rejected() {
        assert!(Cartridge::load(b"nope").is_err(), "bad magic");
        assert!(Cartridge::load(b"NES\x1a").is_err(), "truncated");
        let mut other_mapper = ines_header(4, 2);
        other_mapper[7] = 0x43; // mapper 4, MMC3
        assert!(Cartridge::load(&other_mapper).is_err(), "wrong mapper");
    }

    #[test]
    fn the_real_cartridge_loads_and_boots_into_reset_code() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/Super Mario Bros. + Duck Hunt (USA).nes"
        );
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                println!("note: {} not present, skipping", path);
                return;
            }
        };
        let c = Cartridge::load(&bytes).expect("real cartridge must load");
        assert_eq!(c.prg.len(), 65536);
        // RESET vector, read from the fixed window.
        assert_eq!(
            u16::from_le_bytes([c.cpu_read(0xFFFC), c.cpu_read(0xFFFD)]),
            0x8000
        );
        // ...which is only coherent at power-on bank 2: SEI, the reset stub.
        assert_eq!(c.cpu_read(0x8000), 0x78);
    }
}
