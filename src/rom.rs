//! iNES loader for the boards this emulator knows: mapper 1 (MMC1) for
//! "Legend of Zelda", mapper 2 (UxROM) for "Contra", mapper 3 (CNROM) for its
//! large library, mapper 4 (MMC3) for the TxROM library, mapper 7 (AxROM) for
//! "Battletoads" and its siblings, mapper 71 (Camerica/Codemasters) for its
//! unlicensed library, mapper 66 (UxROM variant with bus conflicts) for
//! "Super Mario Bros. + Duck Hunt", mapper 185 (CNROM with CHR-ROM disabled
//! until the copy-protection latch names the chip's select value) for titles
//! such as B-Wings and Mighty Bomb Jack, and mapper 0 (NROM) for blargg's APU
//! test ROMs -- NROM-128 and NROM-256, both with the `$6000-$7FFF` PRG RAM the
//! tests need, and the 2005 suite with no CHR at all (the PPU gets RAM instead).
//!
//! NES 2.0 headers extend those fields: the mapper number and submapper reach
//! 12 bits, byte 9 carries the high four bits of the PRG/CHR sizes, and the
//! sizes can switch to exponent-multiplier form for odd images. The console
//! type is parsed too; only a regular NES/Famicom is modelled, so Vs. System,
//! PlayChoice-10 and extended Famiclone types are refused by name. Byte 10 and
//! byte 11 declare the PRG and CHR RAM sizes, which the bus and PPU honour.
//! Plain iNES ignores all of those bytes and keeps its 8 KiB defaults.
//!
//! Mapper 4 is MMC3: four 8 KiB PRG windows (two switchable, the second-last
//! and last fixed, with bit 6 of `$8000` swapping the switchable pair), 2 KiB
//! and 1 KiB CHR windows selected by R0-R5, runtime mirroring at `$A000`, and
//! an 8 KiB PRG RAM window. Its scanline counter is clocked by filtered rises
//! of PPU A12, which the PPU reports from its real background and sprite fetch
//! addresses (241 a frame with the usual `$0000` background and `$1000`
//! sprites on NTSC).
//!
//! The PRG RAM window is always enabled. iNES mapper 4 also covers MMC6, where
//! `$A001` bits 4-7 individually enable reads and writes to two 512-byte RAM
//! banks instead of acting as one MMC3-style disable, and plain iNES cannot
//! tell the two chips apart. Many emulators keep the whole window enabled for
//! that reason; StarTropics and its sequel are the only commercial MMC6 games.
//! The MMC3 `$A001` write-protect bit is ignored along with the MMC6 control
//! bits, because it only guards save RAM during power-off.
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
//! Mapper 3 is CNROM: fixed PRG, one 8 KiB switchable CHR window at
//! `$0000-$1FFF`, and AND-type bus conflicts. The board's 74HC161 latch sees
//! the written byte ANDed with the PRG byte it drives at the same address.
//! NES 2.0 submapper 1 names mapper hacks with no bus conflicts and submapper
//! 2 the original AND-type board; submapper 0 leaves the behavior unknown, so
//! it keeps the conflict like plain iNES. Regular boards connect only two latch
//! outputs for up to 32 KiB of CHR; the modulo over the image's real bank count
//! also covers the 128 KiB Bandai variant, whose four outputs are all connected.
//!
//! Mapper 185 is CNROM with only 8 KiB of CHR-ROM, whose pins 26 and 27 are
//! chip selects instead of address bits. The latch's low two bits must match
//! the board's wired select value or the CHR chip tri-states: pattern reads are
//! then open bus, which this emulator models as `$FF`, the pulled-up data lines
//! the protection checks expect. NES 2.0 submappers 4-7 name that value as 0-3;
//! every other header (including plain iNES) is unknown, so the board follows
//! NESdev's heuristic: CHR stays disabled for the first two `$2007` reads after
//! reset, then turns on for good. Writes always take the AND-type bus conflict,
//! and there is no CHR banking: all four select values address the same 8 KiB.
//!
//! Mapper 7 is AxROM: one 32 KiB PRG bank over the whole `$8000-$FFFF` window,
//! selected by bits 0-3 of any write in that window (bit 3 only exists on the
//! 512 KiB variant), and bit 4 of the same register selects the 1 KiB VRAM page
//! all four nametables use (0 lower, 1 upper). The board carries 8 KiB of CHR
//! RAM and no PRG RAM. Plain iNES cannot name the board revision, so the model
//! follows NES 2.0 submapper 0's guidance and does not emulate bus conflicts;
//! NES 2.0 submapper 2 (AMROM) asks for AND-type conflicts, and that one is
//! honoured. The ANROM and AN1ROM boards have none.
//!
//! Mapper 71 is Camerica/Codemasters: UxROM's 16 KiB low window and fixed last
//! bank, but the bank register is `xxxx PPPP` at `$C000-$FFFF` only, with no
//! bus conflicts. The boards carry CHR RAM and no PRG RAM. Ordinary boards
//! hardwire mirroring from the header; Fire Hawk's BF9097 wires a one-screen
//! latch over `$8000-$9FFF` bit 4 (NES 2.0 submapper 1). Plain iNES cannot name
//! that board, so the `$9000-$9FFF` subset is the heuristic other emulators
//! use. NES 2.0 submapper 0 keeps the header's fixed mirroring.
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
use crate::state::{rom_fingerprint, StateReader, StateWriter};

/// Bytes of a ROM on disk: a plain image, or the `.nes` inside a single-ROM
/// `.zip`. The archive is unpacked in memory and never written back.
pub fn read_rom(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let display = path.display();
    let zipped = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
    if !zipped {
        return std::fs::read(path).map_err(|e| format!("{display}: {e}"));
    }
    let file = std::fs::File::open(path).map_err(|e| format!("{display}: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("{display}: {e}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| format!("{display}: {e}"))?;
        if !entry.name().to_ascii_lowercase().ends_with(".nes") {
            continue;
        }
        let name = entry.name().to_string();
        // The entry's claimed size is archive metadata, so it is not trusted
        // for a capacity hint; `read_to_end` grows to the real length.
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes)
            .map_err(|e| format!("{display}: {name}: {e}"))?;
        return Ok(bytes);
    }
    Err(format!("{display}: no .nes inside the zip"))
}

/// The console a NES 2.0 header names. Plain iNES has no console field, so
/// those images are always `Nes`. Only a regular NES/Famicom is modelled.
enum Console {
    Nes,
    VsSystem,
    PlayChoice10,
    /// Console type 3: byte 13's low nibble names the extended type.
    Extended(u8),
}

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

/// Bytes named by an iNES/NES 2.0 size field. `lsb` is the old 8-bit count in
/// `unit`s and `msb` is byte 9's nibble. A nibble of `$F` selects NES 2.0's
/// exponent-multiplier form, where `lsb` is `EEEE EEMM` and the size is
/// `2^E * (2*M + 1)` bytes. `None` means the header's exponent does not fit in
/// a `usize`, which no real file can satisfy anyway.
fn rom_size(lsb: u8, msb: u8, unit: usize) -> Option<usize> {
    if msb == 0x0F {
        let exponent = (lsb >> 2) as u32;
        let multiplier = usize::from(lsb & 0x03) * 2 + 1;
        (1usize.checked_shl(exponent)?).checked_mul(multiplier)
    } else {
        Some(((usize::from(msb) << 8) | usize::from(lsb)) * unit)
    }
}

/// PRG ROM, CHR ROM, and the mapper registers. The CHR bytes stay here
/// because the file owns them; the PPU holds the banks because it fetches
/// pattern memory every dot, and the registers swap one in for the other.
#[derive(Clone)]
pub struct Cartridge {
    prg: Vec<u8>,
    chr: Vec<u8>,
    /// The 16-byte iNES header. Kept so the save-state fingerprint covers
    /// header-only differences such as the power-on mirroring and battery flag.
    header: [u8; 16],
    /// Which board this image is, and therefore how `$8000-$FFFF` decodes.
    board: Board,
    /// NES 2.0 submapper, 0 for plain iNES. Mapper 3 uses it to choose
    /// no bus conflicts (submapper 1) or AND-type (submapper 2), mapper 7
    /// to choose AND-type bus conflicts (submapper 2), and mapper 185 to name
    /// the CHR chip's select value (submappers 4-7 mean 0-3).
    submapper: u8,
    /// Which PRG bank is mapped at the switchable window: a 32 KiB bank over
    /// `$8000-$FFFF` on GxROM and AxROM, a 16 KiB bank at `$8000-$BFFF` on
    /// UxROM and Camerica, and MMC1's PRG register in 16 KiB units whose
    /// meaning follows `control`. NROM and CNROM have no PRG bank register, so
    /// it stays 0.
    bank: usize,
    /// The two 4 KiB CHR banks the board maps at `$0000-$0FFF` and
    /// `$1000-$1FFF`. GxROM's and CNROM's 8 KiB value fills both; MMC1 selects
    /// them separately in 4 KiB mode.
    chr_banks: [usize; 2],
    /// Mapper 185's copy-protection latch. Its low two bits are the CHR chip's
    /// select inputs; every other board leaves it 0.
    latch: u8,
    /// Whether the pattern memory may drive the PPU data bus. True everywhere
    /// except mapper 185, whose CHR chip tri-states until the latch names the
    /// wired select value or the unknown-submapper heuristic has run.
    chr_enabled: bool,
    /// Mapper 185's unknown-submapper heuristic: `$2007` reads since reset,
    /// capped at 2. The second read turns CHR on for the rest of the session.
    ppu_reads: u8,
    /// MMC1 control register. Power-on/reset value is PRG mode 3 (switch at
    /// `$8000`, last bank fixed), 8 KiB CHR mode, and one-screen lower
    /// nametable mirroring (control bits 0-1 are 0).
    control: u8,
    /// MMC1's five-write serial port: bits received so far and how many.
    shift: u8,
    shift_count: u8,
    /// MMC1B PRG register bit 4: false disables `$6000-$7FFF` until the game
    /// writes a value with the bit clear. Power-on is enabled. MMC3 ignores
    /// this field because its always-enabled window has to serve MMC6 too.
    prg_ram_enabled: bool,
    /// MMC3's register file. Defaults for every other board.
    mmc3: Mmc3,
    /// How the board wires the four logical nametables onto 2 KiB of VRAM.
    mirroring: Mirroring,
    /// Whether the header marks PRG RAM as battery-backed, so it should be
    /// written to a `.sav` file next to the ROM.
    battery: bool,
    /// Which console timing the header asks for.
    region: Region,
}

/// The eight boards this emulator knows how to read.
#[derive(Clone, Copy)]
enum Board {
    /// NROM, no banking: 16 KiB PRG mirrors across the window, 32 KiB maps
    /// both halves directly. Writes to the window go nowhere.
    Nrom,
    /// CNROM, mapper 3: fixed PRG, one 8 KiB switchable CHR bank, and bus
    /// conflicts on the register write (AND-type unless NES 2.0 submapper 1
    /// names a mapper hack without them).
    CnRom,
    /// CNROM with a copy-protected 8 KiB CHR chip, mapper 185: the latch's low
    /// two bits must match the board's select value or pattern reads are open
    /// bus (see the module docs).
    CnRom185,
    /// MMC1, the Zelda board: serial PRG/CHR banking, mirroring control, and
    /// battery-backed PRG RAM.
    Mmc1,
    /// GxROM, the SMB + Duck Hunt board: bus-conflict banking, see above.
    GxRom,
    /// UxROM, the Contra board: 16 KiB switchable low window, last 16 KiB
    /// fixed high window, and no CHR banking.
    UxRom,
    /// Camerica/Codemasters, mapper 71: UxROM banking with the register in
    /// `$C000-$FFFF` only, plus Fire Hawk's mirroring latch.
    Camerica,
    /// AxROM, the Battletoads board: one 32 KiB bank over the whole PRG
    /// window, one-screen mirroring selected by the same register, CHR RAM.
    AxRom,
    /// MMC3, the TxROM board: 8 KiB PRG windows, 1-2 KiB CHR windows,
    /// switchable mirroring and a scanline IRQ counter.
    Mmc3,
}

/// Whether a PRG image of `len` bytes can be mapped by `board` without
/// indexing outside it. NROM/CNROM mirror a power-of-two image; the banking
/// boards need a whole number of their fixed bank window.
fn prg_size_supported(board: Board, len: usize) -> bool {
    match board {
        Board::Nrom | Board::CnRom | Board::CnRom185 => len.is_power_of_two(),
        Board::Mmc1 | Board::UxRom | Board::Camerica => {
            len >= PRG_UNIT && len.is_multiple_of(PRG_UNIT)
        }
        Board::Mmc3 => len >= 0x2000 && len.is_multiple_of(0x2000),
        // GxROM's register masks its bank number with the bank count minus
        // one, so only a power-of-two number of 32 KiB banks wraps correctly.
        Board::GxRom => len >= BANK && len.is_multiple_of(BANK) && (len / BANK).is_power_of_two(),
        Board::AxRom => len >= BANK && len.is_multiple_of(BANK),
    }
}

/// MMC3's write-only register file. Kept apart from the boards that do not use
/// it so their state stays exactly as it was.
#[derive(Clone, Default)]
struct Mmc3 {
    /// `$8000`: low three bits pick the next bank register, bit 6 swaps the PRG
    /// windows at `$8000`/`$C000`, bit 7 swaps the CHR 2 KiB/1 KiB halves.
    bank_select: u8,
    /// R0-R7, written through `$8001`.
    regs: [u8; 8],
    irq_latch: u8,
    irq_counter: u8,
    /// `$C001` asked for a reload on the next A12 rise.
    irq_reload: bool,
    irq_enabled: bool,
    irq_pending: bool,
}

impl Cartridge {
    pub fn load(bytes: &[u8]) -> Result<Cartridge, String> {
        if bytes.len() < 16 || &bytes[..4] != b"NES\x1a" {
            return Err("not an iNES image: bad magic".to_string());
        }
        let mut header = [0u8; 16];
        header.copy_from_slice(&bytes[..16]);
        let flags6 = bytes[6];
        // A trainer is 512 bytes between the header and PRG. Rare, but reading
        // it as PRG puts every bank off by 32 KiB, which looks like a bad ROM.
        let start = 16 + if flags6 & 0x04 != 0 { 512 } else { 0 };
        let flags7 = bytes[7];
        let nes20 = flags7 & 0x0C == 0x08;
        // NES 2.0 puts the high four bits of each ROM size in byte 9 and
        // allows exponent-multiplier sizes. Plain iNES ignores that byte.
        let (prg_msb, chr_msb) = if nes20 {
            (bytes[9] & 0x0F, bytes[9] >> 4)
        } else {
            (0, 0)
        };
        let prg_len = rom_size(bytes[4], prg_msb, PRG_UNIT)
            .ok_or_else(|| "NES 2.0 header's PRG size is out of range".to_string())?;
        if prg_len == 0 || bytes.len() < start + prg_len {
            return Err(format!("truncated PRG: header claims {prg_len} bytes"));
        }
        let chr_len = rom_size(bytes[5], chr_msb, CHR_BANK)
            .ok_or_else(|| "NES 2.0 header's CHR size is out of range".to_string())?;
        if bytes.len() < start + prg_len + chr_len {
            return Err(format!("truncated CHR: header claims {chr_len} bytes"));
        }
        // NES 2.0 marks itself in flags 7 bits 2-3 and puts the CPU/PPU timing
        // in byte 12: 0 NTSC, 1 licensed PAL, 2 multiple-region, 3 Dendy. A
        // plain iNES header only has flags 9 bit 0 and flags 10 bits 0-1, and
        // those bytes are padding in every other header variant -- including
        // the "DiskDude!" corruption, whose 's' in flags 9 would otherwise
        // claim PAL. NESdev's detection requires flags 7 bits 2-3 clear AND
        // bytes 12-15 zero before iNES fields are trusted; a ripper signature
        // in the tail can leave anything in flags 9/10.
        //
        // NES 2.0 also extends the mapper number to 12 bits in byte 8's low
        // nibble and puts the submapper in its high nibble. iNES leaves byte 8
        // as padding, so only trust it once the signature says NES 2.0.
        let mapper = if nes20 {
            u16::from(bytes[8] & 0x0F) << 8 | u16::from(flags7 & 0xF0) | u16::from(bytes[6] >> 4)
        } else {
            u16::from(flags7 & 0xF0 | bytes[6] >> 4)
        };
        let submapper = if nes20 { bytes[8] >> 4 } else { 0 };
        // Byte 7 bits 0-1 name the console; value 3 moves the real type to
        // byte 13. Vs. System and PlayChoice-10 have different PPU palettes
        // and registers, and the extended types are Famiclones, so refuse
        // them instead of running them as a stock NES.
        let console = if nes20 {
            match flags7 & 0x03 {
                0 => Console::Nes,
                1 => Console::VsSystem,
                2 => Console::PlayChoice10,
                _ => Console::Extended(bytes[13] & 0x0F),
            }
        } else {
            Console::Nes
        };
        match console {
            Console::Nes | Console::Extended(0) => {}
            Console::VsSystem => {
                return Err("NES 2.0 Vs. System console is not supported".to_string());
            }
            Console::PlayChoice10 => {
                return Err("NES 2.0 PlayChoice-10 console is not supported".to_string());
            }
            Console::Extended(kind) => {
                return Err(format!(
                    "NES 2.0 extended console type {kind} is not supported"
                ));
            }
        }
        // ponytail: Dendy and multiple-region fall back to NTSC. Both are a
        // mixed clock this emulator has no model for; add a Region variant
        // when a Dendy game turns up.
        let plain_ines = flags7 & 0x0C == 0x00 && bytes[12..16] == [0; 4];
        let region = if nes20 {
            match bytes[12] & 0x03 {
                1 => Region::Pal,
                _ => Region::Ntsc,
            }
        } else if plain_ines && (bytes[9] & 0x01 != 0 || bytes[10] & 0x03 == 0x02) {
            Region::Pal
        } else {
            Region::Ntsc
        };
        let board = match mapper {
            0 => Board::Nrom,
            1 => Board::Mmc1,
            2 => Board::UxRom,
            3 => Board::CnRom,
            4 => Board::Mmc3,
            7 => Board::AxRom,
            66 => Board::GxRom,
            71 => Board::Camerica,
            185 => Board::CnRom185,
            other => {
                return Err(format!(
                    "mapper {other}, only 0, 1, 2, 3, 4, 7, 66, 71 and 185 are supported"
                ));
            }
        };
        if !prg_size_supported(board, prg_len) {
            return Err(format!(
                "mapper {mapper} cannot map {prg_len} bytes of PRG ROM"
            ));
        }
        // NROM, CNROM, MMC1, MMC3 and UxROM start with bank 0. GxROM's power-on
        // bank is measured, and `POWER_ON_BANK % (prg_len / BANK)` would divide
        // by zero for a 16 KiB image.
        let bank = match board {
            Board::Nrom
            | Board::CnRom
            | Board::CnRom185
            | Board::Mmc1
            | Board::Mmc3
            | Board::UxRom
            | Board::Camerica
            | Board::AxRom => 0,
            Board::GxRom => POWER_ON_BANK % (prg_len / BANK).max(1),
        };
        // MMC1's control register powers up at $0C: PRG mode 3, 8 KiB CHR
        // mode, and one-screen lower nametable arrangement. AxROM's bank
        // register powers up at 0, which also selects the lower nametable
        // page. GxROM is hardwired vertical. The rest follow the header bit;
        // MMC3's `$A000` overrides it as soon as the game writes the register.
        let mirroring = match board {
            Board::Mmc1 | Board::AxRom => Mirroring::SingleLower,
            Board::GxRom => Mirroring::Vertical,
            Board::Nrom
            | Board::CnRom
            | Board::CnRom185
            | Board::Mmc3
            | Board::UxRom
            | Board::Camerica
                if flags6 & 0x01 != 0 =>
            {
                Mirroring::Vertical
            }
            Board::Nrom
            | Board::CnRom
            | Board::CnRom185
            | Board::Mmc3
            | Board::UxRom
            | Board::Camerica => Mirroring::Horizontal,
        };
        // Mapper 185's 74HC161 powers up at 0, so only the board whose select
        // value is 0 (NES 2.0 submapper 4) has CHR enabled before the game
        // writes the latch. Every other board always drives its pattern data.
        let chr_enabled = match board {
            Board::CnRom185 => submapper == 4,
            _ => true,
        };
        Ok(Cartridge {
            prg: bytes[start..start + prg_len].to_vec(),
            chr: bytes[start + prg_len..start + prg_len + chr_len].to_vec(),
            header,
            board,
            submapper,
            bank,
            chr_banks: [0, 1],
            latch: 0,
            chr_enabled,
            ppu_reads: 0,
            control: 0x0C,
            shift: 0,
            shift_count: 0,
            prg_ram_enabled: true,
            mmc3: Mmc3::default(),
            mirroring,
            battery: flags6 & 0x02 != 0,
            region,
        })
    }

    /// Whether the CPU can reach PRG RAM right now. NROM's test images and
    /// MMC1's battery save both use the window; the standard CNROM board
    /// modeled here, SMB, Contra and AxROM do not. NROM has no register, while
    /// MMC1's PRG register bit 4 is MMC1B's disable bit: writes with it set make
    /// `$6000-$7FFF` open bus until the game clears it again. MMC3's window is
    /// always enabled so MMC6 boards, which give `$A001` bit 7 a different
    /// meaning, still work under iNES.
    pub fn prg_ram_enabled(&self) -> bool {
        // A NES 2.0 header names the RAM size, and zero means the board has
        // none. Plain iNES has no size field, so its boards keep the historical
        // defaults below.
        if self.is_nes20() {
            if self.prg_ram_size() + self.prg_nvram_size() == 0 {
                return false;
            }
            return match self.board {
                // MMC1B's register can still close the window at runtime.
                Board::Mmc1 => self.prg_ram_enabled,
                _ => true,
            };
        }
        match self.board {
            Board::Mmc3 => true,
            Board::Nrom | Board::Mmc1 => self.prg_ram_enabled,
            Board::UxRom
            | Board::Camerica
            | Board::GxRom
            | Board::CnRom
            | Board::CnRom185
            | Board::AxRom => false,
        }
    }

    /// Whether the cartridge has battery-backed PRG RAM to persist.
    pub fn has_battery(&self) -> bool {
        self.battery
    }

    /// The console timing the header selected.
    pub fn region(&self) -> Region {
        self.region
    }

    /// Whether the header carries the NES 2.0 signature.
    pub fn is_nes20(&self) -> bool {
        self.header[7] & 0x0C == 0x08
    }

    /// NES 2.0's RAM size fields. Zero means the memory is absent; plain iNES
    /// has no such fields, so all four are zero there.
    pub fn prg_ram_size(&self) -> usize {
        self.ram_size(10, false)
    }

    pub fn prg_nvram_size(&self) -> usize {
        self.ram_size(10, true)
    }

    pub fn chr_ram_size(&self) -> usize {
        self.ram_size(11, false)
    }

    pub fn chr_nvram_size(&self) -> usize {
        self.ram_size(11, true)
    }

    /// A NES 2.0 RAM size nibble is a shift count: `64 << count` bytes.
    fn ram_size(&self, index: usize, upper: bool) -> usize {
        if !self.is_nes20() {
            return 0;
        }
        let shift = if upper {
            self.header[index] >> 4
        } else {
            self.header[index] & 0x0F
        };
        if shift == 0 {
            0
        } else {
            64usize << shift
        }
    }

    /// iNES mapper number, used in the save-state header so a state is never
    /// offered to a board that decodes the same writes differently.
    pub(crate) fn mapper_id(&self) -> u8 {
        match self.board {
            Board::Nrom => 0,
            Board::Mmc1 => 1,
            Board::UxRom => 2,
            Board::CnRom => 3,
            Board::Mmc3 => 4,
            Board::AxRom => 7,
            Board::GxRom => 66,
            Board::Camerica => 71,
            Board::CnRom185 => 185,
        }
    }

    /// Identity of the loaded image, not its mapper state. Covers the iNES
    /// header as well as PRG/CHR, so header-only differences such as mirroring
    /// and battery cannot share a save state. The state's header carries this
    /// and refuses to restore an identical-looking file from a different dump.
    pub(crate) fn fingerprint(&self) -> u64 {
        rom_fingerprint(&self.header, &self.prg, &self.chr)
    }

    /// The mapper's own registers. PRG/CHR bytes, the board type, battery flag
    /// and region are already implied by the ROM the state is checked against.
    pub(crate) fn save_state(&self, writer: &mut StateWriter) {
        writer.u32(self.bank as u32);
        writer.u8(self.chr_banks[0] as u8);
        writer.u8(self.chr_banks[1] as u8);
        writer.u8(self.control);
        writer.u8(self.shift);
        writer.u8(self.shift_count);
        writer.bool(self.prg_ram_enabled);
        writer.u8(self.mirroring.id());
        // MMC3 has more registers than the other boards, and none of its states
        // predate the appended fields, so the common layout stays unchanged.
        if matches!(self.board, Board::Mmc3) {
            writer.u8(self.mmc3.bank_select);
            writer.bytes(&self.mmc3.regs);
            writer.u8(self.mmc3.irq_latch);
            writer.u8(self.mmc3.irq_counter);
            writer.bool(self.mmc3.irq_reload);
            writer.bool(self.mmc3.irq_enabled);
            writer.bool(self.mmc3.irq_pending);
        }
        // The copy-protection latch and the heuristic's read count are state,
        // not ROM identity: a quicksave made during the check must resume it.
        if matches!(self.board, Board::CnRom185) {
            writer.u8(self.latch);
            writer.bool(self.chr_enabled);
            writer.u8(self.ppu_reads);
        }
    }

    pub(crate) fn load_state(&mut self, reader: &mut StateReader) -> Result<(), String> {
        let bank = reader.u32()? as usize;
        let banks = match self.board {
            // NROM and CNROM have no PRG bank register; bank 0 is the only
            // legal state.
            Board::Nrom | Board::CnRom | Board::CnRom185 => 1,
            // MMC1's PRG register is four bits, and a value past the ROM's
            // count wraps in `mmc1_prg_banks`, exactly as on hardware.
            Board::Mmc1 => 16,
            Board::UxRom | Board::Camerica => (self.prg.len() / PRG_UNIT).max(1),
            Board::Mmc3 => 1,
            Board::GxRom | Board::AxRom => (self.prg.len() / BANK).max(1),
        };
        if bank >= banks {
            return Err(format!(
                "save state has PRG bank {bank}; this cartridge has {banks}"
            ));
        }
        self.bank = bank;
        let low = reader.u8()? as usize;
        let high = reader.u8()? as usize;
        self.chr_banks = [low, high];
        if matches!(self.board, Board::Mmc1) && (low > 0x1F || high > 0x1F) {
            return Err(format!(
                "save state has MMC1 CHR banks {low} and {high}; the register holds five bits"
            ));
        }
        self.control = reader.u8()?;
        if self.control & !0x1F != 0 {
            return Err(format!(
                "save state has MMC1 control ${:02X}; the register holds five bits",
                self.control
            ));
        }
        self.shift = reader.u8()?;
        self.shift_count = reader.u8()?;
        if self.shift_count > 4 || self.shift >> self.shift_count != 0 {
            return Err("save state has an invalid MMC1 shift register".to_string());
        }
        self.prg_ram_enabled = reader.bool()?;
        self.mirroring = Mirroring::from_id(reader.u8()?)?;
        if matches!(self.board, Board::Mmc3) {
            let bank_select = reader.u8()?;
            if bank_select & !0xC7 != 0 {
                return Err(format!(
                    "save state has MMC3 bank select ${bank_select:02X}; only bits 7, 6 and 2-0 are used"
                ));
            }
            self.mmc3.bank_select = bank_select;
            self.mmc3.regs = reader.array()?;
            self.mmc3.irq_latch = reader.u8()?;
            self.mmc3.irq_counter = reader.u8()?;
            self.mmc3.irq_reload = reader.bool()?;
            self.mmc3.irq_enabled = reader.bool()?;
            self.mmc3.irq_pending = reader.bool()?;
        }
        if matches!(self.board, Board::CnRom185) {
            let latch = reader.u8()?;
            if latch & 0xF0 != 0 {
                return Err(format!(
                    "save state has mapper 185 latch ${latch:02X}; the register is four bits"
                ));
            }
            self.latch = latch;
            self.chr_enabled = reader.bool()?;
            let reads = reader.u8()?;
            if reads > 2 {
                return Err(format!(
                    "save state has {reads} mapper 185 CHR reads; the heuristic stops at two"
                ));
            }
            self.ppu_reads = reads;
        }
        Ok(())
    }

    /// The CHR ROM image, for the PPU to hold. Borrowed, not copied.
    pub fn chr_rom(&self) -> &[u8] {
        &self.chr
    }

    /// Whether the board's CHR chip is currently driving the PPU data bus.
    /// True everywhere except mapper 185, where the copy-protection latch
    /// tri-states it until the correct select value is written (or the
    /// unknown-submapper heuristic completes).
    pub fn chr_enabled(&self) -> bool {
        self.chr_enabled
    }

    /// Mapper 185's wired CHR select value, when NES 2.0 names it. Submappers
    /// 4-7 mean 0-3; plain iNES and every other value leave it unknown.
    fn chr_select(&self) -> Option<u8> {
        match self.submapper {
            4..=7 => Some(self.submapper - 4),
            _ => None,
        }
    }

    /// One CPU `$2007` read. With an unknown mapper-185 select value, NESdev's
    /// heuristic returns open bus for the first two reads after reset and
    /// enables CHR for good afterwards. Returns true when this read is one of
    /// those two open-bus reads. Every other board ignores the read.
    pub fn note_ppu_read(&mut self) -> bool {
        if !matches!(self.board, Board::CnRom185) || self.chr_select().is_some() {
            return false;
        }
        if self.ppu_reads < 2 {
            self.ppu_reads += 1;
            self.chr_enabled = self.ppu_reads == 2;
            return true;
        }
        false
    }

    /// Power-on/reset state for the register boards. The 74HC161 latch powers
    /// up cleared, so mapper 185 has CHR enabled only when the board's select
    /// value is 0 (NES 2.0 submapper 4), and its read heuristic restarts.
    /// Every other board is unaffected by a reset.
    pub fn reset(&mut self) {
        if matches!(self.board, Board::CnRom185) {
            self.latch = 0;
            self.ppu_reads = 0;
            self.chr_enabled = self.chr_select() == Some(0);
        }
    }

    /// The eight 1 KiB CHR slots at `$0000-$1FFF`. Slots 0-3 are the first
    /// 4 KiB bank and slots 4-7 the second; MMC1's 8 KiB mode points both
    /// halves at one aligned pair.
    pub fn chr_map(&self) -> [usize; 8] {
        if matches!(self.board, Board::Mmc3) {
            return self.mmc3_chr_map();
        }
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

    /// MMC3's eight 1 KiB CHR slots. R0/R1 are 2 KiB banks (their low bit is
    /// ignored); bit 7 of `$8000` swaps the 2 KiB and 1 KiB halves.
    fn mmc3_chr_map(&self) -> [usize; 8] {
        let count = (self.chr.len() / 0x400).max(8);
        let one = |bank: u8| bank as usize % count;
        let pair = |bank: u8| {
            let base = bank as usize & !1;
            [base % count, (base + 1) % count]
        };
        let [r0a, r0b] = pair(self.mmc3.regs[0]);
        let [r1a, r1b] = pair(self.mmc3.regs[1]);
        let [r2, r3, r4, r5] = [
            one(self.mmc3.regs[2]),
            one(self.mmc3.regs[3]),
            one(self.mmc3.regs[4]),
            one(self.mmc3.regs[5]),
        ];
        if self.mmc3.bank_select & 0x80 == 0 {
            [r0a, r0b, r1a, r1b, r2, r3, r4, r5]
        } else {
            [r2, r3, r4, r5, r0a, r0b, r1a, r1b]
        }
    }

    /// How the board currently wires the nametables.
    pub fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// One MMC3 scanline-counter clock, the rising edge of PPU A12. The bus
    /// calls this once per rendering scanline; other boards ignore it.
    pub fn clock_irq(&mut self) {
        if !matches!(self.board, Board::Mmc3) {
            return;
        }
        if self.mmc3.irq_counter == 0 || self.mmc3.irq_reload {
            self.mmc3.irq_counter = self.mmc3.irq_latch;
            self.mmc3.irq_reload = false;
        } else {
            self.mmc3.irq_counter -= 1;
        }
        if self.mmc3.irq_counter == 0 && self.mmc3.irq_enabled {
            self.mmc3.irq_pending = true;
        }
    }

    /// Whether the cartridge is holding `/IRQ` low. Only MMC3 has a mapper IRQ.
    pub fn irq_pending(&self) -> bool {
        matches!(self.board, Board::Mmc3) && self.mmc3.irq_pending
    }

    /// The CPU's `$8000-$FFFF` window. NROM, CNROM and its mapper-185 variant
    /// have no PRG bank register: 16 KiB images mirror their single block, 32
    /// KiB ones map both halves directly, and `& (len - 1)` does both. GxROM
    /// and AxROM map one 32 KiB bank across the whole window. UxROM and
    /// Camerica map a 16 KiB bank at `$8000-$BFFF` and pin the last 16 KiB of
    /// the file at `$C000-$FFFF`. MMC1 follows its PRG mode.
    pub fn cpu_read(&self, addr: u16) -> u8 {
        match self.board {
            Board::Nrom | Board::CnRom | Board::CnRom185 => {
                self.prg[addr as usize & (self.prg.len() - 1)]
            }
            Board::GxRom | Board::AxRom => self.prg[self.bank * BANK + (addr as usize - 0x8000)],
            Board::UxRom | Board::Camerica if addr < 0xC000 => {
                self.prg[self.bank * PRG_UNIT + (addr as usize - 0x8000)]
            }
            Board::UxRom | Board::Camerica => {
                self.prg[self.prg.len() - PRG_UNIT + (addr as usize - 0xC000)]
            }
            Board::Mmc1 => {
                let (low, high) = self.mmc1_prg_banks();
                let bank = if addr < 0xC000 { low } else { high };
                self.prg[bank * PRG_UNIT + (addr as usize & 0x3FFF)]
            }
            Board::Mmc3 => {
                let window = (addr as usize - 0x8000) / 0x2000;
                let bank = self.mmc3_prg_banks()[window];
                self.prg[bank * 0x2000 + (addr as usize & 0x1FFF)]
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

    /// The four 8 KiB PRG banks MMC3 maps at `$8000`, `$A000`, `$C000` and
    /// `$E000`. Bit 6 of `$8000` swaps the two switchable windows; `$E000`
    /// always holds the last bank.
    fn mmc3_prg_banks(&self) -> [usize; 4] {
        let count = (self.prg.len() / 0x2000).max(1);
        let r6 = self.mmc3.regs[6] as usize % count;
        let r7 = self.mmc3.regs[7] as usize % count;
        let last = count - 1;
        let second_last = count.saturating_sub(2);
        if self.mmc3.bank_select & 0x40 == 0 {
            [r6, r7, second_last, last]
        } else {
            [second_last, r7, r6, last]
        }
    }

    /// Mapper register writes.
    ///
    /// CNROM's register is one 8 KiB CHR bank latch over the whole
    /// `$8000-$FFFF` window. Its bus conflict is AND-type: the latch sees the
    /// written byte ANDed with the PRG byte the cartridge drives there.
    /// NES 2.0 submapper 1 names the mapper hacks with no bus conflicts, so
    /// those take the written byte whole; submapper 2 and the unknown
    /// submapper 0 keep the conflict. The 74HC161's four outputs select up to
    /// sixteen banks; regular 32 KiB boards wire only two, which the modulo
    /// over the image's real bank count covers.
    ///
    /// Mapper 185 is the same latch, always with AND-type bus conflicts, but
    /// its low two outputs are the CHR chip's select inputs instead of address
    /// bits. NES 2.0 submappers 4-7 name the one value that enables CHR; an
    /// unknown value leaves CHR disabled until the first-two-$2007-reads
    /// heuristic in `note_ppu_read` enables it. The latch itself never selects
    /// a CHR bank: the board mounts exactly one 8 KiB bank.
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
    /// AxROM selects one 32 KiB bank from bits 0-3 (bit 3 only on the 512 KiB
    /// variant) and the one-screen nametable page from bit 4 on any write in
    /// `$8000-$FFFF`. The bank counter wraps modulo the image's bank count.
    /// Plain iNES uses the submapper-0 rule and has no bus conflicts; NES 2.0
    /// submapper 2 ANDs the written value with the PRG byte at that address.
    ///
    /// Camerica's bank register is `xxxx PPPP` at `$C000-$FFFF` only, with no
    /// bus conflicts; the last 16 KiB stays fixed. Fire Hawk's mirroring latch
    /// covers `$8000-$9FFF` for NES 2.0 submapper 1 and `$9000-$9FFF` for the
    /// plain-iNES heuristic, bit 4 for the one-screen page. Submapper 0 keeps
    /// the header mirroring, so writes elsewhere in the PRG window change
    /// nothing.
    ///
    /// MMC1 receives five serial bits per register write, least significant
    /// first; bit 7 of a write resets the shift register instead.
    pub fn cpu_write(&mut self, addr: u16, val: u8) {
        match self.board {
            // NROM has no registers at all: every write to the window is
            // ignored.
            Board::Nrom => {}
            Board::CnRom => {
                if addr < 0x8000 {
                    return;
                }
                // The original board ANDs the write with the PRG byte at that
                // address. NES 2.0 submapper 1 names the mapper hacks that
                // have no bus conflicts; submapper 2 (and the unknown
                // submapper 0, like plain iNES) keeps the conflict.
                let value = if self.submapper == 1 {
                    val
                } else {
                    val & self.cpu_read(addr)
                };
                let banks = (self.chr.len() / CHR_BANK).max(1);
                let bank = (value & 0x0F) as usize % banks;
                self.chr_banks = [bank * 2, bank * 2 + 1];
            }
            Board::CnRom185 => {
                if addr < 0x8000 {
                    return;
                }
                // The copy-protected board always wires the AND-type conflict:
                // the 74HC161 sees the written byte ANDed with the byte the
                // fixed PRG ROM drives at that address.
                self.latch = (val & self.cpu_read(addr)) & 0x0F;
                if let Some(select) = self.chr_select() {
                    self.chr_enabled = self.latch & 0x03 == select;
                }
            }
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
            Board::Camerica => {
                // BF9093/9097: bank select is `xxxx PPPP` in `$C000-$FFFF`
                // only, with no bus conflicts. The low window moves; the last
                // 16 KiB stays fixed. Writes to `$8000` on startup (Micro
                // Machines, Ultimate Stuntman) must not bank.
                if addr >= 0xC000 {
                    self.bank = (val & 0x0F) as usize % (self.prg.len() / PRG_UNIT);
                }
                // Fire Hawk's BF9097 decodes the mirroring latch over
                // `$8000-$9FFF`; NES 2.0 submapper 1 names the board. Old iNES
                // dumps cannot, so the `$9000-$9FFF` subset is the heuristic
                // other emulators use. Submapper 0 boards keep the header's
                // fixed mirroring, and their startup `$8000` writes must not
                // change it.
                let nes20 = self.header[7] & 0x0C == 0x08;
                let fire_hawk_write = if nes20 {
                    self.submapper == 1 && (0x8000..0xA000).contains(&addr)
                } else {
                    (0x9000..0xA000).contains(&addr)
                };
                if fire_hawk_write {
                    self.mirroring = if val & 0x10 != 0 {
                        Mirroring::SingleUpper
                    } else {
                        Mirroring::SingleLower
                    };
                }
            }
            Board::AxRom => {
                if addr >= 0x8000 {
                    // Register: xxxM PPPP on the 512 KiB variant; standard
                    // boards leave bit 3 unused. The latch sees the written
                    // value ANDed with the PRG byte at that address only on
                    // NES 2.0 submapper 2 (AMROM), which is the only AxROM
                    // submapper that has bus conflicts.
                    let value = if self.submapper == 2 {
                        val & self.cpu_read(addr)
                    } else {
                        val
                    };
                    self.bank = (value & 0x0F) as usize % (self.prg.len() / BANK).max(1);
                    self.mirroring = if value & 0x10 != 0 {
                        Mirroring::SingleUpper
                    } else {
                        Mirroring::SingleLower
                    };
                }
            }
            Board::Mmc3 => self.mmc3_write(addr, val),
        }
    }

    fn mmc3_write(&mut self, addr: u16, val: u8) {
        let odd = addr & 1 != 0;
        match addr & 0xE000 {
            0x8000 if odd => {
                let register = (self.mmc3.bank_select & 0x07) as usize;
                self.mmc3.regs[register] = val;
            }
            0x8000 => self.mmc3.bank_select = val & 0xC7,
            0xA000 if !odd => {
                self.mirroring = if val & 1 == 0 {
                    Mirroring::Vertical
                } else {
                    Mirroring::Horizontal
                };
            }
            0xC000 if odd => {
                // The counter is cleared at once and reloaded from the latch on
                // the next clock.
                self.mmc3.irq_counter = 0;
                self.mmc3.irq_reload = true;
            }
            0xC000 => self.mmc3.irq_latch = val,
            0xE000 if odd => self.mmc3.irq_enabled = true,
            0xE000 => {
                self.mmc3.irq_enabled = false;
                self.mmc3.irq_pending = false;
            }
            _ => {}
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
/// 1 is MMC1 (battery, horizontal header bits), 2 is UxROM, 3 is CNROM, 7 is
/// AxROM, 71 is Camerica, 0 is plain NROM.
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

    /// A synthesized CNROM image: two 16 KiB PRG blocks filled with `0x11` and
    /// `0x22`, and four 8 KiB CHR banks zero-filled.
    fn cnrom_cart() -> Cartridge {
        let mut bytes = ines_header(2, 4, 3).to_vec();
        bytes.extend(std::iter::repeat_n(0x11, PRG_UNIT));
        bytes.extend(std::iter::repeat_n(0x22, PRG_UNIT));
        bytes.resize(bytes.len() + 4 * CHR_BANK, 0);
        Cartridge::load(&bytes).expect("synthetic CNROM image must load")
    }

    /// The same CNROM image with a NES 2.0 header naming `submapper`.
    fn cnrom_nes20_cart(submapper: u8) -> Cartridge {
        let mut bytes = ines_header(2, 4, 3).to_vec();
        bytes[7] |= 0x08; // NES 2.0 signature
        bytes[8] = submapper << 4;
        bytes.extend(std::iter::repeat_n(0x11, PRG_UNIT));
        bytes.extend(std::iter::repeat_n(0x22, PRG_UNIT));
        bytes.resize(bytes.len() + 4 * CHR_BANK, 0);
        Cartridge::load(&bytes).expect("synthetic NES 2.0 CNROM image must load")
    }

    /// A synthesized mapper-185 image: two 16 KiB PRG blocks filled with
    /// `0x11` and `0x22`, and one 8 KiB CHR bank whose bytes count up. `None`
    /// leaves a plain iNES header, whose unknown select value must use the
    /// two-read heuristic.
    fn cnrom185_cart(submapper: Option<u8>) -> Cartridge {
        let mut header = ines_header(2, 1, 185);
        if let Some(submapper) = submapper {
            header[7] |= 0x08; // NES 2.0 signature
            header[8] = submapper << 4;
        }
        let mut bytes = header.to_vec();
        bytes.extend(std::iter::repeat_n(0x11, PRG_UNIT));
        bytes.extend(std::iter::repeat_n(0x22, PRG_UNIT));
        bytes.extend((0..CHR_BANK).map(|i| i as u8));
        Cartridge::load(&bytes).expect("synthetic mapper-185 image must load")
    }

    /// Raw bytes for a synthesized AxROM image: `prg_banks` 32 KiB blocks,
    /// each filled with its own index, no CHR (the PPU gets RAM), mapper 7.
    fn axrom_bytes(prg_banks: u8) -> Vec<u8> {
        let mut bytes = ines_header(prg_banks * 2, 0, 7).to_vec();
        bytes.extend((0..prg_banks).flat_map(|bank| std::iter::repeat_n(bank, BANK)));
        bytes
    }

    fn axrom_cart(prg_banks: u8) -> Cartridge {
        Cartridge::load(&axrom_bytes(prg_banks)).expect("synthetic AxROM image must load")
    }

    /// The same image with a NES 2.0 signature and `submapper`, and `patch`
    /// written at `$8000` in the power-on bank so a bus conflict is observable.
    fn axrom_nes20_cart(submapper: u8, prg_banks: u8, patch: u8) -> Cartridge {
        let mut bytes = axrom_bytes(prg_banks);
        bytes[7] |= 0x08; // NES 2.0 signature
        bytes[8] = submapper << 4;
        bytes[16] = patch;
        Cartridge::load(&bytes).expect("synthetic NES 2.0 AxROM image must load")
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

    /// Raw bytes for a synthesized Camerica image: `prg_banks` 16 KiB blocks,
    /// each filled with its own index, no CHR (the PPU gets RAM), mapper 71.
    fn camerica_bytes(prg_banks: u8) -> Vec<u8> {
        let mut bytes = ines_header(prg_banks, 0, 71).to_vec();
        for bank in 0..prg_banks {
            bytes.extend(std::iter::repeat_n(bank, PRG_UNIT));
        }
        bytes
    }

    fn camerica_cart(prg_banks: u8) -> Cartridge {
        Cartridge::load(&camerica_bytes(prg_banks)).expect("synthetic Camerica image must load")
    }

    /// The same image with a NES 2.0 header naming a mapper 71 submapper.
    fn camerica_nes20_bytes(prg_banks: u8, submapper: u8) -> Vec<u8> {
        let mut bytes = camerica_bytes(prg_banks);
        bytes[7] |= 0x08;
        bytes[8] = submapper << 4;
        bytes
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

    #[test]
    fn a_fingerprint_separates_header_only_differences() {
        let mut bytes = ines_header(1, 1, 0).to_vec();
        bytes.resize(16 + PRG_UNIT + CHR_BANK, 0);
        let original = Cartridge::load(&bytes).expect("synthetic image must load");
        bytes[6] ^= 0x01; // flip the NROM mirroring bit only
        let flipped = Cartridge::load(&bytes).expect("synthetic image must load");
        assert_ne!(original.fingerprint(), flipped.fingerprint());
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
        let mut other_mapper = ines_header(4, 2, 5).to_vec();
        other_mapper.resize(16 + 4 * PRG_UNIT + 2 * CHR_BANK, 0);
        let err = match Cartridge::load(&other_mapper) {
            Ok(_) => panic!("mapper 5 is not supported"),
            Err(e) => e,
        };
        assert!(
            err.contains("mapper 5"),
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

    /// Mapper 71 is UxROM with the bank register in `$C000-$FFFF` only. Games
    /// like Micro Machines write `$8000` at startup, and that write must not
    /// move the bank. The low window switches and the last bank stays fixed.
    #[test]
    fn camerica_banks_only_from_the_upper_window_and_keeps_the_last_bank_fixed() {
        let mut c = camerica_cart(8);
        assert_eq!(c.mapper_id(), 71, "camerica_cart must load as mapper 71");
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xBFFF), c.cpu_read(0xC000)),
            (0, 0, 7),
            "power-on bank 0 low, last bank fixed high"
        );
        assert!(!c.prg_ram_enabled(), "mapper 71 has no $6000-$7FFF RAM");
        assert!(c.chr_rom().is_empty(), "no CHR bytes -> the PPU gets RAM");

        c.cpu_write(0x8000, 2);
        assert_eq!(c.cpu_read(0x8000), 0, "$8000 is not the bank register");

        c.cpu_write(0xC000, 2);
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xBFFF), c.cpu_read(0xC000)),
            (2, 2, 7),
            "the register moves only the low window"
        );

        c.cpu_write(0xFFD0, 1);
        assert_eq!(c.cpu_read(0x8000), 1, "$FFD0 is in the register window");

        c.cpu_write(0xC000, 0x1F);
        assert_eq!(c.cpu_read(0x8000), 7, "bank 15 wraps to bank 7 of 8");
    }

    /// The header fixes ordinary mapper 71 mirroring, and the Fire Hawk board
    /// (NES 2.0 submapper 1, and the same `$9000` heuristic for plain iNES)
    /// switches one-screen mirroring with bit 4. Only `$9000-$9FFF` is the
    /// mirroring register; `$8000` is not.
    #[test]
    fn camerica_fire_hawk_writes_switch_one_screen_mirroring() {
        let mut bytes = camerica_bytes(8);
        bytes[6] |= 0x01; // header asks for vertical mirroring
        let mut c = Cartridge::load(&bytes).expect("synthetic Camerica image must load");
        assert_eq!(c.mirroring(), Mirroring::Vertical, "header mirroring");

        c.cpu_write(0xC000, 3);
        assert_eq!(c.cpu_read(0x8000), 3, "bank 3 selected for the latch check");

        c.cpu_write(0x9000, 0x12);
        assert_eq!(c.mirroring(), Mirroring::SingleUpper);
        assert_eq!(c.cpu_read(0x8000), 3, "the write did not select a bank");

        c.cpu_write(0x9000, 0x00);
        assert_eq!(c.mirroring(), Mirroring::SingleLower);

        c.cpu_write(0x8000, 0x10);
        assert_eq!(
            c.mirroring(),
            Mirroring::SingleLower,
            "only $9000-$9FFF controls mirroring"
        );
    }

    /// NES 2.0 submapper 0 is the hardwired-mirroring board: neither `$8000`
    /// nor `$9000` is a mirroring register, so the header wins.
    #[test]
    fn camerica_nes20_submapper_zero_keeps_header_mirroring() {
        let mut bytes = camerica_nes20_bytes(8, 0);
        bytes[6] |= 0x01;
        let mut c = Cartridge::load(&bytes).expect("synthetic Camerica image must load");
        c.cpu_write(0xC000, 3);

        c.cpu_write(0x9000, 0x12);
        c.cpu_write(0x8000, 0x12);
        assert_eq!(
            c.mirroring(),
            Mirroring::Vertical,
            "submapper 0 ignores both addresses"
        );
        assert_eq!(c.cpu_read(0x8000), 3, "neither write banks on submapper 0");
    }

    /// NES 2.0 submapper 1 is Fire Hawk: its latch spans `$8000-$9FFF`, while
    /// the bank register stays in `$C000-$FFFF`.
    #[test]
    fn camerica_nes20_fire_hawk_uses_the_full_8000_9fff_latch() {
        let mut c = Cartridge::load(&camerica_nes20_bytes(8, 1))
            .expect("synthetic Camerica image must load");
        c.cpu_write(0xC000, 3);
        assert_eq!(c.cpu_read(0x8000), 3);

        c.cpu_write(0x8000, 0x10);
        assert_eq!(c.mirroring(), Mirroring::SingleUpper);
        assert_eq!(c.cpu_read(0x8000), 3, "the latch write did not bank");

        c.cpu_write(0x9FFF, 0x00);
        assert_eq!(c.mirroring(), Mirroring::SingleLower);

        c.cpu_write(0xA000, 0x10);
        assert_eq!(
            c.mirroring(),
            Mirroring::SingleLower,
            "$A000 is outside the latch"
        );
    }

    /// The bank and the one-screen page are mapper state: both must survive a
    /// save and load.
    #[test]
    fn a_saved_camerica_state_restores_the_bank_and_mirroring() {
        use crate::state::{StateReader, StateWriter};

        let mut cart = camerica_cart(8);
        cart.cpu_write(0xC000, 3);
        cart.cpu_write(0x9000, 0x10);
        let before = (cart.cpu_read(0x8000), cart.mirroring());
        assert_eq!(before, (3, Mirroring::SingleUpper));

        let mut writer = StateWriter::new();
        cart.save_state(&mut writer);
        let bytes = writer.finish();

        let mut restored = camerica_cart(8);
        restored
            .load_state(&mut StateReader::new(&bytes))
            .expect("mapper 71 state must load");
        assert_eq!(
            (restored.cpu_read(0x8000), restored.mirroring()),
            before,
            "the bank and nametable page must come back"
        );
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

    /// CNROM's PRG side is unbanked: 16 KiB images mirror across the whole
    /// window and 32 KiB images map both halves directly. The CHR register is
    /// the only bank state.
    #[test]
    fn cnrom_loads_as_mapper_3_with_fixed_prg_and_no_prg_ram() {
        let c = cnrom_cart();
        assert_eq!(c.mapper_id(), 3, "cnrom_cart must load as mapper 3");
        assert!(matches!(c.board, Board::CnRom), "mapper 3 is CNROM");
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000)),
            (0x11, 0x22),
            "32 KiB PRG is mapped directly, not banked"
        );
        assert!(!c.prg_ram_enabled(), "CNROM has no $6000-$7FFF RAM");
        assert_eq!(
            c.chr_map(),
            [0, 1, 2, 3, 4, 5, 6, 7],
            "power-on selects CHR bank 0"
        );
        assert_eq!(
            c.mirroring(),
            Mirroring::Horizontal,
            "the header mirroring bit is fixed on the board"
        );
    }

    /// CNROM-128 mirrors its single 16 KiB PRG block across the whole
    /// `$8000-$FFFF` window. The byte at an offset must appear at that offset
    /// in both halves.
    #[test]
    fn cnrom_16k_prg_mirrors_across_the_whole_window() {
        let mut bytes = ines_header(1, 1, 3).to_vec();
        bytes.extend((0..PRG_UNIT).map(|o| o as u8));
        bytes.extend(std::iter::repeat_n(0u8, CHR_BANK));
        let c = Cartridge::load(&bytes).expect("16 KiB CNROM image must load");
        assert_eq!(c.cpu_read(0x8000 + 0x1234), 0x34, "lower half");
        assert_eq!(
            c.cpu_read(0xC000 + 0x1234),
            0x34,
            "upper half mirrors the same block"
        );
        assert_eq!(c.cpu_read(0xFFFF), 0xFF, "last byte of the window");
    }

    /// The CHR register decodes every write in `$8000-$FFFF` and selects one
    /// 8 KiB CHR bank. It does not move PRG: a CNROM image has no PRG bank
    /// register at all.
    #[test]
    fn cnrom_write_selects_the_chr_bank_named_by_the_rom_byte() {
        let mut c = cnrom_cart();
        c.prg[0x100] = 0x02; // $8100
        c.cpu_write(0x8100, 0x02);
        assert_eq!(
            c.chr_map(),
            [16, 17, 18, 19, 20, 21, 22, 23],
            "register value 2 selects the third 8 KiB CHR bank"
        );
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000)),
            (0x11, 0x22),
            "the write must not move either PRG half"
        );
    }

    /// A CNROM write is ANDed with the byte the PRG ROM drives at that address.
    /// A matching write therefore is not required: `$03` against a ROM byte of
    /// `$01` selects bank 1, and `$00` against `$02` selects bank 0. An equality
    /// gate would ignore both writes and leave the previous bank selected.
    #[test]
    fn cnrom_bus_conflict_ands_the_written_value_with_the_rom_byte() {
        let mut c = cnrom_cart();
        c.prg[0x101] = 0x01; // $8101
        c.cpu_write(0x8101, 0x03);
        assert_eq!(
            c.chr_map()[0],
            8,
            "only the bits the ROM drives high survive the conflict"
        );

        c.prg[0x102] = 0x02; // $8102
        c.cpu_write(0x8102, 0x00);
        assert_eq!(
            c.chr_map()[0],
            0,
            "a zero written against a nonzero ROM byte still selects bank 0"
        );
    }

    /// NES 2.0 submapper 1 marks the mapper-3 hack boards with no bus
    /// conflicts, so the latch takes the written value whole. The same write
    /// that loses bits to a plain-iNES conflict must select bank 3 here.
    #[test]
    fn cnrom_nes20_submapper_one_has_no_bus_conflicts() {
        let mut c = cnrom_nes20_cart(1);
        c.prg[0x101] = 0x01; // $8101
        c.cpu_write(0x8101, 0x03);
        assert_eq!(
            c.chr_map()[0],
            24,
            "submapper 1 must not AND the write with the ROM byte"
        );
    }

    /// Submapper 2 names the original AND-type conflict board. A write keeps
    /// only the bits the PRG ROM drives high there.
    #[test]
    fn cnrom_nes20_submapper_two_applies_and_type_bus_conflicts() {
        let mut c = cnrom_nes20_cart(2);
        c.prg[0x101] = 0x01; // $8101
        c.cpu_write(0x8101, 0x03);
        assert_eq!(
            c.chr_map()[0],
            8,
            "submapper 2 must AND the write with the ROM byte"
        );
    }

    /// Submapper 0 leaves the conflict behavior unknown, so the emulator
    /// keeps the original AND-type behavior that plain iNES also uses.
    #[test]
    fn cnrom_nes20_submapper_zero_keeps_and_type_bus_conflicts() {
        let mut c = cnrom_nes20_cart(0);
        c.prg[0x101] = 0x01; // $8101
        c.cpu_write(0x8101, 0x03);
        assert_eq!(
            c.chr_map()[0],
            8,
            "unknown submapper 0 keeps the AND-type conflict"
        );
    }

    /// The CHR bank is mapper state: a state saved after a CNROM switch must
    /// restore the same pattern table.
    #[test]
    fn a_saved_cnrom_state_restores_the_chr_bank() {
        use crate::state::{StateReader, StateWriter};

        let mut cart = cnrom_cart();
        cart.prg[0x100] = 0x03;
        cart.cpu_write(0x8100, 0x03);
        let before = cart.chr_map();
        assert_eq!(before[0], 24, "test setup selected the fourth CHR bank");

        let mut writer = StateWriter::new();
        cart.save_state(&mut writer);
        let bytes = writer.finish();

        let mut restored = cnrom_cart();
        restored
            .load_state(&mut StateReader::new(&bytes))
            .expect("CNROM mapper state must load");
        assert_eq!(restored.chr_map(), before, "the CHR bank must come back");
    }

    /// Mapper 185 is CNROM's fixed-PRG side with one 8 KiB CHR bank. Its
    /// protection latch is not a bank register: all four select values point
    /// at the same pattern data.
    #[test]
    fn cnrom185_loads_as_mapper_185_with_fixed_prg_and_one_chr_bank() {
        let c = cnrom185_cart(Some(5));
        assert_eq!(c.mapper_id(), 185, "cnrom185_cart must load as mapper 185");
        assert!(matches!(c.board, Board::CnRom185), "mapper 185 is CnRom185");
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000)),
            (0x11, 0x22),
            "32 KiB PRG is mapped directly, not banked"
        );
        assert!(!c.prg_ram_enabled(), "mapper 185 has no $6000-$7FFF RAM");
        assert_eq!(
            c.chr_map(),
            [0, 1, 2, 3, 4, 5, 6, 7],
            "the 8 KiB bank fills the whole pattern window"
        );
        assert_eq!(
            c.mirroring(),
            Mirroring::Horizontal,
            "the header mirroring bit is fixed on the board"
        );
    }

    /// NES 2.0 submappers 4-7 wire the CHR chip's select inputs to 0-3. The
    /// board's 74HC161 powers up at 0, so only submapper 4 starts enabled.
    /// Every other latch value tri-states the chip.
    #[test]
    fn cnrom185_submappers_name_the_chr_select_value() {
        for (submapper, select) in [(4, 0), (5, 1), (6, 2), (7, 3)] {
            let mut c = cnrom185_cart(Some(submapper));
            assert_eq!(
                c.chr_enabled(),
                select == 0,
                "submapper {submapper}: power-on latch 0 {} select {select}",
                if select == 0 {
                    "matches"
                } else {
                    "does not match"
                }
            );

            // The ROM byte drives $0F, so a write with the select value in its
            // low two bits latches exactly that value.
            c.prg[0x100] = 0x0F; // $8100
            c.cpu_write(0x8100, 0xF0 | select);
            assert!(c.chr_enabled(), "submapper {submapper}: the wired value");

            // A different low nibble must disable the chip again.
            c.prg[0x101] = 0x0F; // $8101
            c.cpu_write(0x8101, 0xF0 | ((select + 1) & 0x03));
            assert!(!c.chr_enabled(), "submapper {submapper}: the wrong value");
        }
    }

    /// Mapper 185 always takes the CNROM AND-type bus conflict, even for a
    /// NES 2.0 header. Only the bits the PRG ROM drives high reach the latch.
    #[test]
    fn cnrom185_writes_are_anded_with_the_rom_byte() {
        let mut c = cnrom185_cart(Some(5)); // select 1
        c.prg[0x100] = 0x03; // $8100
        c.cpu_write(0x8100, 0x03);
        assert_eq!(c.latch & 0x03, 3, "the latch sees val & ROM");
        assert!(!c.chr_enabled(), "$03 is not the wired select value 1");

        c.prg[0x101] = 0x01; // $8101
        c.cpu_write(0x8101, 0x03);
        assert_eq!(c.latch & 0x03, 1, "only the ROM-driven bit survives");
        assert!(c.chr_enabled(), "$01 is the wired select value");
    }

    /// Without a NES 2.0 select value, NESdev's heuristic returns open bus for
    /// the first two `$2007` reads after reset and then enables CHR for good.
    /// A known select value ignores the read count entirely.
    #[test]
    fn cnrom185_unknown_select_uses_the_two_read_heuristic() {
        let mut c = cnrom185_cart(None);
        assert!(!c.chr_enabled(), "the unknown-select board starts disabled");
        assert!(c.note_ppu_read(), "the first read is open bus");
        assert!(!c.chr_enabled());
        assert!(c.note_ppu_read(), "the second read is open bus");
        assert!(c.chr_enabled(), "the second read enables CHR");
        assert!(!c.note_ppu_read(), "later reads are not overridden");
        assert!(c.chr_enabled());

        // A latch write does not disable the chip once the heuristic completed,
        // because the unknown board cannot know which value would be wrong.
        c.prg[0x100] = 0xFF;
        c.cpu_write(0x8100, 0xFF);
        assert!(
            c.chr_enabled(),
            "the heuristic stays enabled for the session"
        );

        for submapper in 4..=7 {
            let mut known = cnrom185_cart(Some(submapper));
            assert!(
                !known.note_ppu_read(),
                "submapper {submapper} has no heuristic"
            );
            assert_eq!(
                known.chr_enabled(),
                submapper == 4,
                "submapper {submapper} keeps its power-on select state"
            );
        }
    }

    /// RESET clears the latch and restarts the unknown-select heuristic.
    #[test]
    fn cnrom185_reset_restores_the_power_on_protection_state() {
        let mut known = cnrom185_cart(Some(5)); // select 1
        known.prg[0x100] = 0x01;
        known.cpu_write(0x8100, 0x01);
        assert!(known.chr_enabled());
        known.reset();
        assert_eq!(known.latch, 0, "the 74HC161 powers up cleared");
        assert!(!known.chr_enabled(), "latch 0 is not select 1");

        let mut unknown = cnrom185_cart(None);
        assert!(unknown.note_ppu_read());
        assert!(unknown.note_ppu_read());
        assert!(unknown.chr_enabled());
        unknown.reset();
        assert!(!unknown.chr_enabled(), "reset disables the unknown board");
        assert!(unknown.note_ppu_read(), "the first read is open bus again");
        assert!(!unknown.chr_enabled());
        assert!(unknown.note_ppu_read(), "the second read enables CHR again");
        assert!(unknown.chr_enabled());
    }

    /// The latch, enable state and read count are mapper state: a state saved
    /// during the startup check must resume it, not restart it.
    #[test]
    fn a_saved_cnrom185_state_restores_the_protection_state() {
        use crate::state::{StateReader, StateWriter};

        let mut known = cnrom185_cart(Some(5)); // select 1
        known.prg[0x100] = 0x01;
        known.cpu_write(0x8100, 0x01);
        let before = (known.latch, known.chr_enabled(), known.ppu_reads);
        assert!(before.1, "test setup enabled CHR");

        let mut writer = StateWriter::new();
        known.save_state(&mut writer);
        let bytes = writer.finish();
        let mut restored = cnrom185_cart(Some(5));
        restored
            .load_state(&mut StateReader::new(&bytes))
            .expect("mapper 185 state must load");
        assert_eq!(
            (restored.latch, restored.chr_enabled(), restored.ppu_reads),
            before,
            "the latch and enable state must come back"
        );

        // A state saved after one heuristic read must still need exactly one
        // more read before CHR turns on.
        let mut unknown = cnrom185_cart(None);
        assert!(unknown.note_ppu_read());
        let mut writer = StateWriter::new();
        unknown.save_state(&mut writer);
        let bytes = writer.finish();
        let mut restored = cnrom185_cart(None);
        restored
            .load_state(&mut StateReader::new(&bytes))
            .expect("mapper 185 heuristic state must load");
        assert!(!restored.chr_enabled());
        assert!(restored.note_ppu_read(), "the interrupted second read");
        assert!(restored.chr_enabled());
    }

    /// AxROM maps one 32 KiB bank over the whole `$8000-$FFFF` window, and the
    /// register decodes every write in that window. A model that keeps the old
    /// fixed top half fails the `$C000` checks.
    #[test]
    fn axrom_maps_one_32k_bank_across_the_whole_window() {
        let mut c = axrom_cart(4); // 128 KiB = four 32 KiB banks
        assert_eq!(c.mapper_id(), 7, "axrom_cart must load as mapper 7");
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000), c.cpu_read(0xFFFF)),
            (0, 0, 0),
            "power-on bank 0 covers the whole window"
        );

        c.cpu_write(0x8000, 2);
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000), c.cpu_read(0xFFFF)),
            (2, 2, 2),
            "bank 2 took both halves of the window"
        );

        // The register decodes every write in $8000-$FFFF, not just $8000.
        c.cpu_write(0xFFD0, 1);
        assert_eq!(
            (c.cpu_read(0x8000), c.cpu_read(0xC000)),
            (1, 1),
            "a write in the upper half still moves the bank"
        );

        // The 74HC161 counts modulo the image's bank count.
        c.cpu_write(0x8000, 7);
        assert_eq!(c.cpu_read(0x8000), 3, "bank 7 wraps to bank 3 of 4");
    }

    /// Register bit 4 selects the 1 KiB VRAM page for all four nametables:
    /// clear is the lower page, set is the upper. Bits 2-0 keep banking.
    #[test]
    fn axrom_register_bit_four_switches_one_screen_mirroring() {
        let mut c = axrom_cart(4);
        assert_eq!(
            c.mirroring(),
            Mirroring::SingleLower,
            "power-on selects the lower nametable page"
        );

        c.cpu_write(0x8000, 0x10);
        assert_eq!(c.mirroring(), Mirroring::SingleUpper);
        assert_eq!(c.cpu_read(0x8000), 0, "bit 4 does not move the bank");

        c.cpu_write(0x8000, 0x12);
        assert_eq!(c.mirroring(), Mirroring::SingleUpper);
        assert_eq!(c.cpu_read(0x8000), 2, "bit 4 and bank bits share a write");

        c.cpu_write(0x8000, 0x02);
        assert_eq!(c.mirroring(), Mirroring::SingleLower);
        assert_eq!(c.cpu_read(0x8000), 2);
    }

    /// NES 2.0 submapper 2 names an AxROM board with AND-type bus conflicts:
    /// the latch sees the written byte ANDed with the PRG byte at that address.
    #[test]
    fn axrom_nes20_submapper_two_applies_and_type_bus_conflicts() {
        // Bank 0's $8000 reads $01, so writing $03 leaves $01 and selects bank 1.
        let mut c = axrom_nes20_cart(2, 4, 0x01);
        c.cpu_write(0x8000, 0x03);
        assert_eq!(
            c.cpu_read(0x8001),
            1,
            "only the bit the ROM drives high survives"
        );

        // A write with no bit in common with the ROM leaves the bank alone.
        let mut c = axrom_nes20_cart(2, 4, 0x00);
        c.cpu_write(0x8000, 0x02);
        assert_eq!(c.cpu_read(0x8001), 0, "AND with 0 selects bank 0");

        // The mirroring bit is part of the same conflict.
        let mut c = axrom_nes20_cart(2, 4, 0x00);
        c.cpu_write(0x8000, 0x10);
        assert_eq!(c.mirroring(), Mirroring::SingleLower);
        let mut c = axrom_nes20_cart(2, 4, 0x10);
        c.cpu_write(0x8000, 0x10);
        assert_eq!(c.mirroring(), Mirroring::SingleUpper);
    }

    /// Submapper 0 and 1 have no bus conflicts, so the written value reaches
    /// the register unchanged.
    #[test]
    fn axrom_nes20_submapper_one_has_no_bus_conflicts() {
        let mut c = axrom_nes20_cart(1, 4, 0x00);
        c.cpu_write(0x8000, 0x02);
        assert_eq!(c.cpu_read(0x8001), 2, "the write selected bank 2 directly");
        c.cpu_write(0x8000, 0x10);
        assert_eq!(c.mirroring(), Mirroring::SingleUpper);
    }

    /// Bit 3 is the fourth bank bit on the 512 KiB AxROM variant, so a 16-bank
    /// image must reach banks 8-15.
    #[test]
    fn axrom_uses_bit_three_for_the_upper_half_of_a_512k_image() {
        let mut c = axrom_cart(16);
        assert_eq!(c.prg.len(), 16 * BANK, "512 KiB of PRG");
        c.cpu_write(0x8000, 0x08);
        assert_eq!(c.cpu_read(0x8000), 8, "bit 3 selected bank 8");
        c.cpu_write(0x8000, 0x0F);
        assert_eq!(c.cpu_read(0x8000), 15, "all four bank bits are live");
    }

    /// AxROM has 8 KiB of CHR RAM and no PRG RAM, and its mirroring is entirely
    /// register-controlled: the iNES header bit must not leak through.
    #[test]
    fn axrom_has_no_prg_ram_and_ignores_the_header_mirroring() {
        let mut bytes = axrom_bytes(2);
        bytes[6] |= 0x01; // header asks for vertical mirroring
        let c = Cartridge::load(&bytes).expect("synthetic AxROM image must load");
        assert!(!c.prg_ram_enabled(), "AxROM has no $6000-$7FFF RAM");
        assert!(c.chr_rom().is_empty(), "no CHR bytes -> the PPU gets RAM");
        assert_eq!(
            c.mirroring(),
            Mirroring::SingleLower,
            "the board powers up one-screen lower regardless of the header"
        );
    }

    /// The bank and the one-screen page are mapper state: both must survive a
    /// save and load.
    #[test]
    fn a_saved_axrom_state_restores_the_bank_and_mirroring() {
        use crate::state::{StateReader, StateWriter};

        let mut cart = axrom_cart(4);
        cart.cpu_write(0x8000, 0x12); // bank 2, upper nametable
        let before = (cart.cpu_read(0x8000), cart.mirroring());
        assert_eq!(before, (2, Mirroring::SingleUpper));

        let mut writer = StateWriter::new();
        cart.save_state(&mut writer);
        let bytes = writer.finish();

        let mut restored = axrom_cart(4);
        restored
            .load_state(&mut StateReader::new(&bytes))
            .expect("AxROM mapper state must load");
        assert_eq!(
            (restored.cpu_read(0x8000), restored.mirroring()),
            before,
            "the bank and nametable page must come back"
        );
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

    /// An MMC3 image: `prg_16k` 16 KiB PRG units, each 8 KiB half filled with
    /// its own index, and `chr_8k` 8 KiB CHR banks whose 1 KiB quarters are
    /// filled with 0x80+index.
    fn mmc3_cart(prg_16k: u8, chr_8k: u8) -> Cartridge {
        let mut bytes = ines_header(prg_16k, chr_8k, 4).to_vec();
        for bank in 0..prg_16k * 2 {
            bytes.extend(std::iter::repeat_n(bank, 0x2000));
        }
        for bank in 0..chr_8k * 8 {
            bytes.extend(std::iter::repeat_n(0x80 + bank, 0x400));
        }
        let last = 16 + prg_16k as usize * 0x4000 - 0x2000;
        let vectors = last + (0xFFFC - 0xE000);
        bytes[vectors..vectors + 2].copy_from_slice(&0xE000u16.to_le_bytes());
        Cartridge::load(&bytes).expect("synthetic MMC3 image must load")
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

    /// Power-on has no meaningful bank registers, so the reset vector must live
    /// in the fixed last 8 KiB bank at `$E000`. The second-last bank is fixed at
    /// `$C000` in PRG mode 0; R6 and R7 power up as bank 0.
    #[test]
    fn mmc3_powers_up_with_the_last_two_banks_fixed() {
        let c = mmc3_cart(2, 0); // 32 KiB PRG = four 8 KiB banks
        assert_eq!(c.mapper_id(), 4, "mmc3_cart must load as mapper 4");
        assert!(c.prg_ram_enabled(), "MMC3 powers up with PRG RAM enabled");
        assert_eq!(c.cpu_read(0x8000), 0, "R6 powers up as bank 0");
        assert_eq!(c.cpu_read(0xA000), 0, "R7 powers up as bank 0");
        assert_eq!(c.cpu_read(0xC000), 2, "the second-last bank is fixed");
        assert_eq!(c.cpu_read(0xE000), 3, "the last bank holds the vectors");
        assert_eq!(
            u16::from_le_bytes([c.cpu_read(0xFFFC), c.cpu_read(0xFFFD)]),
            0xE000,
            "the synthetic reset vector is in the fixed bank"
        );
    }

    /// `$8000` selects which register `$8001` writes. R6 and R7 are the two 8
    /// KiB switchable windows; bit 6 swaps which of `$8000`/`$C000` gets R6.
    #[test]
    fn mmc3_bank_registers_and_prg_mode_choose_the_8k_windows() {
        // Four 8 KiB banks. R6 = 1 at $8000, R7 = 2 at $A000.
        let mut c = mmc3_cart(2, 0);
        c.cpu_write(0x8000, 6);
        c.cpu_write(0x8001, 1);
        c.cpu_write(0x8000, 7);
        c.cpu_write(0x8001, 2);
        assert_eq!(c.cpu_read(0x8000), 1);
        assert_eq!(c.cpu_read(0xA000), 2);

        // PRG mode 1: R6 moves to $C000 and $8000 is pinned to the second-last
        // bank; R7 and the last bank do not move.
        c.cpu_write(0x8000, 0x46);
        c.cpu_write(0x8001, 1);
        assert_eq!(c.cpu_read(0x8000), 2, "second-last bank is fixed at $8000");
        assert_eq!(c.cpu_read(0xA000), 2, "R7 stays at $A000");
        assert_eq!(c.cpu_read(0xC000), 1, "R6 now lives at $C000");
        assert_eq!(c.cpu_read(0xE000), 3, "the last bank never moves");

        // A register past the end wraps like the bank counter it drives.
        c.cpu_write(0x8000, 0x06);
        c.cpu_write(0x8001, 0x1A);
        assert_eq!(c.cpu_read(0xC000), 0x1A % 4);
    }

    /// CHR mode 0: R0/R1 are 2 KiB banks at `$0000`/`$0800`, R2-R5 are 1 KiB
    /// banks at `$1000-$1C00`. Bit 7 of `$8000` swaps the two halves.
    #[test]
    fn mmc3_chr_registers_map_2k_and_1k_windows_in_both_modes() {
        let mut c = mmc3_cart(2, 2); // 16 KiB CHR = sixteen 1 KiB banks
        c.cpu_write(0x8000, 0); // R0
        c.cpu_write(0x8001, 2); // 2 KiB at $0000
        c.cpu_write(0x8000, 1); // R1
        c.cpu_write(0x8001, 6); // 2 KiB at $0800
        assert_eq!(
            c.chr_map(),
            [2, 3, 6, 7, 0, 0, 0, 0],
            "R2-R5 power up as bank 0"
        );

        // Inverted: R2-R5 fill $0000-$0FFF and R0/R1 move to $1000.
        c.cpu_write(0x8000, 0x82); // R2, inverted
        c.cpu_write(0x8001, 7);
        c.cpu_write(0x8000, 0x83); // R3
        c.cpu_write(0x8001, 8);
        c.cpu_write(0x8000, 0x84); // R4
        c.cpu_write(0x8001, 9);
        c.cpu_write(0x8000, 0x85); // R5
        c.cpu_write(0x8001, 10);
        c.cpu_write(0x8000, 0x80); // R0
        c.cpu_write(0x8001, 12);
        c.cpu_write(0x8000, 0x81); // R1
        c.cpu_write(0x8001, 14);
        assert_eq!(c.chr_map(), [7, 8, 9, 10, 12, 13, 14, 15]);

        // 2 KiB registers ignore their low bit.
        c.cpu_write(0x8000, 0x00);
        c.cpu_write(0x8001, 3);
        assert_eq!(
            (c.chr_map()[0], c.chr_map()[1]),
            (2, 3),
            "R0 bit 0 is ignored"
        );
    }

    /// `$A000` bit 0 picks the nametable arrangement. `$A001` is MMC6's bank
    /// control on some mapper-4 boards, so none of its bits close PRG RAM.
    #[test]
    fn mmc3_mirroring_is_register_controlled_and_prg_ram_stays_enabled() {
        let mut c = mmc3_cart(2, 0);
        assert_eq!(
            c.mirroring(),
            Mirroring::Horizontal,
            "the synthetic header asks for horizontal"
        );
        c.cpu_write(0xA000, 0);
        assert_eq!(c.mirroring(), Mirroring::Vertical);
        c.cpu_write(0xA000, 1);
        assert_eq!(c.mirroring(), Mirroring::Horizontal);

        assert!(c.prg_ram_enabled(), "power-on PRG RAM is enabled");
        for value in [0x00, 0x10, 0x20, 0x40, 0x80, 0xC0] {
            c.cpu_write(0xA001, value);
            assert!(
                c.prg_ram_enabled(),
                "$A001 ${value:02X} belongs to MMC6 or only write-protects; keep RAM reachable"
            );
        }
    }

    /// A `$C001` write arms a reload for the next clock. The counter then
    /// counts down once per clock and raises the IRQ as it reaches zero; the
    /// gap between reload and IRQ is always latch+1 clocks.
    #[test]
    fn mmc3_irq_counter_fires_after_the_latch_plus_one_clocks() {
        let mut c = mmc3_cart(2, 0);
        c.cpu_write(0xC000, 3); // latch
        c.cpu_write(0xC001, 0); // reload on the next clock
        c.cpu_write(0xE001, 0); // enable

        for _ in 0..3 {
            c.clock_irq();
            assert!(!c.irq_pending(), "no IRQ before the counter reaches 0");
        }
        c.clock_irq();
        assert!(c.irq_pending(), "IRQ on the fourth clock");

        // $E000 acknowledges and disables. The counter keeps running while
        // disabled, and $E001 re-enables without acknowledging.
        c.cpu_write(0xE000, 0);
        assert!(!c.irq_pending(), "$E000 acknowledges the IRQ");
        c.clock_irq();
        assert!(!c.irq_pending(), "a disabled counter raises no IRQ");
        c.cpu_write(0xE001, 0);
        for _ in 0..2 {
            c.clock_irq();
            assert!(!c.irq_pending());
        }
        c.clock_irq();
        assert!(c.irq_pending(), "the reloaded counter still counts down");
    }

    /// A latch of 0 is the MMC3B/C behaviour: the IRQ stays asserted on every
    /// clock. Games that want a per-scanline split rely on this.
    #[test]
    fn mmc3_zero_latch_fires_an_irq_on_every_clock() {
        let mut c = mmc3_cart(2, 0);
        c.cpu_write(0xC000, 0);
        c.cpu_write(0xC001, 0);
        c.cpu_write(0xE001, 0);
        c.clock_irq();
        assert!(c.irq_pending());
    }

    /// MMC3's bank select, register file, IRQ counter and mirroring are part of
    /// the state. A state saved mid-countdown must resume the same countdown.
    #[test]
    fn a_saved_mmc3_state_restores_every_mapper_register() {
        use crate::state::{StateReader, StateWriter};

        let mut cart = mmc3_cart(4, 4); // eight 8 KiB PRG banks, 32 KiB CHR
        cart.cpu_write(0x8000, 0x46); // PRG mode 1, R6 selected
        cart.cpu_write(0x8001, 3);
        cart.cpu_write(0x8000, 0x07); // R7
        cart.cpu_write(0x8001, 5);
        cart.cpu_write(0x8000, 0x80); // R0, inverted CHR
        cart.cpu_write(0x8001, 4);
        cart.cpu_write(0x8000, 0x81); // R1
        cart.cpu_write(0x8001, 12);
        cart.cpu_write(0x8000, 0x82); // R2
        cart.cpu_write(0x8001, 9);
        cart.cpu_write(0xA000, 1); // horizontal
        cart.cpu_write(0xC000, 10); // IRQ latch
        cart.cpu_write(0xC001, 0); // reload
        cart.cpu_write(0xE001, 0); // enable
        for _ in 0..4 {
            cart.clock_irq(); // reload 10, then 9, 8, 7
        }
        assert!(!cart.irq_pending(), "the countdown is still running");
        let before = (
            cart.cpu_read(0x8000),
            cart.cpu_read(0xA000),
            cart.cpu_read(0xC000),
            cart.cpu_read(0xE000),
            cart.chr_map(),
            cart.mirroring(),
            cart.prg_ram_enabled(),
        );
        assert_eq!(before.0, 3, "mode 1 puts R6 at $C000");
        assert_eq!(before.1, 5);
        assert!(
            before.6,
            "MMC3 PRG RAM stays enabled for MMC6 compatibility"
        );

        let mut writer = StateWriter::new();
        cart.save_state(&mut writer);
        let bytes = writer.finish();

        let mut restored = mmc3_cart(4, 4);
        restored
            .load_state(&mut StateReader::new(&bytes))
            .expect("mapper state must load");
        assert_eq!(
            (
                restored.cpu_read(0x8000),
                restored.cpu_read(0xA000),
                restored.cpu_read(0xC000),
                restored.cpu_read(0xE000),
                restored.chr_map(),
                restored.mirroring(),
                restored.prg_ram_enabled(),
            ),
            before,
            "every MMC3 register must come back"
        );

        let mut original = cart.clone();
        let clocks_to_irq = |c: &mut Cartridge| {
            let mut clocks = 0;
            while !c.irq_pending() && clocks < 300 {
                c.clock_irq();
                clocks += 1;
            }
            clocks
        };
        let expected = clocks_to_irq(&mut original);
        assert_eq!(
            clocks_to_irq(&mut restored),
            expected,
            "the saved counter must resume at the same value"
        );
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

        // NESdev's iNES detection also requires bytes 12-15 to be zero. A
        // ripper signature there can leave garbage in flags 9/10, so those
        // bytes must not select the region.
        let mut signature = ines_header(1, 0, 0);
        signature[9] = 0x01;
        signature[12..].copy_from_slice(b"JUNK");
        assert_eq!(
            load(signature).region(),
            Region::Ntsc,
            "a signed header is not plain iNES"
        );
    }

    /// NES 2.0 byte 9's low nibble is the PRG size's high four bits. $01 in
    /// byte 9 turns byte 4's one 16 KiB unit into 256 units, 4 MiB.
    #[test]
    fn nes20_prg_size_uses_the_high_nibble_of_byte_nine() {
        let mut bytes = ines_header(0, 0, 0).to_vec();
        bytes[7] |= 0x08; // NES 2.0 signature
        bytes[9] = 0x01;
        let err = match Cartridge::load(&bytes) {
            Ok(_) => panic!("4 MiB of PRG cannot fit in a 16-byte file"),
            Err(e) => e,
        };
        assert!(
            err.contains("4194304"),
            "the error must name the 12-bit size: {err}"
        );
    }

    /// NES 2.0 byte 9's high nibble is the CHR size's high four bits.
    #[test]
    fn nes20_chr_size_uses_the_high_nibble_of_byte_nine() {
        let mut bytes = ines_header(1, 0, 0).to_vec();
        bytes[7] |= 0x08; // NES 2.0 signature
        bytes[9] = 0x10;
        bytes.extend(std::iter::repeat_n(0u8, PRG_UNIT));
        let err = match Cartridge::load(&bytes) {
            Ok(_) => panic!("2 MiB of CHR cannot fit in a PRG-only file"),
            Err(e) => e,
        };
        assert!(
            err.contains("2097152"),
            "the error must name the 12-bit size: {err}"
        );
    }

    /// Both size bytes use exponent-multiplier notation when their MSB nibble
    /// is $F: size = 2^E * (2*M + 1). Byte 4 = %00110100 is E=13, M=0, so
    /// 8 KiB; byte 5 = %00101100 is E=11, M=0, so 2 KiB.
    #[test]
    fn nes20_exponent_multiplier_sizes_are_loaded() {
        let mut bytes = ines_header(0b0011_0100, 0b0010_1100, 0).to_vec();
        bytes[7] |= 0x08; // NES 2.0 signature
        bytes[9] = 0xFF; // both MSB nibbles select exponent-multiplier
        bytes.extend(std::iter::repeat_n(0u8, 8192));
        bytes.extend(std::iter::repeat_n(0u8, 2048));
        let c = Cartridge::load(&bytes).expect("exponent-sized image must load");
        assert_eq!(c.prg.len(), 8192, "byte 4 names 8 KiB of PRG");
        assert_eq!(c.chr_rom().len(), 2048, "byte 5 names 2 KiB of CHR");
    }

    /// The multiplier in the exponent form allows sizes that are not powers of
    /// two: %00110110 is E=13, M=2, so 8192 * 5 = 40 KiB.
    #[test]
    fn nes20_exponent_multiplier_covers_odd_sizes() {
        assert_eq!(rom_size(0b0011_0110, 0xF, PRG_UNIT), Some(40960));
        assert_eq!(rom_size(0b0010_1100, 0xF, CHR_BANK), Some(2048));
    }

    /// A board can only map PRG sizes its bank window can address. Exponent
    /// sizes that cannot fit are refused instead of panicking in `cpu_read`.
    #[test]
    fn nes20_prg_sizes_that_the_board_cannot_map_are_rejected() {
        let cases = [
            (8192, 0x34, 66, "GxROM under one 32 KiB bank"),
            (8192, 0x34, 2, "UxROM under one 16 KiB bank"),
            (24576, 0x35, 2, "UxROM half a 16 KiB bank"),
            (24576, 0x35, 0, "NROM not a power of two"),
            (98304, 0x3D, 66, "GxROM with three 32 KiB banks"),
        ];
        for (size, lsb, mapper, label) in cases {
            let mut bytes = ines_header(0, 0, mapper).to_vec();
            bytes[4] = lsb;
            bytes[7] |= 0x08;
            bytes[9] = 0x0F;
            bytes.resize(16 + size, 0);
            let err = match Cartridge::load(&bytes) {
                Ok(_) => panic!("{label} must be refused"),
                Err(e) => e,
            };
            assert!(
                err.contains(&size.to_string()),
                "{label}: the error must name {size}: {err}"
            );
        }
    }

    /// Byte 10 and byte 11 hold four shift counts. A non-zero count n means
    /// 64 << n bytes; zero means the memory is absent.
    #[test]
    fn nes20_ram_sizes_are_parsed_from_bytes_ten_and_eleven() {
        let mut bytes = ines_header(1, 0, 0).to_vec();
        bytes[7] |= 0x08; // NES 2.0 signature
        bytes[10] = 0x97; // PRG RAM 64<<7, PRG NVRAM 64<<9
        bytes[11] = 0x67; // CHR RAM 64<<7, CHR NVRAM 64<<6
        bytes.extend(std::iter::repeat_n(0u8, PRG_UNIT));
        let c = Cartridge::load(&bytes).expect("NES 2.0 image must load");
        assert_eq!(c.prg_ram_size(), 8192);
        assert_eq!(c.prg_nvram_size(), 32768);
        assert_eq!(c.chr_ram_size(), 8192);
        assert_eq!(c.chr_nvram_size(), 4096);
    }

    /// NES 2.0's PRG RAM fields are authoritative even on boards whose enum
    /// historically had no RAM. Alwa's Awakening uses this to save 8 KiB of
    /// NVRAM on mapper 2.
    #[test]
    fn nes20_declared_prg_ram_enables_a_ramless_board() {
        let mut bytes = ines_header(1, 0, 2).to_vec();
        bytes[7] |= 0x08; // NES 2.0 signature
        bytes[10] = 0x70; // PRG NVRAM 64 << 7
        bytes.extend(std::iter::repeat_n(0u8, PRG_UNIT));
        let c = Cartridge::load(&bytes).expect("NES 2.0 UxROM image must load");
        assert!(
            c.prg_ram_enabled(),
            "declared NVRAM must enable the $6000-$7FFF window"
        );
    }

    /// NES 2.0 zero means no PRG RAM, even on NROM/MMC1/MMC3 where plain
    /// iNES has always provided 8 KiB.
    #[test]
    fn nes20_zero_prg_ram_disables_the_window() {
        let mut bytes = ines_header(1, 0, 0).to_vec();
        bytes[7] |= 0x08; // NES 2.0 signature; byte 10 stays zero
        bytes.extend(std::iter::repeat_n(0u8, PRG_UNIT));
        let c = Cartridge::load(&bytes).expect("NES 2.0 NROM image must load");
        assert!(!c.prg_ram_enabled(), "NES 2.0 zero means absent");
    }

    /// Byte 7 bits 0-1 are the console type under NES 2.0. Value 3 asks for
    /// the extended type in byte 13's low nibble. Only a regular NES/Famicom
    /// is modelled, so the other consoles are refused by name.
    #[test]
    fn nes20_console_type_is_parsed_and_unsupported_consoles_are_refused() {
        let bytes = |console: u8, extended: u8| {
            let mut h = ines_header(1, 0, 0);
            h[7] = (h[7] & 0xF0) | 0x08 | console;
            h[13] = extended;
            let mut bytes = h.to_vec();
            bytes.extend(std::iter::repeat_n(0u8, PRG_UNIT));
            bytes
        };
        let load = |console: u8, extended: u8| Cartridge::load(&bytes(console, extended));

        load(0, 0).expect("NES console must load");
        load(3, 0).expect("extended type 0 is a regular NES/Famicom");

        let err = match load(1, 0) {
            Ok(_) => panic!("Vs. System is not modelled"),
            Err(e) => e,
        };
        assert!(err.contains("Vs. System"), "the error must name it: {err}");

        let err = match load(2, 0) {
            Ok(_) => panic!("PlayChoice-10 is not modelled"),
            Err(e) => e,
        };
        assert!(
            err.contains("PlayChoice-10"),
            "the error must name it: {err}"
        );

        let err = match load(3, 6) {
            Ok(_) => panic!("extended console type 6 is not modelled"),
            Err(e) => e,
        };
        assert!(
            err.contains("extended console type 6"),
            "the error must name it: {err}"
        );

        let mut plain = ines_header(1, 0, 0);
        plain[7] |= 0x03;
        let mut bytes = plain.to_vec();
        bytes.extend(std::iter::repeat_n(0u8, PRG_UNIT));
        Cartridge::load(&bytes).expect("plain iNES has no console field to read");
    }

    /// Plain iNES keeps its byte 4/5 sizes and has no NES 2.0 RAM fields.
    #[test]
    fn plain_ines_ignores_the_nes20_size_and_ram_fields() {
        let mut h = ines_header(1, 0, 0);
        h[9] = 0x11; // would extend both sizes under NES 2.0
        h[10] = 0x77; // would claim 8 KiB of each PRG RAM type
        h[11] = 0x77; // would claim 8 KiB of each CHR RAM type
        let mut bytes = h.to_vec();
        bytes.extend(std::iter::repeat_n(0u8, PRG_UNIT));
        let c = Cartridge::load(&bytes).expect("plain iNES image must load");
        assert_eq!(c.prg.len(), PRG_UNIT, "byte 9 is not a size on iNES");
        assert_eq!(c.prg_ram_size(), 0, "iNES has no PRG RAM size field");
        assert_eq!(c.prg_nvram_size(), 0);
        assert_eq!(c.chr_ram_size(), 0);
        assert_eq!(c.chr_nvram_size(), 0);
    }

    /// The 12-bit mapper number includes byte 8's low nibble. Mapper $100 is
    /// not supported, and the error must name the full number, not mapper 0.
    #[test]
    fn nes20_mapper_includes_the_high_nibble_from_byte_eight() {
        let mut h = ines_header(1, 0, 0);
        h[7] |= 0x08; // NES 2.0 signature
        h[8] = 0x01; // mapper $100
        let mut bytes = h.to_vec();
        bytes.extend(std::iter::repeat_n(0u8, PRG_UNIT));
        let err = match Cartridge::load(&bytes) {
            Ok(_) => panic!("mapper $100 is not supported"),
            Err(e) => e,
        };
        assert!(
            err.contains("mapper 256"),
            "the error must name the full 12-bit mapper: {err}"
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

    /// MMC1's serial latch, PRG/CHR banks, mirroring and PRG-RAM enable are
    /// part of the state; a load has to put every one of them back.
    #[test]
    fn a_saved_mmc1_state_restores_every_mapper_register() {
        use crate::state::{StateReader, StateWriter};

        let mut cart = mmc1_cart(8, 8);
        mmc1_write(&mut cart, 0x8000, 0x0E); // PRG mode 3, 8 KiB CHR, vertical mirroring
        mmc1_write(&mut cart, 0xA000, 3); // CHR bank 0
        mmc1_write(&mut cart, 0xC000, 5); // CHR bank 1
        mmc1_write(&mut cart, 0xE000, 0x13); // PRG bank 3, PRG RAM disabled
        let before = (
            cart.cpu_read(0x8000),
            cart.cpu_read(0xE000),
            cart.chr_map(),
            cart.mirroring(),
            cart.prg_ram_enabled(),
        );
        assert_eq!(before.0, 3, "PRG bank 3 at $8000");
        assert_eq!(before.1, 7, "the fixed last bank at $C000 stays 7");
        assert!(!before.4, "the PRG RAM disable bit was written");

        let mut writer = StateWriter::new();
        cart.save_state(&mut writer);
        let bytes = writer.finish();

        let mut restored = mmc1_cart(8, 8);
        restored
            .load_state(&mut StateReader::new(&bytes))
            .expect("mapper state must load");
        assert_eq!(
            (
                restored.cpu_read(0x8000),
                restored.cpu_read(0xE000),
                restored.chr_map(),
                restored.mirroring(),
                restored.prg_ram_enabled(),
            ),
            before,
            "every MMC1 register must come back"
        );
    }

    /// Writes deflated entries, the method real archives use.
    fn write_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, bytes) in entries {
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn temp_dir(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nes-rom-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_zip_rom_is_the_nes_entry_inside_it() {
        let dir = temp_dir("zip");
        let path = dir.join("game.zip");
        let image = ines_header(1, 1, 0).to_vec();
        write_zip(
            &path,
            &[("readme.txt", b"not a rom"), ("Game (USA).NES", &image)],
        );

        assert_eq!(read_rom(&path).expect("the zip must load"), image);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_zip_without_a_nes_entry_names_the_archive() {
        let dir = temp_dir("zip-empty");
        let path = dir.join("game.zip");
        write_zip(&path, &[("readme.txt", b"not a rom")]);

        let error = read_rom(&path).expect_err("a zip with no .nes must fail");

        assert!(
            error.contains("game.zip") && error.contains(".nes"),
            "{error}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
