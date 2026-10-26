//! PPU: registers, VRAM, palette RAM, the CHR ROM or CHR RAM pattern space,
//! and the dot-by-dot background and sprite pipelines.
//!
//! References, all fetched 2026-10-16:
//! - NESdev "PPU registers" -- register bits, the v/t/x/w internal registers,
//!   the `$2002` read side effects, and the `$2007` read buffer.
//! - NESdev "PPU scrolling" -- the exact coarse-X increment and Y increment
//!   pseudocode, the dot-257 horizontal copy and the dots 280-304 vertical copy.
//! - NESdev "PPU rendering" -- the fetch timeline, the shift-register reload,
//!   and the sprite/background priority multiplexer table.
//! - NESdev "PPU sprite evaluation" -- secondary OAM, the eight sprite output
//!   units, and the note that evaluation does not happen on the pre-render line.
//! - NESdev "PPU OAM" -- the OAM entry layout, the 8x16 pattern memory map,
//!   the sprite-0 hit rules, and OAM priority by address.
//! - NESdev "PPU frame timing" -- vblank at 241.1, the odd-frame skipped dot.
//! - NESdev "PPU palettes" -> `File:2C02G_U_wiki.pal` -- the 2C02 master palette.
//! - "NTSC 2C02 technical reference" (Brad Taylor) -- "Details of object
//!   pattern fetch & render" for the 8x16 address rule, the in-range comparator,
//!   and the down-counter that serialises a sprite's eight pixels.

use crate::region::Region;
use crate::state::{StateReader, StateWriter};

const DOTS_PER_LINE: u32 = 341;
const VISIBLE_LINES: u32 = 240;
const VBLANK_FIRST: u32 = 241;

/// The MMC3's A12 filter ignores a low pulse shorter than this many PPU dots,
/// so normal same-table fetches and the four-dot gap between sprite pattern
/// reads do not clock the scanline counter. Hardware filters A12 for three
/// falling edges of M2, about ten PPU dots.
const A12_FILTER_DOTS: u32 = 10;

/// The 2C02 master palette, verbatim from the NESdev wiki page "PPU palettes",
/// file `2C02G_U_wiki.pal` (generated with pally v0.23.0, `--cld -phd 4 -e`),
/// downloaded from
/// `https://www.nesdev.org/w/index.php?title=Special:Redirect/file/2C02G_U_wiki.pal`
/// and read as 64 RGB triplets. `$0F` is `$000000` and `$20`/`$30` are
/// `$FFFFFF`, which is what NESdev means by "the canonical code for black is
/// `$0F`".
const PALETTE: [u32; 64] = [
    0x00_57_57_57,
    0x00_00_0C_8E,
    0x00_08_00_A6,
    0x00_34_00_96,
    0x00_55_00_61,
    0x00_63_00_15,
    0x00_5A_00_00,
    0x00_3C_0E_00,
    0x00_11_28_00,
    0x00_00_3B_00,
    0x00_42_00,
    0x00_3A_05,
    0x00_00_26_52,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_A5_A5_A5,
    0x00_00_41_D9,
    0x00_2F_1E_FF,
    0x00_67_04_F2,
    0x00_94_00_B4,
    0x00_AA_00_57,
    0x00_A3_18_00,
    0x00_80_39_00,
    0x00_4B_5B_00,
    0x00_13_76_00,
    0x00_00_81_00,
    0x00_00_79_23,
    0x00_00_62_88,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_FF_FF_FF,
    0x00_4A_9F_FF,
    0x00_79_7E_FF,
    0x00_AF_63_FF,
    0x00_DD_55_FF,
    0x00_F7_57_C2,
    0x00_F7_6A_63,
    0x00_DC_88_10,
    0x00_AE_A9_00,
    0x00_78_C4_00,
    0x00_4A_D2_11,
    0x002F_CF64,
    0x00_2F_BD_C4,
    0x00_41_41_41,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_FF_FF_FF,
    0x00_B9_DD_FF,
    0x00_CA_D1_FF,
    0x00_DE_C6_FF,
    0x00_F0_C0_FF,
    0x00_FC_C0_EE,
    0x00_FD_C6_CA,
    0x00_F5_D0_AA,
    0x00_E4_DD_95,
    0x00_D0_E8_92,
    0x00_BD_EE_A2,
    0x00_B2_EE_C0,
    0x00_B0_E8_E3,
    0x00_B3_B3_B3,
    0x00_00_00_00,
    0x00_00_00_00,
];

/// The 2C07 (PAL) palette, verbatim from the NESdev wiki page "PPU palettes",
/// file `2C07_wiki.pal` (pally v0.24.1, `--cld -ppu 2C07 -phd 4
/// --delay-line-filter -e`). The file holds 512 entries, eight emphasis
/// variants of the 64 colours; the first 64 are the no-emphasis values this
/// emulator uses.
const PAL_PALETTE: [u32; 64] = [
    0x00_62_62_62,
    0x00_00_23_65,
    0x00_0D_10_7F,
    0x00_2B_01_7F,
    0x00_45_00_65,
    0x00_54_00_37,
    0x00_54_05_01,
    0x00_45_15_00,
    0x00_2B_28_00,
    0x00_0D_37_00,
    0x00_00_3F_00,
    0x00_00_3E_01,
    0x00_00_33_37,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_AB_AB_AB,
    0x00_11_51_AA,
    0x00_34_38_CE,
    0x00_5C_23_CE,
    0x00_7F_19_AA,
    0x00_93_1A_6C,
    0x00_93_28_24,
    0x00_7F_3F_00,
    0x00_5C_58_00,
    0x00_34_6C_00,
    0x00_11_77_00,
    0x00_00_75_24,
    0x00_00_67_6C,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_FF_FF_FF,
    0x00_61_A1_FB,
    0x00_84_89_FF,
    0x00_AD_74_FF,
    0x00_D0_69_FB,
    0x00_E4_6B_BC,
    0x00_E4_79_74,
    0x00_D0_8F_36,
    0x00_AD_A8_12,
    0x00_84_BD_12,
    0x00_61_C8_36,
    0x00_4D_C6_74,
    0x00_4D_B8_BC,
    0x00_4E_4E_4E,
    0x00_00_00_00,
    0x00_00_00_00,
    0x00_FF_FF_FF,
    0x00_C4_DD_FF,
    0x00_D1_D3_FF,
    0x00_E1_CB_FF,
    0x00_EF_C7_FF,
    0x00_F7_C8_E7,
    0x00_F7_CD_CB,
    0x00_EF_D6_B3,
    0x00_E1_DF_A5,
    0x00_D1_E7_A5,
    0x00_C4_EC_B3,
    0x00_BC_EB_CB,
    0x00_BC_E5_E7,
    0x00_B8_B8_B8,
    0x00_00_00_00,
    0x00_00_00_00,
];

/// The colour the PPU emits where both the background and the sprites are
/// transparent. On a stock NES the EXT pins are grounded, so this is whatever
/// palette RAM entry `$3F00` holds; the shift register model only ever asks for
/// colour 0 with no tile, and a colour-0 pattern bit pair is transparent for
/// the same reason, so this is the one colour that is never looked up per pixel.
const BACKDROP: u32 = 0x00_00_00_00;

/// Which nametable RAM each `$2000-$2FFF` address reaches. Boards wire the
/// four logical nametables differently; MMC1 and later mappers can also change
/// the wiring at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mirroring {
    Vertical,
    Horizontal,
    SingleLower,
    SingleUpper,
}

impl Mirroring {
    pub(crate) fn id(self) -> u8 {
        match self {
            Mirroring::Vertical => 0,
            Mirroring::Horizontal => 1,
            Mirroring::SingleLower => 2,
            Mirroring::SingleUpper => 3,
        }
    }

    pub(crate) fn from_id(id: u8) -> Result<Mirroring, String> {
        match id {
            0 => Ok(Mirroring::Vertical),
            1 => Ok(Mirroring::Horizontal),
            2 => Ok(Mirroring::SingleLower),
            3 => Ok(Mirroring::SingleUpper),
            other => Err(format!("save state has an unknown mirroring mode {other}")),
        }
    }
}

/// Nametable RAM: 2 KiB of physical RAM. The offset inside it depends on the
/// board's mirroring: vertical folds `$2800` onto `$2000`, horizontal folds
/// `$2400` onto `$2000`, and the single-screen modes make all four logical
/// nametables the same 1 KiB.
fn vram_index(addr: u16, mirroring: Mirroring) -> usize {
    let addr = addr & 0x0FFF;
    match mirroring {
        Mirroring::Vertical => (addr & 0x07FF) as usize,
        Mirroring::Horizontal => (((addr & 0x0800) >> 1) | (addr & 0x03FF)) as usize,
        Mirroring::SingleLower => (addr & 0x03FF) as usize,
        Mirroring::SingleUpper => 0x0400 | (addr & 0x03FF) as usize,
    }
}

/// Palette RAM is 32 entries. Only four of the `$3F10-$3F1F` addresses are
/// mirrors: `$3F10`, `$3F14`, `$3F18` and `$3F1C` sit behind
/// `$3F00`/`$3F04`/`$3F08`/`$3F0C` (NESdev "PPU palettes": "Addresses $3F10/$3F14/
/// $3F18/$3F1C are mirrors of $3F00/$3F04/$3F08/$3F0C"), and `$3F20` upwards
/// mirrors `$3F00`. The other twelve are ordinary background entries -- folding
/// all sixteen down leaves a sprite palette no writer can reach.
fn palette_index(addr: u16) -> usize {
    let i = (addr & 0x1F) as usize;
    if i >= 0x10 && i.is_multiple_of(4) {
        i - 0x10
    } else {
        i
    }
}

#[derive(Clone)]
pub struct Ppu {
    vram: [u8; 2048],
    palette: [u8; 32],
    /// Pattern memory. The cartridge may carry CHR ROM larger than the PPU's
    /// 8 KiB address window; the bank map selects which part is visible, so
    /// this holds the whole image. A cartridge with no CHR bytes gets 8 KiB of
    /// CHR RAM here.
    chr: Vec<u8>,
    /// Which 1 KiB CHR bank backs each of the eight 1 KiB slots at
    /// `$0000-$1FFF`. Eight slots rather than one 8 KiB bank lets a mapper
    /// switch 4 KiB (MMC1) or 1 KiB (MMC3) windows without the PPU knowing
    /// which board it is running.
    chr_map: [usize; 8],
    /// How the board wires the four logical nametables onto the 2 KiB of RAM.
    mirroring: Mirroring,
    /// Which console timing this PPU is: the scanline count and the palette.
    region: Region,
    /// Whether `$0000-$1FFF` is RAM instead of ROM: true for cartridges that
    /// carry no CHR bytes at all (blargg's 2005 suite), which upload their own
    /// tiles through `$2007`.
    chr_ram: bool,
    /// Power-on OAM is dynamic RAM with no defined contents, and Y = $FF is the
    /// documented way to hide a sprite ("Hide a sprite by moving it down
    /// offscreen, by writing any values between #$EF-#$FF here"). Filling it
    /// with $FF is therefore the honest "nothing is on screen yet": every game
    /// writes all 256 bytes by OAM DMA in vblank before it ever shows anything,
    /// and a zero-filled OAM would instead put 64 opaque sprites on screen at
    /// (0,0) for any test that renders a frame without touching OAM.
    oam: [u8; 256],
    secondary_oam: [u8; 32],
    /// Which of the eight secondary-OAM slots hold OAM entry 0. Secondary OAM
    /// fills COMPACTLY -- "the OAM data for each sprite found to be within range
    /// is copied into the secondary OAM" -- so a slot's index says nothing about
    /// which sprite it is, and the sprite-0 hit has to be told explicitly.
    secondary_sprite0: [bool; 8],
    ctrl: u8,
    mask: u8,
    status: u8,
    oam_addr: u8,
    /// The current VRAM address (the low 15 bits of the PPU's `v` register).
    v: u16,
    /// The temporary VRAM address (`t`): the scroll position / the address
    /// `$2006` is loading. `v` is copied from it at dot 257 of every scanline
    /// and, for the vertical bits, at dots 280-304 of the pre-render line.
    t: u16,
    /// Fine X scroll (`x`), the low 3 bits of the first `$2005` write.
    fine_x: u8,
    /// `$2005`/`$2006` share one first-write/second-write flip-flop (`w`).
    write_latch: bool,
    /// `$2007` reads return this one-byte-late buffer, not the addressed byte.
    read_buffer: u8,
    scanline: u32,
    dot: u32,
    /// Toggles every frame. Odd frames drop one dot from the pre-render line.
    odd_frame: bool,
    frame_done: bool,
    nmi: bool,
    framebuffer: Vec<u32>,
    /// Background pipeline. The shifters hold TWO tiles at once: the tile being
    /// drawn in the high byte and the one after it in the low byte, because a
    /// pixel near a tile boundary can need a bit of the following tile (fine X).
    fetch: Tile,
    prev: Tile,
    cur: Tile,
    bg_lo: u16,
    bg_hi: u16,
    /// The palette number of each of those two tiles, kept separately because
    /// the fetches keep landing in `prev`/`cur` while they are still in use.
    attr_prev: u8,
    attr_cur: u8,
    /// The eight sprite output units holding the data for the scanline being
    /// drawn. Loaded at dot 257 of the previous scanline (hardware spreads it
    /// over dots 257-320) and indexed in OAM order, because the 2C02 reference
    /// says priority is "passed to the next serializer" and so unit 0 wins
    /// wherever sprites overlap. Evaluation fills them COMPACTLY, so a unit's
    /// index is not its OAM index: `is_sprite0` says which one is sprite 0.
    sprites: [Sprite; 8],
    /// How many dots the last PPU address kept A12 low. The MMC3 sees the
    /// filtered edge of this line, not a synthetic once-per-scanline pulse.
    a12_low_dots: u32,
    /// One rising edge can wait between two PPU ticks after a `$2006` write.
    a12_rise: bool,
}

/// One sprite output unit: the X counter the 2C02 reference describes, the two
/// pattern bytes it serialises, the attribute bits, and whether this unit is
/// sprite 0. A unit with no sprite keeps `lo` and `hi` at 0, which is the
/// "completely transparent bitmap patterns" the hardware loads into the unused
/// slots.
#[derive(Clone, Copy, Default)]
struct Sprite {
    x: u8,
    lo: u8,
    hi: u8,
    attr: u8,
    is_sprite0: bool,
}

impl Sprite {
    /// This sprite's 2-bit colour index at screen X `x`, 0 for transparent.
    /// The counter is loaded with the sprite's X and "is decremented every time
    /// the PPU renders a pixel"; the unit starts serialising when it reaches 0
    /// and runs out "8 clocks after", so the eight pixels land on x..x+7 -- the
    /// same dots the background writes.
    fn pixel(&self, x: u32) -> u8 {
        let col = x.wrapping_sub(self.x as u32);
        if col > 7 {
            return 0;
        }
        let bit = 7 - col;
        ((self.hi >> bit) & 1) << 1 | ((self.lo >> bit) & 1)
    }

    fn save_state(&self, writer: &mut StateWriter) {
        writer.u8(self.x);
        writer.u8(self.lo);
        writer.u8(self.hi);
        writer.u8(self.attr);
        writer.bool(self.is_sprite0);
    }

    fn load_state(&mut self, reader: &mut StateReader) -> Result<(), String> {
        self.x = reader.u8()?;
        self.lo = reader.u8()?;
        self.hi = reader.u8()?;
        self.attr = reader.u8()?;
        self.is_sprite0 = reader.bool()?;
        Ok(())
    }
}

/// One fetched background tile: its two pattern bytes, its nametable index, and
/// the 2-bit palette number its attribute-table field selected.
#[derive(Clone, Copy, Default)]
struct Tile {
    lo: u8,
    hi: u8,
    tile: u8,
    attr: u8,
}

impl Tile {
    fn save_state(&self, writer: &mut StateWriter) {
        writer.u8(self.lo);
        writer.u8(self.hi);
        writer.u8(self.tile);
        writer.u8(self.attr);
    }

    fn load_state(&mut self, reader: &mut StateReader) -> Result<(), String> {
        self.lo = reader.u8()?;
        self.hi = reader.u8()?;
        self.tile = reader.u8()?;
        self.attr = reader.u8()?;
        Ok(())
    }
}

impl Ppu {
    /// Power-on position. Hardware comes out of reset "at the top of the
    /// picture" with its shift registers still cold, which would leave the
    /// first 16 pixels of the first scanline unwritten. Starting on the
    /// pre-render line instead costs one blanked scanline at boot and makes
    /// every frame this PPU renders complete.
    #[cfg(test)]
    pub fn new() -> Ppu {
        Ppu::for_region(Region::Ntsc)
    }

    /// A PPU for `region`: NTSC's 262-scanline 2C02 or PAL's 312-scanline
    /// 2C07.
    pub fn for_region(region: Region) -> Ppu {
        Ppu {
            vram: [0; 2048],
            palette: [0; 32],
            chr: vec![0; 16384],
            chr_map: [0, 1, 2, 3, 4, 5, 6, 7],
            mirroring: Mirroring::Vertical,
            region,
            chr_ram: false,
            oam: [0xFF; 256],
            secondary_oam: [0; 32],
            secondary_sprite0: [false; 8],
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            v: 0,
            t: 0,
            fine_x: 0,
            write_latch: false,
            read_buffer: 0,
            scanline: region.scanlines() - 1,
            dot: 0,
            odd_frame: false,
            frame_done: false,
            nmi: false,
            framebuffer: vec![BACKDROP; 256 * 240],
            fetch: Tile::default(),
            prev: Tile::default(),
            cur: Tile::default(),
            bg_lo: 0,
            bg_hi: 0,
            attr_prev: 0,
            attr_cur: 0,
            sprites: [Sprite::default(); 8],
            a12_low_dots: 1,
            a12_rise: false,
        }
    }

    /// The pre-render line: the last line of the frame, 261 on NTSC and 311
    /// on PAL.
    fn pre_render(&self) -> u32 {
        self.region.scanlines() - 1
    }

    /// Advance the PPU by one dot. Three of these are one CPU cycle.
    pub fn tick(&mut self) {
        let pre_render = self.pre_render();
        // "The VBlank flag of the PPU is set at tick 1 (the second tick) of
        // scanline 241" and cleared 20 scanlines later, at dot 1 of the
        // pre-render line (261 on NTSC, 311 on PAL).
        if self.scanline == VBLANK_FIRST && self.dot == 1 {
            self.status |= 0x80;
            self.frame_done = true;
            if self.ctrl & 0x80 != 0 {
                self.nmi = true;
            }
        }
        if self.scanline == pre_render && self.dot == 1 {
            // "The three flags in this register are automatically cleared on
            // dot 1 of the prerender scanline" (NESdev "PPU registers"), which
            // is the ONLY place the sprite-0 hit and the overflow flag fall:
            // both stay up all through vblank, so a game can see them while it
            // polls $2002 between frames.
            //
            // The brief also asks for them to be cleared at 241.1. They are not
            // cleared there, because on hardware they are not, and SMB reads
            // $2002 inside its vblank handler.
            self.status &= !0xE0;
        }

        // The background pipeline runs on the visible scanlines and on the
        // pre-render line. It also keeps running when only sprites are enabled:
        // a disabled component is transparent, not absent, so the other one's
        // fetches and scroll updates are unaffected (NESdev "PPUMASK").
        let fetching = self.rendering_enabled()
            && (self.scanline < VISIBLE_LINES || self.scanline == pre_render);
        if fetching {
            self.background_dot();
            self.sprite_fetch_dot();
            // "At dot 256 of each scanline ... the PPU increments the vertical
            // position in v", which skips the attribute table and wraps between
            // nametables. Not on the pre-render line: NESdev's Y-increment
            // pseudocode is scoped to the scanlines the picture is drawn on,
            // and its dots 280-304 copy every one of those bits out of `t`
            // anyway.
            if self.dot == 256 && self.scanline < VISIBLE_LINES {
                self.increment_y();
            }
            // "At dot 257 of each scanline ... the PPU copies all bits related
            // to horizontal position from t to v".
            if self.dot == 257 {
                self.v = (self.v & !0x041F) | (self.t & 0x041F);
            }
            // "During pixels 280 through 304 of the pre-render scanline, the
            // vertical scroll bits are reloaded."
            if self.scanline == pre_render && (280..=304).contains(&self.dot) {
                self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
            }
            // Sprite evaluation (dots 65-256) and the sprite pattern fetches
            // (dots 257-320) both prepare the NEXT scanline. All 64 OAM
            // entries are read well before dot 1 of that scanline, so both
            // phases are collapsed onto dot 257 here.
            if self.dot == 257 {
                self.evaluate_sprites();
                self.load_sprite_units();
            }
        } else {
            // With rendering off, and while the PPU is outside the visible and
            // pre-render lines, the address bus holds `v` (NESdev "PPU
            // rendering", PPU address bus contents).
            let addr = self.v & 0x3FFF;
            self.observe_a12(addr);
        }

        if self.a12_low_dots > 0 {
            self.a12_low_dots = self.a12_low_dots.saturating_add(1);
        }

        self.dot += 1;
        if self.dot == DOTS_PER_LINE {
            // "On odd frames the cycle at the end of the pre-render scanline is
            // skipped ... by jumping directly from (339,261) to (0,0)". Only
            // with rendering enabled; with it disabled every frame is 89342
            // dots long.
            if self.scanline == pre_render && self.odd_frame && self.rendering_enabled() {
                self.dot = 0;
                self.scanline = 0;
                self.odd_frame = !self.odd_frame;
            } else {
                self.dot = 0;
                self.scanline += 1;
                if self.scanline > pre_render {
                    self.scanline = 0;
                    self.odd_frame = !self.odd_frame;
                }
            }
        }
    }

    /// Rendering is enabled as a whole while background OR sprites are shown.
    /// A disabled component is transparent, not absent: the other one's fetch
    /// pipeline keeps running.
    pub fn rendering_enabled(&self) -> bool {
        self.mask & 0x18 != 0
    }

    /// One dot of the background pipeline: four fetches, one shift-register
    /// reload every eight dots, one pixel out, one shift.
    ///
    /// The fetch cadence is eight dots long and runs straight through the
    /// scanline boundary: nametable at 1, attribute at 3, low pattern at 5, high
    /// pattern at 7, and the first TWO tiles of the next scanline are fetched at
    /// dots 321-336. That two-tile lead is what makes fine X work at all. A
    /// pixel can need a bit of the tile AFTER the one being drawn, so both tiles
    /// sit in the 16-bit shifters at once -- the drawn tile in the high byte, the
    /// next one in the low byte -- the leftmost `fine_x` pixels of the drawn
    /// tile coming out of the low byte. NESdev times the fetch-to-output delay
    /// as `16 - fine_x` dots ("NTSC 2C02 technical reference", playfield render
    /// pipeline); this is the same delay expressed as an 8-dot fetch lead plus
    /// the `15 - fine_x` tap.
    fn background_dot(&mut self) {
        let dot = self.dot;
        if (1..=256).contains(&dot) || (321..=336).contains(&dot) {
            match dot % 8 {
                1 => {
                    let addr = 0x2000 | (self.v & 0x0FFF);
                    self.observe_a12(addr);
                    self.fetch.tile = self.vram[vram_index(addr, self.mirroring)];
                }
                3 => {
                    // "attribute address = $23C0 | (v & $0C00) | ((v >> 4) &
                    // $38) | ((v >> 2) & $07)". Which of the four 2-bit fields
                    // in that byte is used is set by coarse X bit 1 and coarse Y
                    // bit 1 -- coarse, not fine: a Y scroll of 16 is coarse_y = 2
                    // with fine_y still 0, and the picture is in the next
                    // attribute row.
                    let addr = 0x23C0
                        | (self.v & 0x0C00)
                        | ((self.v >> 4) & 0x38)
                        | ((self.v >> 2) & 0x07);
                    self.observe_a12(addr);
                    let shift = (((self.v >> 6) & 1) << 2) | (((self.v >> 1) & 1) << 1);
                    self.fetch.attr = (self.vram[vram_index(addr, self.mirroring)] >> shift) & 3;
                }
                5 => {
                    let addr = self.pattern_address(0);
                    self.observe_a12(addr);
                    self.fetch.lo = self.chr[self.crbank(addr)];
                }
                7 => {
                    let addr = self.pattern_address(8);
                    self.observe_a12(addr);
                    self.fetch.hi = self.chr[self.crbank(addr)];
                    // The group is complete; it becomes the low half of the
                    // shifters one reload from now.
                    self.prev = self.cur;
                    self.cur = self.fetch;
                }
                _ => {}
            }
        }
        // Every 8th dot of the fetch regions moves the pattern bytes into the
        // shifters and steps coarse X on: "it begins at dots 328 and 336, and
        // will continue through the next scanline at 8, 16, 24 ... 256". Dot 0
        // is a reload without a step, and it is where the two tiles prefetched
        // at the end of the previous scanline start being drawn.
        let reload = dot.is_multiple_of(8)
            && (dot == 0 || (8..=256).contains(&dot) || (321..=336).contains(&dot));
        if reload {
            self.bg_lo = ((self.prev.lo as u16) << 8) | self.cur.lo as u16;
            self.bg_hi = ((self.prev.hi as u16) << 8) | self.cur.hi as u16;
            self.attr_prev = self.prev.attr;
            self.attr_cur = self.cur.attr;
        }
        if dot.is_multiple_of(8) && ((8..=256).contains(&dot) || (321..=336).contains(&dot)) {
            self.increment_coarse_x();
        }
        if dot < 256 && self.scanline < VISIBLE_LINES {
            // Same dots for both layers: pixel `x` of this scanline is written
            // at dot `x`, so a sprite cannot drift against the background.
            let bg = self.draw_pixel(dot);
            self.draw_sprite_pixel(dot, bg);
        }
        if dot <= 256 {
            self.bg_lo <<= 1;
            self.bg_hi <<= 1;
        }
    }

    /// Pixel `x` of the visible scanline being rendered, background first and
    /// sprites on top. Returns the background pixel's 2-bit colour index, 0 for
    /// transparent: the sprite-0 hit needs "an opaque pixel of sprite 0
    /// overlaps an opaque pixel of the background" (NESdev "PPU OAM"), and
    /// computing the background pixel once here is what keeps the caller from
    /// having to derive it a second time.
    fn draw_pixel(&mut self, x: u32) -> u8 {
        let y = self.scanline as usize * 256 + x as usize;
        // Bit 3 off, or the leftmost 8 pixels clipped by bit 1, means backdrop.
        if self.mask & 0x08 == 0 || (x < 8 && self.mask & 0x02 == 0) {
            self.framebuffer[y] = self.backdrop();
            return 0;
        }
        let tap = 15 - self.fine_x as u16;
        let value = ((((self.bg_hi >> tap) & 1) << 1) | ((self.bg_lo >> tap) & 1)) as u8;
        // The leftmost `fine_x` pixels of a tile come out of the NEXT tile, so
        // for those dots the next tile's attribute is the live one.
        let attr = if x % 8 < 8 - self.fine_x as u32 {
            self.attr_prev
        } else {
            self.attr_cur
        };
        self.framebuffer[y] = if value == 0 {
            // Both pattern bits clear: transparent. No palette entry is read,
            // which is why colour 0 never hides what is behind it.
            self.backdrop()
        } else {
            self.colour(self.palette[attr as usize * 4 + value as usize])
        };
        value
    }

    /// The sprite layer on top of a background pixel of colour index `bg`,
    /// following the priority multiplexer table on NESdev "PPU rendering": a
    /// transparent background always loses to a sprite, a transparent sprite
    /// always loses to the background, and otherwise attribute bit 5 decides.
    /// Where sprites overlap the lowest OAM index wins -- including when the
    /// higher-priority one is the sprite sitting behind an opaque background
    /// pixel, which is the "sprite priority quirk" the same page describes.
    fn draw_sprite_pixel(&mut self, x: u32, bg: u8) {
        // PPUMASK is `BGRs bMmG` (NESdev "PPU registers"): bit 4 (`s`) enables
        // sprites, bit 2 (`M`) shows them in the leftmost 8 pixels. NESdev puts
        // no sprite-0 hit at x=0..7 while either clip window is enabled, so
        // returning without touching the flag is right here.
        if self.mask & 0x10 == 0 || (x < 8 && self.mask & 0x04 == 0) {
            return;
        }
        for s in self.sprites {
            let value = s.pixel(x);
            if value == 0 {
                continue;
            }
            if bg != 0 && s.attr & 0x20 != 0 {
                // Behind an opaque background pixel: pass priority on to the
                // next sprite instead of letting this one win.
                continue;
            }
            let palette = 0x3F00 + 0x10 + ((s.attr & 3) as u16) * 4 + value as u16;
            let y = self.scanline as usize * 256 + x as usize;
            self.framebuffer[y] = self.colour(self.palette[palette_index(palette)]);
            break;
        }
        // Sprite-0 hit (NESdev "PPU OAM" -> Sprite 0 hits): "when an opaque
        // pixel of sprite 0 overlaps an opaque pixel of the background". The
        // unit carries an explicit is_sprite0 flag because evaluation fills
        // secondary OAM compactly, so unit 0 is only sprite 0 when sprite 0 was
        // found first in range.
        //
        // Two dots cannot report it, and only two. x = 255 never can: NESdev
        // says it is "an obscure reason related to the pixel pipeline",
        // unconditional. x = 0..7 can, unless a left-side clipping window is
        // enabled -- "sprite 0 hit does not happen at x=0 to x=7 if the
        // left-side clipping window is enabled (if bit 2 or bit 1 of PPUMASK
        // ($2001) is 0)" -- and that is exactly the `return` at the top of this
        // function. PPUMASK is `BGRs bMmG` (NESdev "PPU registers"): bit 4
        // enables sprites anywhere, bit 2 enables them in the leftmost 8 pixels.
        // SMB's HUD split is timed off this flag and its status bar sprite sits
        // at x = 0, so x = 0 suppressing unconditionally was a live timing
        // deviation, not a shortcut.
        if bg != 0
            && x != 255
            && self.status & 0x40 == 0
            && self.sprites.iter().any(|s| s.is_sprite0 && s.pixel(x) != 0)
        {
            self.status |= 0x40;
        }
    }

    /// How tall a sprite is: `$2000` bit 5.
    fn sprite_height(&self) -> u32 {
        if self.ctrl & 0x20 != 0 {
            16
        } else {
            8
        }
    }

    /// Sprite evaluation for the NEXT scanline: the first eight OAM entries in
    /// range are copied into secondary OAM COMPACTLY, and a ninth in-range entry
    /// raises the overflow flag and stops. "Compactly" is the whole point: the
    /// eight-sprite cap and the destination slot both count sprites FOUND, not
    /// their OAM index, so eight in-range sprites at OAM 20..27 are all drawn
    /// and raise nothing, and unit 0 is only sprite 0 when sprite 0 was found
    /// first -- which is what `secondary_sprite0` records.
    ///
    /// In range is the 9-bit comparator, not `y + 1 <= row < y + 1 + height`:
    /// `scanline - Y` wraps, which is what makes $EF-$FF the documented way to
    /// hide a sprite. See `in_range`.
    fn evaluate_sprites(&mut self) {
        self.secondary_oam = [0xFF; 32];
        self.secondary_sprite0 = [false; 8];
        // "Sprite evaluation does not happen on the pre-render scanline.
        // Because evaluation applies to the next line's sprite rendering, no
        // sprites will be rendered on the first scanline" (NESdev "PPU sprite
        // evaluation"). Leaving secondary OAM at $FF makes the pre-render line
        // load eight transparent units, which is that same outcome. It also
        // matters for the flags: the comparator at the pre-render line sees row
        // 262, where Y = $FF is 7 rows in range, so evaluating there with the
        // power-on all-$FF OAM would raise overflow every frame.
        if self.scanline >= VISIBLE_LINES {
            return;
        }
        let next = self.scanline + 1;
        let mut found = 0usize;
        for n in 0..64usize {
            if !self.in_range(next, self.oam[n * 4]) {
                continue;
            }
            if found >= 8 {
                self.status |= 0x20; // sprite overflow, $2002 bit 5
                return;
            }
            self.secondary_oam[found * 4..found * 4 + 4]
                .copy_from_slice(&self.oam[n * 4..n * 4 + 4]);
            self.secondary_sprite0[found] = n == 0;
            found += 1;
        }
    }

    /// Is an OAM entry with Y `y` in range for screen row `screen_row`?
    ///
    /// The OAM Y coordinate is "scanline coordinate minus one of object's top
    /// pixel row" (2C02 reference), so the in-range rows are `y + 1 ..= y +
    /// height`. The comparator that produces that range is only NINE bits wide,
    /// so `screen_row - y` WRAPS modulo 512: an OAM Y of $EF-$FF is far below
    /// every visible scanline in that arithmetic and lands off the top of the
    /// screen, which is why NESdev "PPU OAM" documents those values as the way
    /// to hide a sprite. Without the wrap $FF would be the largest Y and would
    /// hide nothing.
    ///
    /// This same 9-bit comparator output is the 2C02 reference's "range result"
    /// value, and that is all it is: it is constant across a scanline, so it is
    /// never a per-row bit and never selects a pattern table.
    fn in_range(&self, screen_row: u32, y: u8) -> bool {
        let diff = (screen_row as i32 - y as i32) & 0x1FF;
        (1..=self.sprite_height() as i32).contains(&diff)
    }

    /// Load the eight sprite output units for the NEXT scanline. Hardware
    /// spreads this over dots 257-320, eight dots each, so every unit is
    /// loaded here before that scanline's first pixel.
    fn load_sprite_units(&mut self) {
        let next = self.scanline + 1;
        let height = self.sprite_height();
        for i in 0..8usize {
            self.sprites[i] = Sprite::default();
            let o = i * 4;
            let y = self.secondary_oam[o];
            if !self.in_range(next, y) {
                continue; // an unused slot: transparent, by construction
            }
            let tile = self.secondary_oam[o + 1];
            let attr = self.secondary_oam[o + 2];
            let fine = (next - y as u32 - 1) as u8;
            // "Invert the 3/4-bit (8/16 scanlines/object mode) scanline address
            // used to access an object tile": for 8x16 that also exchanges the
            // two halves.
            let row = if attr & 0x80 != 0 {
                height as u8 - 1 - fine
            } else {
                fine
            };
            let addr = self.sprite_pattern_address(tile, row);
            let (lo, hi) = (self.chr[self.crbank(addr)], self.chr[self.crbank(addr + 8)]);
            let (lo, hi) = if attr & 0x40 != 0 {
                (lo.reverse_bits(), hi.reverse_bits())
            } else {
                (lo, hi)
            };
            self.sprites[i] = Sprite {
                x: self.secondary_oam[o + 3],
                lo,
                hi,
                attr,
                is_sprite0: self.secondary_sprite0[i],
            };
        }
    }

    /// The CHR address of row `row` of a sprite's pattern, in units of the LOW
    /// bitplane; the high one is 8 bytes above it (NESdev "PPU rendering").
    ///
    /// 8x8: the whole tile index, pattern table from `$2000` bit 3.
    ///
    /// 8x16: an 8x16 sprite whose top tile is `T` occupies 32 CONTIGUOUS
    /// bytes -- bank from `T & 1`, block from `(T & 0xFE) * 16`, and the bottom
    /// half is the next 16 bytes with NO bank change. So tile `$02` is
    /// `$0020-$003F` inside one bank, NOT `$0020` top and `$1020` bottom
    /// (NESdev "PPU OAM" -> Sprite pattern tables: `$00: $0000-$001F,
    /// $01: $1000-$101F, $02: $0020-$003F, $03: $1020-$103F, $FE: $0FE0-$0FFF,
    /// $FF: $1FE0-$1FFF`).
    ///
    /// `row & 7` is the offset inside the half: the 2C02's "lower 3 bits" fine
    /// vertical offset. The 2C02's "MSB of the range result value become the LSB
    /// of the indexed tile" describes the internal INDEX (whose low bit then
    /// selects the table), not the address: the range result is the 9-bit
    /// `scanline - Y` comparator output, constant across a scanline, so it
    /// cannot be a per-row bit and cannot pick a pattern table.
    fn sprite_pattern_address(&self, tile: u8, row: u8) -> u16 {
        if self.ctrl & 0x20 == 0 {
            // "$2000 bit 3 applies to 8x8 sprites" (NESdev "PPU pattern
            // tables") -- bit 4 is the BACKGROUND's table, not this one.
            let base = if self.ctrl & 0x08 != 0 {
                0x1000
            } else {
                0x0000
            };
            return base + tile as u16 * 16 + (row & 7) as u16;
        }
        (tile & 1) as u16 * 0x1000
            + (tile & 0xFE) as u16 * 16
            + (row & 7) as u16
            + if row >= 8 { 16 } else { 0 }
    }

    /// A background pattern address for the tile named by the nametable byte
    /// just fetched: `$2000` bit 4 picks the table, bits 12-14 of `v` are the
    /// row inside the tile, and the low plane lives 8 bytes below the high one.
    fn pattern_address(&self, plane: u16) -> u16 {
        let base = if self.ctrl & 0x10 != 0 {
            0x1000
        } else {
            0x0000
        };
        base + self.fetch.tile as u16 * 16 + ((self.v >> 12) & 7) + plane
    }

    /// Put one PPU address on the bus and clock the MMC3's filtered A12
    /// detector when bit 12 rises after a low period.
    fn observe_a12(&mut self, addr: u16) {
        if addr & 0x1000 != 0 {
            if self.a12_low_dots > A12_FILTER_DOTS {
                self.a12_rise = true;
            }
            self.a12_low_dots = 0;
        } else if self.a12_low_dots == 0 {
            self.a12_low_dots = 1;
        }
    }

    /// The sprite pattern fetches on dots 257-320 drive A12 just as background
    /// fetches do. Each sprite has two garbage nametable reads, then the low
    /// and high pattern bytes. Unused slots still fetch tile `$FF`, which
    /// matters for 8x16 sprites: that dummy tile selects CHR bank 1.
    fn sprite_fetch_dot(&mut self) {
        if !(257..=320).contains(&self.dot) {
            return;
        }
        let slot = ((self.dot - 257) / 8) as usize;
        let phase = (self.dot - 257) % 8;
        match phase {
            0 | 2 => self.observe_a12(0x2000 | (self.v & 0x0FFF)),
            4 | 6 => {
                let tile = self.secondary_oam[slot * 4 + 1];
                self.observe_a12(self.sprite_pattern_address(tile, 0));
            }
            _ => {}
        }
    }

    /// The backdrop is palette RAM `$3F00`: a stock NES grounds the EXT pins,
    /// so this is what every transparent pixel and the border show.
    fn backdrop(&self) -> u32 {
        self.colour(self.palette[0])
    }

    /// The single place a palette RAM byte becomes a framebuffer pixel, and
    /// therefore the single place the mask's unimplemented bits would show.
    fn colour(&self, value: u8) -> u32 {
        // Greyscale ($2001 bit 0) ANDs the palette value with $30.
        //
        // ponytail: colour emphasis ($2001 bits 5-7) and backdrop override (a
        // `v` pointing into palette RAM drawing that entry with rendering off)
        // are NOT implemented -- no test ROM in this repo exercises them and
        // SMB plays at $1E. Add when a game needs them.
        let value = if self.mask & 0x01 != 0 {
            value & 0x30
        } else {
            value & 0x3F
        };
        match self.region {
            Region::Ntsc => PALETTE[value as usize],
            Region::Pal => PAL_PALETTE[value as usize],
        }
    }

    /// "Bits 0-4 are incremented, with overflow toggling bit 10."
    fn increment_coarse_x(&mut self) {
        if self.v & 0x001F == 31 {
            self.v &= !0x001F;
            self.v ^= 0x0400;
        } else {
            self.v += 1;
        }
    }

    /// "Fine Y is incremented at dot 256 of each scanline, overflowing to coarse
    /// Y, and finally adjusted to wrap among the nametables vertically." Row 29
    /// is the last row of tiles; row 31 wraps to 0 without switching nametables.
    fn increment_y(&mut self) {
        if self.v & 0x7000 != 0x7000 {
            self.v += 0x1000;
            return;
        }
        self.v &= !0x7000;
        let y = match (self.v & 0x03E0) >> 5 {
            29 => {
                self.v ^= 0x0800;
                0
            }
            31 => 0,
            y => y + 1,
        };
        self.v = (self.v & !0x03E0) | (y << 5);
    }

    pub fn frame_ready(&self) -> bool {
        self.frame_done
    }

    /// Consume the filtered A12 rising edge, if one is pending. MMC3 uses
    /// exactly these edges; a synthetic once-per-line pulse would miss boards
    /// that run the background from `$1000` and sprites from `$0000`, and would
    /// not see the extra edges of 8x16 sprites or `$2006`/`$2007`.
    pub fn take_a12_rise(&mut self) -> bool {
        std::mem::take(&mut self.a12_rise)
    }

    pub fn framebuffer(&self) -> &[u32] {
        &self.framebuffer
    }

    /// Consume the frame-ready flag. The framebuffer is left in place and read
    /// through `framebuffer()`, so a frame costs no copy at all.
    pub fn take_frame(&mut self) {
        self.frame_done = false;
    }

    pub fn nmi_pending(&self) -> bool {
        self.nmi
    }

    pub fn take_nmi(&mut self) {
        self.nmi = false;
    }

    pub fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr & 7 {
            2 => {
                // Bits 4-0 are the PPU's open bus; this machine masks them to 0.
                let value = self.status & 0xE0;
                self.status &= !0x80;
                self.write_latch = false;
                self.nmi = false;
                value
            }
            4 => self.oam[self.oam_addr as usize],
            7 => {
                let addr = self.v & 0x3FFF;
                self.observe_a12(addr);
                // "The referenced 6-bit palette data is returned immediately
                // instead of going to the internal read buffer", and the same
                // access performs the shadowed nametable read underneath it.
                if addr >= 0x3F00 {
                    let value = self.palette[palette_index(addr)] & 0x3F;
                    self.read_buffer = self.ppu_bus_read(addr);
                    self.increment_v();
                    self.observe_a12(self.v & 0x3FFF);
                    return value;
                }
                let value = self.read_buffer;
                self.read_buffer = self.ppu_bus_read(addr);
                self.increment_v();
                self.observe_a12(self.v & 0x3FFF);
                value
            }
            // $2000, $2001, $2003, $2005 and $2006 are write-only; they read
            // back as open bus, which is masked to 0 like $2002's low bits.
            _ => 0,
        }
    }

    pub fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr & 7 {
            0 => {
                self.ctrl = val;
                // "$2000 (PPUCTRL) write: t: ...GH.. ........ <- d: ......GH".
                // Nothing else in `t` moves, and SMB does write $2000 mid-frame
                // (NESdev "PPUCTRL - Bit 0 race condition" says so), so wiping
                // the scroll here would lose it.
                self.t = (self.t & !0x0C00) | ((val as u16 & 0x03) << 10);
                // "Changing NMI enable from 0 to 1 while the vblank flag in
                // PPUSTATUS is 1 will immediately trigger an NMI."
                if val & 0x80 != 0 && self.status & 0x80 != 0 {
                    self.nmi = true;
                }
            }
            1 => self.mask = val,
            3 => self.oam_addr = val,
            4 => {
                self.oam[self.oam_addr as usize] = val;
                self.oam_addr = self.oam_addr.wrapping_add(1);
            }
            5 => {
                if self.write_latch {
                    self.t =
                        (self.t & !0x73E0) | ((val as u16 & 0x07) << 12) | ((val as u16 >> 3) << 5);
                } else {
                    self.t = (self.t & !0x001F) | (val as u16 >> 3);
                    self.fine_x = val & 7;
                }
                self.write_latch = !self.write_latch;
            }
            6 => {
                if self.write_latch {
                    self.t = (self.t & 0xFF00) | val as u16;
                    self.v = self.t;
                    // PPUADDR writes also drive A12; the MMC3 sees those edges
                    // even with rendering disabled, which its test ROMs and a
                    // few games use to clock the counter by hand.
                    self.observe_a12(self.v & 0x3FFF);
                } else {
                    // Bits 14-15 of the written value are ignored and bit 14 of
                    // `t` is forced to 0.
                    self.t = (self.t & 0x00FF) | ((val as u16 & 0x3F) << 8);
                }
                self.write_latch = !self.write_latch;
            }
            7 => {
                let addr = self.v & 0x3FFF;
                self.observe_a12(addr);
                if addr >= 0x3F00 {
                    self.palette[palette_index(addr)] = val & 0x3F;
                } else if addr >= 0x2000 {
                    self.vram[vram_index(addr, self.mirroring)] = val;
                } else if self.chr_ram {
                    // The game uploads its tiles here through `$2007`. CHR RAM
                    // goes through the same bank map as CHR ROM, because MMC1
                    // can bank its 8 KiB of pattern RAM too.
                    let at = self.crbank(addr);
                    self.chr[at] = val;
                }
                // `$0000-$1FFF` is pattern memory. On a cartridge with CHR ROM
                // the write goes NOWHERE; on one with none it is the 8 KiB RAM
                // pattern space written above. `v` advances either way --
                // hardware steps the address register on every `$2007` access
                // whether or not the byte lands, and getting that wrong hangs
                // any game that streams data through a fixed address window.
                self.increment_v();
                self.observe_a12(self.v & 0x3FFF);
            }
            // $2002 is read-only: "Reading from $2002 clears the vblank flag
            // ... Writes here have no effect." (2C02 technical reference.)
            _ => {}
        }
    }

    /// The `$2007` read buffer is filled from PPU memory: pattern memory below
    /// `$2000`, nametable RAM above it (palette RAM is handled by the caller).
    fn ppu_bus_read(&self, addr: u16) -> u8 {
        if addr < 0x2000 {
            self.chr[self.crbank(addr)]
        } else {
            self.vram[vram_index(addr, self.mirroring)]
        }
    }

    /// `$2007` advances `v` by 1, or by 32 when `$2000` bit 2 is set.
    fn increment_v(&mut self) {
        let step = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
        self.v = self.v.wrapping_add(step) & 0x7FFF;
    }

    /// Index into the whole CHR array for a `$0000-$1FFF` pattern address, via
    /// the 1 KiB slot the mapper currently maps there. The modulo keeps a
    /// mapper register from reading past the image when it selects a bank the
    /// cartridge does not carry (or a 16 KiB CHR array from a larger board).
    fn crbank(&self, addr: u16) -> usize {
        (self.chr_map[(addr as usize >> 10) & 7] * 1024 + (addr as usize & 0x03FF)) % self.chr.len()
    }

    /// Map each 1 KiB slot at `$0000-$1FFF` individually. MMC1 maps 4 KiB
    /// windows and MMC3 maps 1 KiB windows; both are just assignments here.
    pub fn set_chr_map(&mut self, map: [usize; 8]) {
        self.chr_map = map;
    }

    /// Rewire the logical nametables. MMC1 changes this at runtime; simpler
    /// boards set it once from the iNES header when the bus is built.
    pub fn set_mirroring(&mut self, mirroring: Mirroring) {
        self.mirroring = mirroring;
    }

    /// The whole CHR image the `Cartridge` parsed out of the file, banked by
    /// the mapper. A short -- or absent -- image is zero-filled to at least the
    /// PPU's 8 KiB pattern window. With no image at all, the window is RAM:
    /// every `$2007` write into pattern space lands there.
    pub fn load_chr_rom(&mut self, data: &[u8]) {
        self.chr = vec![0; data.len().max(8192)];
        self.chr[..data.len()].copy_from_slice(data);
        self.chr_ram = data.is_empty();
    }

    /// Everything the PPU will still be using when the next dot arrives: the
    /// two RAMs, the CHR bank map and RAM image, the register file, the three
    /// pipeline tiles, the two background shifters and their attributes, and
    /// the eight sprite output units. The framebuffer is output and is not
    /// saved; the next frame redraws it.
    pub(crate) fn save_state(&self, writer: &mut StateWriter) {
        writer.bytes(&self.vram);
        writer.bytes(&self.palette);
        // CHR ROM is in the cartridge, not in the state. Only the mutable RAM
        // image needs saving, and then its length keeps the read bounded.
        writer.bool(self.chr_ram);
        if self.chr_ram {
            writer.u32(self.chr.len() as u32);
            writer.bytes(&self.chr);
        }
        for bank in self.chr_map {
            writer.u32(bank as u32);
        }
        writer.u8(self.mirroring.id());
        writer.bytes(&self.oam);
        writer.bytes(&self.secondary_oam);
        for &sprite0 in &self.secondary_sprite0 {
            writer.bool(sprite0);
        }
        writer.u8(self.ctrl);
        writer.u8(self.mask);
        writer.u8(self.status);
        writer.u8(self.oam_addr);
        writer.u16(self.v);
        writer.u16(self.t);
        writer.u8(self.fine_x);
        writer.bool(self.write_latch);
        writer.u8(self.read_buffer);
        writer.u32(self.scanline);
        writer.u32(self.dot);
        writer.bool(self.odd_frame);
        writer.bool(self.frame_done);
        writer.bool(self.nmi);
        self.fetch.save_state(writer);
        self.prev.save_state(writer);
        self.cur.save_state(writer);
        writer.u16(self.bg_lo);
        writer.u16(self.bg_hi);
        writer.u8(self.attr_prev);
        writer.u8(self.attr_cur);
        for sprite in &self.sprites {
            sprite.save_state(writer);
        }
    }

    pub(crate) fn load_state(&mut self, reader: &mut StateReader) -> Result<(), String> {
        self.vram = reader.array()?;
        self.palette = reader.array()?;
        let chr_ram = reader.bool()?;
        if chr_ram != self.chr_ram {
            return Err(
                "save state's CHR type does not match this cartridge (ROM vs RAM)".to_string(),
            );
        }
        if chr_ram {
            let len = reader.u32()? as usize;
            if len != self.chr.len() {
                return Err(format!(
                    "save state has {len} bytes of CHR RAM; this cartridge has {}",
                    self.chr.len()
                ));
            }
            self.chr.copy_from_slice(reader.bytes(len)?);
        }
        for bank in &mut self.chr_map {
            *bank = reader.u32()? as usize;
        }
        self.mirroring = Mirroring::from_id(reader.u8()?)?;
        self.oam = reader.array()?;
        self.secondary_oam = reader.array()?;
        for sprite0 in &mut self.secondary_sprite0 {
            *sprite0 = reader.bool()?;
        }
        self.ctrl = reader.u8()?;
        self.mask = reader.u8()?;
        self.status = reader.u8()?;
        self.oam_addr = reader.u8()?;
        self.v = reader.u16()?;
        self.t = reader.u16()?;
        self.fine_x = reader.u8()?;
        self.write_latch = reader.bool()?;
        self.read_buffer = reader.u8()?;
        self.scanline = reader.u32()?;
        self.dot = reader.u32()?;
        // A12 filtering is transient bus state and is not part of the state
        // format, so existing mappers' states keep loading unchanged. Start
        // the filter from the restored v address.
        self.a12_low_dots = u32::from(self.v & 0x1000 == 0);
        self.a12_rise = false;
        if self.scanline >= self.region.scanlines() || self.dot >= DOTS_PER_LINE {
            return Err(format!(
                "save state has an impossible PPU position {}.{}",
                self.scanline, self.dot
            ));
        }
        self.odd_frame = reader.bool()?;
        self.frame_done = reader.bool()?;
        self.nmi = reader.bool()?;
        self.fetch.load_state(reader)?;
        self.prev.load_state(reader)?;
        self.cur.load_state(reader)?;
        self.bg_lo = reader.u16()?;
        self.bg_hi = reader.u16()?;
        self.attr_prev = reader.u8()?;
        self.attr_cur = reader.u8()?;
        for sprite in &mut self.sprites {
            sprite.load_state(reader)?;
        }
        Ok(())
    }
}

/// Test-only seams. The PPU is driven through its own `$2000-$3FFF` registers,
/// so the tests exercise the same VRAM and CHR paths the game does; only the
/// raw status/address read-outs need a door.
#[cfg(test)]
impl Ppu {
    fn status(&self) -> u8 {
        self.status
    }

    fn set_status_for_test(&mut self, val: u8) {
        self.status = val;
    }

    fn vram_addr(&self) -> u16 {
        self.v
    }

    /// The temporary address register `t`, where `$2005` and `$2006` park the
    /// scroll and the address until the PPU copies it into `v`.
    fn temp_vram_addr(&self) -> u16 {
        self.t
    }

    /// Overwrite one byte of CHR **ROM** for a test. `$2000-$1FFF` is ROM on this
    /// board, so nothing can reach it through the `$2007` write path and the
    /// tests go through this door instead. No game code ever writes it.
    fn chr_write_for_test(&mut self, addr: u16, val: u8) {
        let at = self.crbank(addr);
        self.chr[at] = val;
    }

    /// Point nametable tile `(x, y)` at `tile` and give it `palette` through the
    /// attribute byte of its 4x4 block, both through `$2006`/`$2007`.
    fn upload_tile_for_test(&mut self, tile: u8, x: u8, y: u8, palette: u8) {
        self.write_vram_for_test(0x2000 + y as u16 * 32 + x as u16, tile);
        // One attribute byte covers a 4x4 block of tiles; its four 2-bit fields
        // pick the palette of each 2x2 sub-block, at column 0, column 1, row 0,
        // row 1 counted from the bottom. NESdev's attribute address puts the
        // coarse Y block in bits 3-5 of the offset and coarse X in bits 0-2:
        // `$23C0 | (v & $0C00) | ((v >> 4) & $38) | ((v >> 2) & $07)`, i.e. the
        // `*8` is on Y, not on X.
        let attr = 0x23C0 + (y as u16 / 4) * 8 + (x as u16 / 4);
        let shift = ((y as u16 / 2) & 1) * 4 + ((x as u16 / 2) & 1) * 2;
        let byte = self.read_vram_for_test(attr);
        self.write_vram_for_test(attr, (byte & !(0x03 << shift)) | ((palette & 3) << shift));
    }

    /// Tick until each listed scanline has been rendered, dots and all.
    fn run_to(&mut self, lines: &[u32]) {
        for &line in lines {
            while self.scanline != line {
                self.tick();
            }
            while self.scanline == line {
                self.tick();
            }
        }
    }

    fn pixel(&self, x: u32, y: u32) -> u32 {
        self.framebuffer[y as usize * 256 + x as usize]
    }

    fn write_vram_for_test(&mut self, addr: u16, val: u8) {
        self.cpu_write(0x2006, (addr >> 8) as u8);
        self.cpu_write(0x2006, addr as u8);
        self.cpu_write(0x2007, val);
    }

    fn read_vram_for_test(&mut self, addr: u16) -> u8 {
        self.cpu_write(0x2006, (addr >> 8) as u8);
        self.cpu_write(0x2006, addr as u8);
        self.read_buffer = self.vram[vram_index(addr, self.mirroring)];
        self.read_buffer
    }

    /// OAM entry `n` -- sprite `n` of 64: Y, tile, attributes, X. The brief's
    /// four-argument version had no X, which cannot place a sprite anywhere on
    /// the screen, so X is the fifth argument.
    fn put_sprite_for_test(&mut self, n: usize, y: u8, tile: u8, attr: u8, x: u8) {
        self.oam[n * 4..n * 4 + 4].copy_from_slice(&[y, tile, attr, x]);
    }

    /// `$2002` bit 6, the sprite-0 hit.
    fn sprite0_hit(&self) -> bool {
        self.status & 0x40 != 0
    }

    /// `$2002` bit 5, sprite overflow.
    fn sprite_overflow(&self) -> bool {
        self.status & 0x20 != 0
    }

    /// An opaque background over the whole screen and an opaque 8x16 sprite 0
    /// whose leftmost pixel is at `x`. `bg_row` is the low-plane pattern byte
    /// every background tile shows on every row -- `$FF` opaque everywhere,
    /// `$00` transparent everywhere, `$80` the leftmost pixel of each tile only.
    ///
    /// Ends by writing `$2000` and `$2005`, so a test only has to add the
    /// `$2001` write (see `show`).
    fn setup_sprite0_over_background_for_test(&mut self, x: u8, bg_row: u8) {
        // Background is tile 0 at $0000; sprite 0 is tile $02, whose 8x16 top
        // half is $0020 and whose bottom half is the next 16 bytes, $0030. They
        // must not share CHR or one would colour the other.
        for row in 0..8u16 {
            self.chr_write_for_test(row, bg_row);
            self.chr_write_for_test(row + 8, 0x00);
            self.chr_write_for_test(0x0020 + row, 0xFF);
            self.chr_write_for_test(0x0028 + row, 0x00);
            self.chr_write_for_test(0x0030 + row, 0xFF);
            self.chr_write_for_test(0x0038 + row, 0x00);
        }
        for r in 0..30u8 {
            for c in 0..32u8 {
                self.upload_tile_for_test(0, c, r, 0); // tile 0, palette 0
            }
        }
        self.write_vram_for_test(0x3F00, 0x0F); // backdrop black
        self.write_vram_for_test(0x3F01, 0x30); // background colour 1 = white
        self.write_vram_for_test(0x3F15, 0x21); // sprite palette 1 colour 1 = blue
                                                // Y = 20 puts the sprite on scanlines 21..36 ("you must subtract 1
                                                // from the sprite's Y coordinate before writing it here").
        self.put_sprite_for_test(0, 20, 0x02, 0x01, x);
        self.cpu_write(0x2000, 0x20); // 8x16 sprites
        self.cpu_write(0x2005, 0);
        self.cpu_write(0x2005, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MMC3 counts 241 A12 rises an NTSC frame: the pre-render line plus 240
    /// visible lines. Vblank fetches nothing, so it must not add clocks.
    #[test]
    fn rendering_produces_241_mapper_clocks_per_frame() {
        let mut p = Ppu::new();
        p.cpu_write(0x2000, 0x08); // background $0000, sprites $1000
        p.cpu_write(0x2001, 0x18);
        let mut clocks = 0;
        for _ in 0..89342 {
            p.tick();
            while p.take_a12_rise() {
                clocks += 1;
            }
        }
        assert_eq!(clocks, 241);
    }

    /// PAL has the same 241 rendering lines; the extra dot on 15 of every 16
    /// bus ticks must not swallow one of the A12 rises.
    #[test]
    fn pal_rendering_produces_241_mapper_clocks_per_frame() {
        let mut p = Ppu::for_region(Region::Pal);
        p.cpu_write(0x2000, 0x08);
        p.cpu_write(0x2001, 0x18);
        let mut clocks = 0;
        for _ in 0..106392 {
            p.tick();
            while p.take_a12_rise() {
                clocks += 1;
            }
        }
        assert_eq!(clocks, 241);
    }

    /// Pattern-table selection decides where A12 rises. With the background in
    /// `$1000` and sprites in `$0000`, the rise is the background prefetch at
    /// the end of the previous line, not a sprite fetch.
    #[test]
    fn background_pattern_table_selection_moves_the_a12_clock() {
        let mut p = Ppu::new();
        p.cpu_write(0x2000, 0x10); // background $1000, sprites $0000
        p.cpu_write(0x2001, 0x18);
        let mut clocks = 0;
        for _ in 0..89342 {
            p.tick();
            while p.take_a12_rise() {
                clocks += 1;
            }
        }
        assert_eq!(clocks, 241, "one rise per rendering line, at dot 325");
    }

    /// Both layers in the low table never raise A12 at all.
    #[test]
    fn both_pattern_tables_low_produce_no_a12_clocks() {
        let mut p = Ppu::new();
        p.cpu_write(0x2000, 0x00);
        p.cpu_write(0x2001, 0x18);
        let mut clocks = 0;
        for _ in 0..89342 {
            p.tick();
            while p.take_a12_rise() {
                clocks += 1;
            }
        }
        assert_eq!(clocks, 0);
    }

    /// 8x16 sprites take their table from the tile index's low bit, not from
    /// `$2000` bit 3. Eight in-range even tiles therefore stay in `$0000`
    /// while the background does too, and no sprite fetch clocks A12.
    #[test]
    fn eight_by_sixteen_sprite_tiles_drive_a12() {
        let mut p = Ppu::new();
        p.cpu_write(0x2000, 0x20); // 8x16 sprites, background $0000
        p.cpu_write(0x2001, 0x18);
        for n in 0..8 {
            p.put_sprite_for_test(n, 0, 0x00, 0, 0);
        }
        // Let scanline 0's evaluation fill secondary OAM, then count only
        // scanline 1's sprite fetches.
        while p.scanline != 1 {
            p.tick();
            while p.take_a12_rise() {}
        }
        let mut clocks = 0;
        while p.scanline == 1 {
            p.tick();
            while p.take_a12_rise() {
                clocks += 1;
            }
        }
        assert_eq!(clocks, 0, "eight even 8x16 tiles all fetch from $0000");

        for n in 0..8 {
            p.put_sprite_for_test(n, 0, 0x01, 0, 0); // odd tile -> bank 1
        }
        // Line 2's evaluation sees the odd tiles; count line 3's fetches.
        while p.scanline != 3 {
            p.tick();
            while p.take_a12_rise() {}
        }
        let mut clocks = 0;
        while p.scanline == 3 {
            p.tick();
            while p.take_a12_rise() {
                clocks += 1;
            }
        }
        assert_eq!(clocks, 1, "odd 8x16 tiles raise A12 at the sprite fetch");
    }

    /// `$2006` and `$2007` put their VRAM address on the PPU bus, so toggling
    /// A12 that way clocks the MMC3 filter even while rendering is disabled.
    #[test]
    fn ppu_address_writes_drive_the_a12_filter() {
        let mut p = Ppu::new();
        p.cpu_write(0x2006, 0x00);
        p.cpu_write(0x2006, 0x00); // v = $0000, A12 low
        for _ in 0..12 {
            p.tick();
        }
        p.cpu_write(0x2006, 0x10);
        p.cpu_write(0x2006, 0x00); // v = $1000, A12 high
        assert!(p.take_a12_rise(), "a $2006 write must clock A12");

        p.cpu_write(0x2006, 0x0F);
        p.cpu_write(0x2006, 0xFF); // v = $0FFF, A12 low
        for _ in 0..12 {
            p.tick();
        }
        assert_eq!(p.cpu_read(0x2007), 0, "reading $0FFF");
        assert!(
            p.take_a12_rise(),
            "$2007's increment to $1000 must clock A12"
        );

        p.cpu_write(0x2006, 0x0F);
        p.cpu_write(0x2006, 0xFF); // v = $0FFF, A12 low
        for _ in 0..12 {
            p.tick();
        }
        p.cpu_write(0x2007, 0);
        assert!(
            p.take_a12_rise(),
            "$2007's increment to $1000 must clock A12"
        );
    }

    #[test]
    fn reading_2002_clears_vblank_and_resets_the_2006_latch() {
        let mut p = Ppu::new();
        // The brief set the vblank flag with `cpu_write(0x2002, 0)`. It cannot:
        // $2002 is read-only and "Writes here have no effect" (2C02 technical
        // reference), which blargg's ppu_vbl_nmi test 04 pins on hardware
        // ("Writing $2002 shouldn't affect VBL flag"). `set_status_for_test` is
        // the same door the next test uses, so the rest of the test is verbatim.
        p.set_status_for_test(0x80);
        p.cpu_write(0x2006, 0x21); // high byte
        p.cpu_write(0x2006, 0x08); // low byte
        p.cpu_write(0x2006, 0x21); // Leaves the latch half-open, which is the
                                   // only state a $2002 read can be caught in:
                                   // after a whole $2006 pair the latch is
                                   // already clear, so without this line the
                                   // reset is invisible.
        let _ = p.cpu_read(0x2002);
        p.cpu_write(0x2006, 0x21); // latch reset: this is the FIRST write again
        p.cpu_write(0x2006, 0x08);
        assert_eq!(
            p.vram_addr(),
            0x2108,
            "a 2002 read must reset the write latch"
        );
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
        p.cpu_write(0x2006, 0x20);
        p.cpu_write(0x2006, 0x00);
        p.cpu_write(0x2007, 0xAB);
        p.cpu_write(0x2006, 0x28);
        p.cpu_write(0x2006, 0x00); // $2800 == $2000
                                   // $2007 is one byte late: a read returns the internal buffer and only
                                   // then loads the addressed byte into it (NESdev "PPUDATA - The PPUDATA
                                   // read buffer"). Priming read added; the assertion is the brief's.
        let _ = p.cpu_read(0x2007);
        assert_eq!(p.cpu_read(0x2007), 0xAB);
    }

    #[test]
    fn horizontal_mirroring_folds_the_second_nametable_onto_the_first() {
        let mut p = Ppu::new();
        p.set_mirroring(Mirroring::Horizontal);
        p.write_vram_for_test(0x2000, 0x11);
        p.write_vram_for_test(0x2400, 0x22); // $2400 == $2000
        assert_eq!(p.read_vram_for_test(0x2000), 0x22);
        p.write_vram_for_test(0x2800, 0x33);
        assert_eq!(
            p.read_vram_for_test(0x2000),
            0x22,
            "$2800 is the other bank"
        );
        assert_eq!(p.read_vram_for_test(0x2C00), 0x33, "$2C00 == $2800");
    }

    #[test]
    fn the_single_screen_modes_make_all_four_nametables_one() {
        for mode in [Mirroring::SingleLower, Mirroring::SingleUpper] {
            let mut p = Ppu::new();
            p.set_mirroring(mode);
            p.write_vram_for_test(0x2000, 0x5A);
            for addr in [0x2400, 0x2800, 0x2C00] {
                assert_eq!(
                    p.read_vram_for_test(addr),
                    0x5A,
                    "{mode:?}: ${addr:04X} must see the same 1 KiB"
                );
            }
        }
    }

    #[test]
    fn the_chr_map_can_swap_four_kib_windows_independently() {
        let mut p = Ppu::new();
        p.set_chr_map([4, 5, 6, 7, 0, 1, 2, 3]);
        assert_eq!(p.crbank(0x0000), 0x1000, "$0000 reads the second 4 KiB");
        assert_eq!(p.crbank(0x1000), 0x0000, "$1000 reads the first 4 KiB");
        assert_eq!(p.crbank(0x1FFF), 0x0FFF, "offset inside the mapped bank");
    }

    #[test]
    fn a_tile_is_fetched_and_shifted_into_the_framebuffer() {
        let mut p = Ppu::new();
        // CHR bank 0 is written directly by the caller in these tests.
        p.chr_write_for_test(0x0000, 0xFF); // tile 0, low plane: all pixels set
        p.chr_write_for_test(0x0001, 0x00); // tile 0, high plane: all clear
        p.upload_tile_for_test(0, 0x00, 0x00, 0); // tile 0 at nametable (0,0), palette 0
                                                  // Two lines the brief leaves out, because neither its pinned colour nor
                                                  // its "did not bleed" assertion can hold without them:
                                                  //  - palette RAM is unspecified at power-on, and the 2C02 master palette
                                                  //    has no white at entry 1, so $3F00/$3F01 are written explicitly.
                                                  //    $0F is $000000 and $30 is $FFFFFF in `2C02G_U_wiki.pal`.
                                                  //  - VRAM reads as zeroes and nametable entry 0 IS tile 0, so without
                                                  //    this the tile at x=8..16 would be tile 0 again and the second
                                                  //    assertion could never fail. Tile 1's CHR is still blank.
        p.upload_tile_for_test(1, 0x01, 0x00, 0); // tile 1 at nametable (1,0)
        p.write_vram_for_test(0x3F00, 0x0F);
        p.write_vram_for_test(0x3F01, 0x30);
        // `$2006`/`$2007` leave `v` and `t` pointing at palette RAM, and the
        // scroll is copied out of `t` at dot 257 and at dots 280-304, so the
        // program has to set the scroll again afterwards: "After using PPUADDR
        // ($2006), the program must always set PPUSCROLL and PPUCTRL again."
        // The brief's setup did not, so its scroll was $3F02 and the frame
        // would be garbage on hardware too.
        p.cpu_write(0x2000, 0);
        p.cpu_write(0x2005, 0);
        p.cpu_write(0x2005, 0);
        p.cpu_write(0x2001, 0x1E); // show bg+sprites, no clipping
        p.run_to(&[21]); // stop at the first visible scanline
        for x in 0..8 {
            assert_eq!(
                p.pixel(x, 0),
                0x00_FF_FF_FF,
                "tile bit not shifted to the right pixel"
            );
        }
        for x in 8..16 {
            assert_eq!(p.pixel(x, 0), BACKDROP, "tile bled past its 8 pixels");
        }
    }

    /// A CHR image larger than the PPU's 8 KiB window is real on MMC1 boards.
    /// It must be kept in full; selecting a high bank must read that bank, not
    /// truncate or index past the end of the array.
    #[test]
    fn a_large_chr_image_keeps_its_upper_banks_in_bounds() {
        let mut chr = vec![0u8; 32 * 1024];
        chr[24 * 1024] = 0x5A;
        let mut p = Ppu::new();
        p.load_chr_rom(&chr);
        p.set_chr_map([24, 25, 26, 27, 28, 29, 30, 31]);
        assert_eq!(p.crbank(0x0000), 24 * 1024);
        assert_eq!(p.ppu_bus_read(0x0000), 0x5A);
        assert_eq!(p.crbank(0x1FFF), 31 * 1024 + 0x03FF);
    }

    /// apu_reset's images carry 8 KiB of CHR, which is the PPU's whole pattern
    /// window; a CHR RAM cart (no CHR bytes at all) gets the same 8 KiB zeroed.
    #[test]
    fn an_8k_chr_image_loads_without_panicking() {
        let mut p = Ppu::new();
        p.load_chr_rom(&[0xAA; 8192]); // apu_reset carries 8 KiB, not 16
        assert_eq!(p.chr.len(), 8192, "the whole 8 KiB pattern window");
        assert_eq!(p.chr[0], 0xAA);
        p.load_chr_rom(&[]); // the 2005 suite: no CHR bytes at all
        assert_eq!(p.chr.len(), 8192, "CHR RAM is 8 KiB");
        assert_eq!(
            p.chr[0], 0x00,
            "an empty image leaves a zeroed pattern space"
        );
    }

    /// With CHR ROM in the file, a `$2007` write into pattern space must still
    /// go nowhere -- in particular not onto nametable RAM, which is where
    /// `vram_index` would put it.
    #[test]
    fn a_pattern_write_goes_nowhere_when_the_cartridge_has_chr_rom() {
        let mut p = Ppu::new();
        p.cpu_write(0x2006, 0x00);
        p.cpu_write(0x2006, 0x00);
        p.cpu_write(0x2007, 0xAA);
        assert_eq!(p.vram[0], 0x00, "not masked onto the nametables");
        assert_eq!(p.chr[0], 0x00, "and not into CHR ROM either");
    }

    /// Mapper 66's register bits 1-0 pick which 8 KiB CHR bank is mapped at
    /// `$0000-$1FFF`, and the game flips that bank mid-frame to show both
    /// games' logos on the title screen at once. The nametable is identical
    /// either way -- here bank 0's tile 0 is solid colour 1 and bank 1's copy of
    /// the same 16 bytes is all zero -- so only the picture can say which bank
    /// is mapped.
    #[test]
    fn the_chr_bank_register_decides_which_8_kib_pattern_table_is_fetched() {
        let mut chr = [0u8; 16384];
        chr[..8].fill(0xFF); // bank 0, tile 0 low plane: opaque, colour 1
        for bank in 0..2 {
            let mut p = Ppu::new();
            p.load_chr_rom(&chr);
            p.write_vram_for_test(0x3F00, 0x0F); // backdrop black
            p.write_vram_for_test(0x3F01, 0x30); // colour 1 = white
            p.upload_tile_for_test(0, 0x00, 0x00, 0); // tile 0 at (0, 0)
            p.set_chr_map(std::array::from_fn(|slot| bank * 8 + slot));
            show(&mut p, 0, 0);
            p.run_to(&[0]);
            let want = if bank == 0 { 0x00_FF_FF_FF } else { BACKDROP };
            for x in 0..8u32 {
                assert_eq!(p.pixel(x, 0), want, "CHR bank {bank}: x={x}");
            }
        }
    }

    /// Every tile on screen carries pattern value 1, and every attribute byte
    /// reads 0,1,2,3 in quadrant order, so the only thing that can change a
    /// pixel's colour is WHICH quadrant its tile reads.
    const QUADRANT_COLOUR: [u8; 4] = [0x30, 0x21, 0x16, 0x2A];

    #[test]
    fn every_attribute_quadrant_selects_its_own_palette() {
        let mut p = Ppu::new();
        // Tile 0: low plane all ones, high plane all zeros => pattern value 1.
        for row in 0..8u16 {
            p.chr_write_for_test(row, 0xFF);
            p.chr_write_for_test(row + 8, 0x00);
        }
        for r in 0..30u8 {
            for c in 0..32u8 {
                p.upload_tile_for_test(0, c, r, 0);
            }
        }
        // Four unmistakable background palettes.
        for (quadrant, entry) in QUADRANT_COLOUR.iter().enumerate() {
            p.write_vram_for_test(0x3F00 + (quadrant as u16) * 4, 0x0F);
            p.write_vram_for_test(0x3F01 + (quadrant as u16) * 4, *entry);
        }
        // Every attribute byte reads 0,1,2,3 in quadrant order. Bit 1 of coarse X
        // and coarse Y pick the quadrant, and within a 4-tile attribute group
        // that bit is just the tile's offset >> 1 -- so quadrant 0 lives in bits
        // 0-1 of every byte, 1 in bits 2-3, 2 in bits 4-5, 3 in bits 6-7. That is
        // the same byte everywhere: 0b11_10_01_00.
        for byte in 0x27C0u16..0x2800 {
            p.write_vram_for_test(byte, 0xE4);
        }
        show_ctrl(&mut p, 0x01, 0, 0);
        p.run_to(&[120]);
        for ty in 0..16u8 {
            for tx in 0..16u8 {
                let quadrant = ((ty >> 1) & 1) * 2 + ((tx >> 1) & 1);
                let want = PALETTE[QUADRANT_COLOUR[quadrant as usize] as usize];
                assert_eq!(
                    p.pixel(tx as u32 * 8, ty as u32 * 8),
                    want,
                    "tile ({tx},{ty}) is in quadrant {quadrant} and picked the wrong palette"
                );
            }
        }
    }

    /// `$2001` is `BGRs bMmG`: bit 4 (`s`) is the sprite enable and bit 2 (`M`)
    /// only lets sprites into the leftmost 8 pixels. The game writes `$18` at
    /// boot to show both layers, and a PPU that reads the enable as bit 2 shows
    /// no sprite at all -- on any scanline, not just a clipped one.
    #[test]
    fn bit_4_of_2001_is_the_sprite_enable() {
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(100, 0xFF);
        p.cpu_write(0x2001, 0x18); // bg + sprites, nothing clipped
        p.run_to(&[21]);
        assert_eq!(
            p.pixel(100, 21),
            0x00_4A_9F_FF,
            "bit 4 alone must switch the sprite layer on"
        );
        assert_eq!(
            p.pixel(60, 21),
            0x00_FF_FF_FF,
            "bit 3 alone must switch the background layer on"
        );
    }

    /// Turn rendering on at a scroll position. Call this LAST: `$2006`/`$2007`
    /// leave `v` and `t` pointing wherever the last write ended, and the scroll
    /// is copied out of `t` at dot 257 and at dots 280-304, so a game sets
    /// `$2000` and `$2005` again after its VRAM writes.
    fn show(p: &mut Ppu, fine_x: u8, y: u8) {
        show_ctrl(p, 0, fine_x, y);
    }

    /// `show` with `$2000` under the test's control -- bit 5 for 8x16 sprites,
    /// bit 3 for the 8x8 sprite pattern table.
    fn show_ctrl(p: &mut Ppu, ctrl: u8, fine_x: u8, y: u8) {
        p.cpu_write(0x2000, ctrl);
        p.cpu_write(0x2005, fine_x);
        p.cpu_write(0x2005, y);
        p.cpu_write(0x2001, 0x1E); // show bg+sprites, no clipping
    }

    /// Sprite tile `$02` -- the top half of an 8x16 sprite -- in colour 1, and
    /// its bottom half, the next 16 bytes at `$0030`, in colour 2. Which half of
    /// the sprite a screen row shows then says which half was fetched. NESdev
    /// "PPU OAM" -> Sprite pattern tables: `$02: $0020-$003F`, ONE bank.
    fn two_colour_sprite(p: &mut Ppu) {
        for row in 0..8u16 {
            p.chr_write_for_test(0x0020 + row, 0xFF); // colour 1
            p.chr_write_for_test(0x0028 + row, 0x00);
            p.chr_write_for_test(0x0030 + row, 0x00);
            p.chr_write_for_test(0x0038 + row, 0xFF); // colour 2
        }
        p.write_vram_for_test(0x3F00, 0x0F); // backdrop black
        p.write_vram_for_test(0x3F11, 0x30); // sprite palette 0 colour 1 = $FFFFFF
        p.write_vram_for_test(0x3F12, 0x21); // sprite palette 0 colour 2 = $004A9FFF
    }

    /// Tile `tile`'s row 0: `lo` in the low plane, `hi` in the high one. The
    /// high plane is 8 bytes above the low one, so tile 1's row 0 is $0010 and
    /// $0018 -- NOT $0011, which is tile 1's low plane row 1.
    fn pattern(p: &mut Ppu, tile: u8, lo: u8, hi: u8) {
        p.chr_write_for_test(tile as u16 * 16, lo);
        p.chr_write_for_test(tile as u16 * 16 + 8, hi);
    }

    /// Every other row of `tile` opaque, so which row of the tile is being drawn
    /// is visible in the picture.
    fn striped(p: &mut Ppu, tile: u8) {
        for row in 0..8u16 {
            p.chr_write_for_test(
                tile as u16 * 16 + row,
                if row % 2 == 0 { 0xFF } else { 0x00 },
            );
            p.chr_write_for_test(tile as u16 * 16 + row + 8, 0x00);
        }
    }

    /// Every row of `tile` opaque in colour 1, so the picture looks the same
    /// wherever the vertical scroll lands.
    fn solid(p: &mut Ppu, tile: u8) {
        for row in 0..8u16 {
            p.chr_write_for_test(tile as u16 * 16 + row, 0xFF);
            p.chr_write_for_test(tile as u16 * 16 + row + 8, 0x00);
        }
    }

    /// Which of the four palettes the attribute byte hands the tile at nametable
    /// column `c`, row `r`: coarse X bit 1 in the low field, coarse Y bit 1 in
    /// the high one.
    fn palette_of(c: u32, r: u32) -> u8 {
        (((c >> 1) & 1) | (((r >> 1) & 1) << 1)) as u8
    }

    /// The pattern byte the tile at nametable column `c` shows on pixel row
    /// `j`: the column in bits 7-5 and 1, the pixel row in bits 4-2, and bit 0
    /// forced on so the LAST pixel of every tile is opaque rather than
    /// transparent.
    fn row_pattern(c: u32, j: u32) -> u8 {
        ((((c & 7) << 5) | (((c >> 3) & 1) << 1) | ((j & 7) << 2)) | 1) as u8
    }

    /// Fine X is the reason the shifters hold two tiles. Pixel 0 of the screen is
    /// pixel `fine_x` of the first tile, and the last `fine_x` pixels of any tile
    /// are pixels of the tile after it. With fine X = 3 the first tile shows its
    /// bits 4..0 and tile 1 takes over from screen pixel 5.
    #[test]
    fn fine_x_scroll_continues_into_the_tile_to_the_right() {
        let mut p = Ppu::new();
        pattern(&mut p, 0, 0xFF, 0x00); // tile 0: every pixel colour 1
        pattern(&mut p, 1, 0x00, 0xFF); // tile 1: every pixel colour 2
        p.upload_tile_for_test(0, 0, 0, 0);
        p.upload_tile_for_test(1, 1, 0, 0);
        p.upload_tile_for_test(3, 2, 0, 0); // tile 3: blank CHR, so the end of
                                            // the spill is visible
        p.write_vram_for_test(0x3F00, 0x0F); // backdrop black
        p.write_vram_for_test(0x3F01, 0x30); // colour 1 = $FFFFFF
        p.write_vram_for_test(0x3F02, 0x21); // colour 2 = $004A9FFF
        show(&mut p, 3, 0);
        p.run_to(&[0]);
        for x in 0..5 {
            assert_eq!(
                p.pixel(x, 0),
                0x00_FF_FF_FF,
                "fine X did not take bits 4..0"
            );
        }
        for x in 5..13 {
            assert_eq!(
                p.pixel(x, 0),
                0x00_4A_9F_FF,
                "fine X did not carry into the next tile at screen pixel 5"
            );
        }
        // Tile 2 is blank CHR, so pixel 13 is where the spill stops.
        for x in 13..21 {
            assert_eq!(p.pixel(x, 0), BACKDROP, "tile 2 did not start at pixel 13");
        }
    }

    /// The attribute byte holds four 2-bit palettes and the field comes from
    /// coarse X bit 1 and coarse Y bit 1 -- the tile's place in the nametable --
    /// NOT from the fine scroll bits. A Y scroll of 16 is fine_y = 0 and
    /// coarse_y = 2, so the picture is already one attribute row down; a Y scroll
    /// of 4 is fine_y = 4 with coarse_y = 0, so it is not; an X scroll of 24 is
    /// coarse_x = 3, one attribute column right. Reading the quadrant off the
    /// fine bits gets all three backwards.
    #[test]
    fn attribute_quadrants_follow_the_coarse_address_not_the_fine_scroll() {
        // One opaque tile, two palettes: white in the left half of the nametable,
        // blue in the right, so the colour says which quadrant the PPU picked.
        for (scroll_x, scroll_y, want) in [
            (0u8, 16u8, 0x00_00_0C_8E), // coarse_y = 2 -> attribute row 1
            (0, 4, 0x00_FF_FF_FF),      // fine_y = 4 -> attribute row 0
            (24, 0, 0x00_4A_9F_FF),     // coarse_x = 3 -> attribute column 1
        ] {
            let mut p = Ppu::new();
            solid(&mut p, 0);
            // One attribute byte covers a 4x4 block of tiles and its four 2-bit
            // fields pick the palette of each 2x2 sub-block, so these three all
            // land in byte $23C0 in three different fields.
            p.upload_tile_for_test(0, 0, 0, 0); // -> palette 0, white
            p.upload_tile_for_test(0, 2, 0, 1); // -> palette 1, blue
            p.upload_tile_for_test(0, 0, 2, 2); // -> palette 2, dark
            p.write_vram_for_test(0x3F00, 0x0F);
            p.write_vram_for_test(0x3F01, 0x30); // palette 0 colour 1 = white
            p.write_vram_for_test(0x3F05, 0x21); // palette 1 colour 1 = blue
            p.write_vram_for_test(0x3F09, 0x01); // palette 2 colour 1 = dark
            show(&mut p, scroll_x, scroll_y);
            p.run_to(&[0]);
            assert_eq!(
                p.pixel(0, 0),
                want,
                "scroll X={scroll_x} Y={scroll_y} chose the wrong attribute quadrant"
            );
        }
    }

    /// Vertical scroll walks the nametable, skips the attribute table, and when
    /// fine Y overflows at row 29 it switches nametable instead of dropping into
    /// `$23C0`. Y scroll 232 puts screen row 0 on nametable 0 row 29; the
    /// striped tile then shows the fine Y counter stepping one line at a time,
    /// and line 8 is where the nametable bit in `v` flips.
    #[test]
    fn vertical_scroll_advances_the_nametable_and_wraps_at_row_29() {
        let mut p = Ppu::new();
        for i in 0..30u8 {
            striped(&mut p, i);
            p.upload_tile_for_test(i, 0, i, 0); // nametable 0, column 0
        }
        p.write_vram_for_test(0x3F00, 0x0F);
        p.write_vram_for_test(0x3F01, 0x30);
        show(&mut p, 0, 29 * 8); // Y scroll 232: coarse_y = 29, fine_y = 0
        p.run_to(&[0]);
        // $7BE0 is coarse Y + the vertical nametable select + fine Y, exactly
        // what dots 280-304 copy out of `t`. $1000 on top is the fine Y that
        // dot 256 of scanline 0 added on top of it.
        assert_eq!(
            p.vram_addr() & 0x7BE0,
            0x13A0,
            "the pre-render line did not reload the vertical scroll from t"
        );
        p.run_to(&[7]);
        for y in 0..8u32 {
            let want = if y % 2 == 0 { 0x00_FF_FF_FF } else { BACKDROP };
            assert_eq!(p.pixel(0, y), want, "row 29 of nametable 0 did not render");
        }
        p.run_to(&[8]);
        // Nametable mirroring here is VERTICAL, so $2800 is the same RAM as
        // $2000 and the switch cannot show in the picture; what it has to do is
        // step bit 11 of `v`, the vertical nametable select.
        assert_eq!(
            p.vram_addr() & 0x0800,
            0x0800,
            "coarse Y did not switch nametable after row 29"
        );
    }

    /// `$2005`'s X scroll lands in `t`, and `v` is reloaded from it at dot 257 of
    /// every scanline: that is what makes a whole scanline come from the
    /// nametable column the camera is looking at.
    #[test]
    fn horizontal_scroll_moves_the_nametable_column() {
        let mut p = Ppu::new();
        solid(&mut p, 1);
        p.upload_tile_for_test(0, 0, 0, 0); // tile 0: blank CHR -> backdrop
        p.upload_tile_for_test(1, 1, 0, 1); // tile 1, palette 1
        p.upload_tile_for_test(0, 2, 0, 1); // blank again
        p.write_vram_for_test(0x3F00, 0x0F);
        p.write_vram_for_test(0x3F05, 0x21);
        show(&mut p, 8, 0); // X scroll 8: coarse_x = 1, fine_x = 0
        p.run_to(&[0]);
        for x in 0..8 {
            assert_eq!(
                p.pixel(x, 0),
                0x00_4A_9F_FF,
                "coarse X did not come from t at dot 257"
            );
        }
        for x in 8..16 {
            assert_eq!(p.pixel(x, 0), BACKDROP, "column 2 did not follow column 1");
        }
    }

    /// Vblank starts at scanline 241 dot 1: the flag, the frame boundary and the
    /// NMI all land there, and a `$2002` read clears both the flag and the pending
    /// NMI. This is the interface Tasks 6 and 7 hang off. `tick` does a dot's
    /// work and then advances, so the flag published during 241.1 is readable
    /// from 241.2 -- a third of a CPU cycle, which the CPU cannot resolve.
    #[test]
    fn vblank_lands_on_scanline_241_dot_1() {
        let mut p = Ppu::new();
        p.cpu_write(0x2000, 0x80); // NMI enabled
        p.cpu_write(0x2001, 0x00); // rendering off
        for _ in 0..89342 {
            // 89342 dots: one frame with rendering off, which is the full
            // length, so this always finds 241.
            if p.frame_ready() {
                break;
            }
            p.tick();
        }
        assert_eq!(p.scanline, 241, "frame_ready must go true on scanline 241");
        assert!(p.nmi_pending(), "$2000 bit 7 must have raised the NMI");
        assert_eq!(
            p.cpu_read(0x2002) & 0x80,
            0x80,
            "the vblank flag was not set"
        );
        assert!(!p.nmi_pending(), "a $2002 read must clear the pending NMI");
        p.take_frame();
        assert_eq!(p.framebuffer().len(), 256 * 240);
        assert!(!p.frame_ready(), "taking the frame must clear frame_ready");
    }

    /// The PAL PPU is a 312-scanline part, so a frame is 106,392 dots against
    /// NTSC's 89,342, and it maps the same six-bit palette index through the
    /// 2C07 table instead of the 2C02 one.
    #[test]
    fn pal_frames_are_312_scanlines_and_use_the_2c07_palette() {
        let mut p = Ppu::for_region(Region::Pal);
        p.cpu_write(0x2001, 0x00); // rendering off: no odd-frame skip
        while !p.frame_ready() {
            p.tick();
        }
        p.take_frame();
        let mut dots = 0;
        while !p.frame_ready() {
            p.tick();
            dots += 1;
        }
        assert_eq!(dots, 312 * 341, "a PAL frame is 312 lines of 341 dots");

        p.write_vram_for_test(0x3F00, 0x11);
        assert_eq!(
            p.backdrop(),
            0x00_11_51_AA,
            "index $11 is the 2C07 colour, not the 2C02 one"
        );
    }

    /// `$2000`'s nametable select lives in `t` at bits 10-11 and nowhere else.
    /// The select values are chosen so their low two bits (00, then 11) differ
    /// from coarse_x 18's (10): a write that also lands in `t`'s bits 0-1 would
    /// reset coarse X's low two bits, and Super Mario Bros. does write `$2000`
    /// mid-frame right after it parks the split scroll, so the playfield would
    /// jump 8/16 pixels against the sprites as the camera moved.
    #[test]
    fn writing_2000_only_moves_the_nametable_bits() {
        let mut p = Ppu::new();
        p.cpu_write(0x2005, 0x91); // coarse_x = 18 (bits 0-1 = 10), fine_x = 1
        p.cpu_write(0x2005, 0xC8); // coarse_y = 25, fine_y = 0
        p.cpu_write(0x2000, 0x00); // nametable select 0: d's bits 0-1 are 00
        assert_eq!(
            p.temp_vram_addr(),
            (25 << 5) | 18,
            "$2000's low bits must not land in t's bits 0-1, which are coarse X's"
        );
        p.cpu_write(0x2000, 0x03); // nametable select 3: d's bits 0-1 are 11
        assert_eq!(
            p.temp_vram_addr(),
            (3 << 10) | (25 << 5) | 18,
            "the nametable select must reach t's bits 10-11 and stop there"
        );
    }

    /// `$2001` bit 1 hides the leftmost 8 pixels of the background. Bit 0 of
    /// the mask makes it a solid wall of tile, so the clip has to show.
    #[test]
    fn the_leftmost_eight_pixels_can_be_clipped() {
        let mut p = Ppu::new();
        solid(&mut p, 0);
        p.upload_tile_for_test(0, 0, 0, 0);
        p.write_vram_for_test(0x3F00, 0x0F);
        p.write_vram_for_test(0x3F01, 0x30);
        show(&mut p, 0, 0);
        p.cpu_write(0x2001, 0x1C); // same as $1E with the background's left
                                   // column switched off
        p.run_to(&[0]);
        for x in 0..8 {
            assert_eq!(p.pixel(x, 0), BACKDROP, "leftmost column was not clipped");
        }
        for x in 8..16 {
            assert_eq!(p.pixel(x, 0), 0x00_FF_FF_FF, "clipping ran past 8 pixels");
        }
    }

    /// `$2000-$1FFF` is pattern memory, and on this board it is CHR **ROM**:
    /// there is nothing there to write, so a `$2007` write to it is DROPPED.
    /// `vram_index` masks that range down to `$0000 + (addr & $07FF)`, so storing
    /// it would silently alias the byte onto a nametable tile -- an address range
    /// that exists because CHR was once assumed to be RAM, and no longer does.
    /// `$1FFF` aliases onto `$27FF`, the last tile of the second nametable. This
    /// game never writes below `$2000`, which is how the bug sat here unnoticed.
    #[test]
    fn a_2007_write_below_2000_is_dropped_and_never_touches_the_nametable() {
        let mut p = Ppu::new();
        solid(&mut p, 0);
        p.write_vram_for_test(0x27F0, 0x00); // nametable 1, column 31 -> x = 248
        p.write_vram_for_test(0x1FFF, 0x2A); // must NOT alias onto $27FF
        p.write_vram_for_test(0x3F00, 0x0F); // backdrop black
        p.write_vram_for_test(0x3F01, 0x30); // colour 1 = $FFFFFF
        assert_eq!(
            p.read_vram_for_test(0x27FF),
            0x00,
            "a $2007 write to $1FFF aliased onto nametable byte $27FF"
        );
        show_ctrl(&mut p, 0x01, 0, 0); // nametable 1
        p.run_to(&[0]);
        assert_eq!(
            p.pixel(248, 0),
            0x00_FF_FF_FF,
            "the dropped write must leave the rendered tile alone"
        );
    }

    /// Palette RAM is one 32-byte memory: `$3F10` is `$3F00`, and `$3F20` up is
    /// `$3F00` up again. A palette read also bypasses the `$2007` read buffer,
    /// which is what lets this read it back in one access.
    #[test]
    fn palette_ram_mirrors_3f10_and_3f20() {
        let mut p = Ppu::new();
        p.write_vram_for_test(0x3F00, 0x0F);
        p.write_vram_for_test(0x3F04, 0x01); // a different entry, to prove the
                                             // mirroring is not the whole array
        let read = |p: &mut Ppu, addr: u16| {
            p.cpu_write(0x2006, (addr >> 8) as u8);
            p.cpu_write(0x2006, addr as u8);
            p.cpu_read(0x2007)
        };
        p.write_vram_for_test(0x3F10, 0x30);
        assert_eq!(read(&mut p, 0x3F00), 0x30, "$3F10 must be $3F00");
        p.write_vram_for_test(0x3F14, 0x0A);
        assert_eq!(read(&mut p, 0x3F04), 0x0A, "$3F14 must be $3F04");
        p.write_vram_for_test(0x3F20, 0x06);
        assert_eq!(read(&mut p, 0x3F00), 0x06, "$3F20 must mirror $3F00");
        assert_eq!(read(&mut p, 0x3F04), 0x0A, "$3F20 must not touch $3F04");
    }

    /// Only `$3F10`, `$3F14`, `$3F18` and `$3F1C` fold down. The other twelve
    /// `$3F1x` addresses are sprite entries in their own right, and a decode that
    /// folds all sixteen down makes them unreachable: SMB writes its sprite
    /// colours to `$3F11`-`$3F1F`, and every one of them would land on a
    /// background entry that the playfield is already using.
    #[test]
    fn the_other_twelve_3f1x_entries_are_sprite_palette_not_mirrors() {
        let mut p = Ppu::new();
        let read = |p: &mut Ppu, addr: u16| {
            p.cpu_write(0x2006, (addr >> 8) as u8);
            p.cpu_write(0x2006, addr as u8);
            p.cpu_read(0x2007)
        };
        for addr in [0x3F10u16, 0x3F14, 0x3F18, 0x3F1C] {
            p.write_vram_for_test(addr, 0x30);
            assert_eq!(
                read(&mut p, addr - 0x10),
                0x30,
                "${addr:04X} is one of the four mirrors and must fold onto the background entry"
            );
        }
        for addr in [
            0x3F11u16, 0x3F12, 0x3F13, 0x3F15, 0x3F16, 0x3F17, 0x3F19, 0x3F1A, 0x3F1B, 0x3F1D,
            0x3F1E, 0x3F1F,
        ] {
            p.write_vram_for_test(addr, 0x16);
            assert_eq!(
                read(&mut p, addr),
                0x16,
                "${addr:04X} is a sprite entry, not a mirror"
            );
        }
        for addr in [0x3F01u16, 0x3F02, 0x3F03, 0x3F05, 0x3F06, 0x3F07] {
            assert_eq!(
                read(&mut p, addr),
                0x00,
                "${addr:04X} is a background entry and the sprite writes must not have reached it"
            );
        }
    }

    #[test]
    fn a_2007_access_advances_by_one_or_by_thirty_two() {
        let mut p = Ppu::new();
        p.cpu_write(0x2006, 0x21);
        p.cpu_write(0x2006, 0x00);
        p.cpu_write(0x2007, 0x11);
        assert_eq!(p.vram_addr(), 0x2101, "$2007 must advance v by 1");
        p.cpu_write(0x2000, 0x04); // increment by 32
        p.cpu_write(0x2007, 0x22);
        assert_eq!(p.vram_addr(), 0x2121, "$2000 bit 2 must make it 32");
    }

    /// The whole 256x240 screen, not just its first 22 pixels and 8 scanlines.
    ///
    /// Every nametable tile is distinct. The tile index carries the tile row,
    /// because the PPU addresses CHR as `tile * 16 + fine_y` and one index per
    /// column cannot tell two tile rows apart; the pattern byte carries the
    /// column and the pixel row; and the four attribute quadrants get four
    /// different colour-1 entries, so no two neighbouring tiles share a colour.
    /// A shifter that stops advancing, or a last pixel that is never written,
    /// then shows up as a wrong colour rather than as an invisible repeat.
    #[test]
    fn the_whole_screen_renders_where_the_nametable_says_it_should() {
        const COLOURS: [u8; 4] = [0x30, 0x21, 0x11, 0x01];
        let mut p = Ppu::new();
        for r in 0..30u32 {
            for c in 0..32u32 {
                // Tile rows cycle every 8 so that 30 rows fit inside the 256
                // tiles one nametable column can name. Rows 8 apart look alike,
                // which costs nothing: `j` still separates their pixel rows.
                let tile = ((r & 7) * 32 + c) as u8;
                for j in 0..8u32 {
                    p.chr_write_for_test(tile as u16 * 16 + j as u16, row_pattern(c, j));
                    p.chr_write_for_test(tile as u16 * 16 + j as u16 + 8, 0x00);
                }
                p.upload_tile_for_test(tile, c as u8, r as u8, palette_of(c, r));
            }
        }
        p.write_vram_for_test(0x3F00, 0x0F); // backdrop: $000000, distinct
                                             // from all four colour-1 entries
        for (n, val) in COLOURS.iter().enumerate() {
            p.write_vram_for_test(0x3F00 + 4 * n as u16 + 1, *val);
        }
        show(&mut p, 0, 0);
        p.run_to(&[239]); // every visible scanline, x=0..255 inclusive
        for y in 0..240u32 {
            for x in 0..256u32 {
                let bit = (row_pattern(x / 8, y % 8) >> (7 - x % 8)) & 1;
                let want = if bit == 0 {
                    BACKDROP
                } else {
                    PALETTE[COLOURS[palette_of(x / 8, y / 8) as usize] as usize]
                };
                assert_eq!(p.pixel(x, y), want, "wrong pixel at x={x} y={y}");
            }
        }
    }

    /// The nametable select in `t` (bits 11-10) belongs to `$2000` and to
    /// nothing else. NESdev's register table gives the second `$2005` write as
    /// `t: FGH..AB CDE.....`, and `t` is 15 bits with the rightmost character
    /// bit 0: `F`,`G`,`H` are bits 14-12 (fine Y), `A`,`B`,`C`,`D`,`E` are bits
    /// 9-5 (coarse Y), and the two dots are bits 11 and 10. Masking with `$73E0`
    /// therefore leaves the nametable bits alone; the `$7BE0` mask clears them
    /// too and would throw away the `$2000` write on the very next `$2005`,
    /// which is how a game switches nametables.
    #[test]
    fn a_2005_second_write_leaves_the_nametable_bits_in_t() {
        let mut p = Ppu::new();
        p.cpu_write(0x2000, 0x03); // nametable select 3 -> t bits 11-10 = 11;
                                   // d's bits 0-1 land in t's bits 0-1
        p.cpu_write(0x2005, 0x98); // coarse_x = 19 (already ends in 11), fine_x = 0
        p.cpu_write(0x2005, 0xC8); // coarse_y = 25, fine_y = 0
        assert_eq!(
            p.temp_vram_addr(),
            (3 << 10) | (25 << 5) | 19,
            "the second $2005 write must not touch t's nametable select"
        );
    }

    /// Task 5 hangs sprite-0 hit and overflow clearing off this exact dot, so it
    /// is pinned here: the vblank flag goes up at 241.1, survives dots 242-260
    /// of the pre-render line's approach, and comes back down at 261.1.
    #[test]
    fn vblank_flag_is_cleared_at_scanline_261_dot_1() {
        let mut p = Ppu::new();
        p.cpu_write(0x2000, 0x80);
        p.cpu_write(0x2001, 0x00); // rendering off: no odd-frame dot to skip
        while !p.frame_ready() {
            p.tick();
        }
        assert_eq!(
            p.status() & 0x80,
            0x80,
            "vblank must be set on scanline 241"
        );
        while p.scanline != 261 || p.dot != 1 {
            p.tick();
        }
        assert_eq!(
            p.status() & 0x80,
            0x80,
            "vblank must survive dots 242 through 260"
        );
        p.tick(); // the work of dot 1 of the pre-render line
        assert_eq!((p.scanline, p.dot), (261, 2));
        assert_eq!(p.status() & 0x80, 0x00, "vblank must be cleared at 261.1");
    }

    /// The vblank flag rises whether or not NMI is enabled; only the NMI is
    /// gated on `$2000` bit 7.
    #[test]
    fn nmi_needs_2000_bit_7() {
        for (ctrl, want) in [(0x00u8, false), (0x80, true)] {
            let mut p = Ppu::new();
            p.cpu_write(0x2000, ctrl);
            p.cpu_write(0x2001, 0x00); // rendering off: a full 89342-dot frame
            while !p.frame_ready() {
                p.tick();
            }
            assert_eq!(p.scanline, 241);
            assert_eq!(p.status() & 0x80, 0x80, "$2000 = {ctrl:#04X}: no flag");
            assert_eq!(
                p.nmi_pending(),
                want,
                "$2000 = {ctrl:#04X}: vblank raised the NMI anyway"
            );
        }
    }

    /// Coarse X is five bits that count 0-31 across a nametable, and the increment
    /// that takes 31 to 0 also toggles bit 10, the horizontal nametable select.
    /// NESdev puts those increments "at dots 328 and 336, and it will continue
    /// through the next scanline at 8, 16, 24 ... 256", so with `t` holding 31
    /// the first wrap lands at dot 328 of the pre-render line, the second at dot
    /// 248 of scanline 0, and dot 257 then puts bit 10 back from `t`.
    #[test]
    fn coarse_x_wraps_into_the_other_horizontal_nametable() {
        let mut p = Ppu::new();
        show(&mut p, 31 * 8, 0); // coarse_x = 31, fine_x = 0
                                 // Power-on is already on the pre-render line; dot 257 has just copied
                                 // the horizontal bits out of `t`.
        while p.dot != 258 {
            p.tick(); // the copy itself is at dot 257; nothing increments at 264
        }
        assert_eq!(
            p.vram_addr() & 0x001F,
            31,
            "dot 257 must copy coarse X out of t"
        );
        while p.dot != 329 {
            p.tick(); // the increment itself is at dot 328
        }
        assert_eq!(p.vram_addr() & 0x001F, 0, "coarse X 31 must wrap to 0");
        assert_eq!(
            p.vram_addr() & 0x0400,
            0x0400,
            "the wrap must toggle the horizontal nametable"
        );
        while p.scanline != 0 || p.dot != 249 {
            p.tick(); // the increment itself is at dot 248
        }
        assert_eq!(
            p.vram_addr() & 0x001F,
            0,
            "coarse X must wrap a second time"
        );
        assert_eq!(
            p.vram_addr() & 0x0400,
            0x0000,
            "the second wrap must toggle the nametable back"
        );
        while p.scanline != 0 || p.dot != 258 {
            p.tick(); // the copy itself is at dot 257
        }
        assert_eq!(
            p.vram_addr() & 0x001F,
            31,
            "dot 257 must put coarse X back from t"
        );
    }

    /// `$2001` bit 0 ANDs the palette value with `$30`, so `$01` ($000C8E) is
    /// drawn as entry `$00` ($575757).
    #[test]
    fn greyscale_ands_the_palette_value_with_30() {
        let mut p = Ppu::new();
        solid(&mut p, 0);
        p.upload_tile_for_test(0, 0, 0, 0);
        p.write_vram_for_test(0x3F00, 0x00); // backdrop looks the same either way
        p.write_vram_for_test(0x3F01, 0x01);
        show(&mut p, 0, 0);
        p.run_to(&[0]);
        assert_eq!(
            p.pixel(0, 0),
            0x00_00_0C_8E,
            "palette entry $01 must look like $01"
        );
        p.cpu_write(0x2001, 0x1F); // $1E plus bit 0
        p.run_to(&[0]);
        assert_eq!(
            p.pixel(0, 0),
            0x00_57_57_57,
            "$2001 bit 0 must AND the palette value with $30"
        );
    }

    /// The Y increment is not done on the pre-render line, whose dots 280-304
    /// reload every vertical bit from `t` regardless. A PPU that comes out of
    /// reset at (261, 0) with `v` = 0 has fine Y 0 there either way, so this
    /// reads `v` at the one dot where the increment would still be visible.
    #[test]
    fn the_pre_render_line_does_not_increment_y() {
        let mut p = Ppu::new();
        show(&mut p, 0, 0);
        while p.dot != 257 {
            p.tick();
        }
        assert_eq!(p.scanline, 261, "power-on position is the pre-render line");
        assert_eq!(
            p.vram_addr() & 0x7000,
            0,
            "dot 256 of the pre-render line must not increment fine Y"
        );
    }

    // ---------------------------------------------------------------- sprites

    /// The 8x16 pattern address, straight off NESdev's OAM memory map: an 8x16
    /// sprite whose top tile is `T` occupies 32 CONTIGUOUS bytes -- the bank is
    /// `T & 1`, the block is `(T & 0xFE) * 16`, and the bottom half is the next
    /// 16 bytes with NO bank change.
    ///
    /// This test IS the map from "PPU OAM" -> Sprite pattern tables:
    /// `$00: $0000-$001F, $01: $1000-$101F, $02: $0020-$003F, $03: $1020-$103F,
    /// ... $FE: $0FE0-$0FFF, $FF: $1FE0-$1FFF`, and the first byte of each half of
    /// each of those ranges is what the function has to return. `$02` spans
    /// `$0020-$003F` inside ONE bank, which is the fact that pins the formula:
    /// rows 8-15 are `base + 16`, not `base + $1000`.
    ///
    /// The tempting wrong formula is `(tile | 1)` for the bottom half followed by
    /// `(tile & 1) * 0x1000 + (tile & 0xFE) * 16`, which flips the pattern table
    /// instead of advancing within it. It gets tile `$00` right (both halves in
    /// bank 0 is the only case where the two rules agree) and 96 of the other 128
    /// (tile, row) combinations wrong -- SMB draws tall Mario at 8x16, so his
    /// legs would come from another sprite. Note it is *not* the 2C02's wording
    /// that forbids it: "If $2000.5 = 1, the MSB of the range result value become
    /// the LSB of the indexed tile" is a statement about the internal INDEX, and
    /// the range result is the 9-bit `scanline - Y` comparator output, which is
    /// constant across a scanline -- it cannot be a per-row bit, and it cannot
    /// select a pattern table either.
    #[test]
    fn an_8x16_pattern_address_is_32_contiguous_bytes_from_the_tile_low_bit() {
        let mut p = Ppu::new();
        p.cpu_write(0x2000, 0x20); // 8x16 sprites
        for (tile, top, bottom) in [
            (0x00u8, 0x0000u16, 0x0010u16),
            (0x01, 0x1000, 0x1010),
            (0x02, 0x0020, 0x0030),
            (0x03, 0x1020, 0x1030),
            (0xFE, 0x0FE0, 0x0FF0),
            (0xFF, 0x1FE0, 0x1FF0),
        ] {
            assert_eq!(
                p.sprite_pattern_address(tile, 0),
                top,
                "tile ${tile:02X} top half must come from ${top:04X}"
            );
            assert_eq!(
                p.sprite_pattern_address(tile, 8),
                bottom,
                "tile ${tile:02X} bottom half must come from ${bottom:04X}"
            );
        }
        // And the offset inside a half is THREE bits, never four: row 8 must not
        // land in row 0's high plane at +8.
        assert_eq!(p.sprite_pattern_address(0x02, 7), 0x0027);
        assert_eq!(p.sprite_pattern_address(0x02, 15), 0x0037);
    }

    /// 8x8 sprites still take the whole tile index and the pattern table from
    /// `$2000` bit 3, and this is the only place in the PPU that does.
    #[test]
    fn an_8x8_sprite_takes_its_pattern_table_from_2000_bit_3() {
        let mut p = Ppu::new();
        p.cpu_write(0x2000, 0x00);
        assert_eq!(p.sprite_pattern_address(0x02, 0), 0x0020);
        p.cpu_write(0x2000, 0x08);
        assert_eq!(p.sprite_pattern_address(0x02, 0), 0x1020);
        // And through the whole pipeline: the same sprite, drawn once from each
        // table, with only one of the two tables holding any pattern.
        for (ctrl, want) in [(0x00u8, BACKDROP), (0x08, 0x00_FF_FF_FF)] {
            let mut p = Ppu::new();
            two_colour_sprite(&mut p);
            // Tile $08 is the one sprite 0 uses, and only its copy in the
            // $1000 table holds any pattern.
            for row in 0..8u16 {
                p.chr_write_for_test(0x1080 + row, 0xFF);
                p.chr_write_for_test(0x1088 + row, 0x00);
            }
            p.put_sprite_for_test(0, 30, 0x08, 0x00, 64);
            show_ctrl(&mut p, ctrl, 0, 0);
            p.run_to(&[40]);
            assert_eq!(
                p.pixel(64, 32),
                want,
                "$2000 = {ctrl:#04X}: 8x8 sprite read the wrong pattern table"
            );
        }
    }

    /// An 8x16 sprite end to end: its top half is fetched from `$0020` and its
    /// bottom half from the next 16 bytes, `$0030`, it is drawn one row BELOW its
    /// OAM Y ("you must subtract 1 from the sprite's Y coordinate before writing
    /// it here"), and the bytes the brief's formula would have read stay
    /// untouched.
    #[test]
    fn an_8x16_sprite_draws_its_halves_from_the_two_pattern_tables() {
        let mut p = Ppu::new();
        two_colour_sprite(&mut p);
        // The brief's formula reads $1010 onwards for tile $02. Fill it, and
        // let it show if anything ever reads it.
        for row in 0..8u16 {
            p.chr_write_for_test(0x1010 + row, 0xFF);
            p.chr_write_for_test(0x1018 + row, 0xFF);
        }
        p.put_sprite_for_test(0, 30, 0x02, 0x00, 64);
        show_ctrl(&mut p, 0x20, 0, 0); // 8x16 sprites
        p.run_to(&[47]); // render rows 0..46
        for y in 0..30u32 {
            assert_eq!(
                p.pixel(64, y),
                BACKDROP,
                "the sprite started above its OAM Y at row {y}"
            );
        }
        for y in 31..39 {
            assert_eq!(
                p.pixel(64, y),
                0x00_FF_FF_FF,
                "row {y} is the sprite's top half and comes from $0020"
            );
        }
        for y in 39..47 {
            assert_eq!(
                p.pixel(64, y),
                0x00_4A_9F_FF,
                "row {y} is the sprite's bottom half and comes from $0030"
            );
        }
        for y in 30..47 {
            assert_eq!(
                p.pixel(63, y),
                BACKDROP,
                "the sprite is 8 pixels wide and must not reach x = 63"
            );
            assert_eq!(
                p.pixel(72, y),
                BACKDROP,
                "the sprite is 8 pixels wide and must not reach x = 72"
            );
        }
    }

    /// A vertically flipped 8x16 sprite draws its bottom half first: NESdev
    /// "in 8x16 mode, vertical flip flips each of the subtiles and also
    /// exchanges their position". Over the two halves that is row 15 - fine,
    /// and SMB uses it.
    #[test]
    fn a_vertically_flipped_8x16_sprite_swaps_its_halves() {
        let mut p = Ppu::new();
        two_colour_sprite(&mut p);
        p.put_sprite_for_test(0, 30, 0x02, 0x80, 64); // attribute bit 7
        show_ctrl(&mut p, 0x20, 0, 0);
        p.run_to(&[47]);
        for y in 31..39 {
            assert_eq!(
                p.pixel(64, y),
                0x00_4A_9F_FF,
                "row {y} should show the flipped bottom half ($0030)"
            );
        }
        for y in 39..47 {
            assert_eq!(
                p.pixel(64, y),
                0x00_FF_FF_FF,
                "row {y} should show the flipped top half ($0020)"
            );
        }
    }

    /// Horizontal flip reverses the bit order of the fetched pattern, which is
    /// "apply bit reversal to fetched object pattern table data" in the OAM
    /// attribute layout. SMB mirrors Mario this way.
    #[test]
    fn a_horizontally_flipped_sprite_is_mirrored() {
        for (attr, want_at) in [(0x00u8, 64u32), (0x40, 71)] {
            let mut p = Ppu::new();
            two_colour_sprite(&mut p);
            // Only the leftmost pixel of the top half's first row is set, so
            // mirroring moves it to the rightmost one INSIDE the same 8-pixel
            // box: the sprite still covers x = 64..71. OAM Y = 30 puts that row
            // on screen row 31.
            for row in 0..8u16 {
                p.chr_write_for_test(0x0020 + row, 0x00);
                p.chr_write_for_test(0x0028 + row, 0x00);
            }
            p.chr_write_for_test(0x0020, 0x80);
            p.put_sprite_for_test(0, 30, 0x02, attr, 64);
            show_ctrl(&mut p, 0x20, 0, 0);
            p.run_to(&[40]);
            for x in 64..72u32 {
                assert_eq!(
                    p.pixel(x, 31),
                    if x == want_at {
                        0x00_FF_FF_FF
                    } else {
                        BACKDROP
                    },
                    "attribute {attr:#04X}: the set pixel is at x={x}, expected x={want_at}"
                );
            }
            assert_eq!(
                p.pixel(63, 31),
                BACKDROP,
                "attribute {attr:#04X}: the sprite must stay inside its 8 pixels"
            );
            assert_eq!(
                p.pixel(72, 31),
                BACKDROP,
                "attribute {attr:#04X}: the sprite must stay inside its 8 pixels"
            );
        }
    }

    /// Sprite 0 hit: NESdev "PPU OAM" -> Sprite 0 hits -- "when an opaque pixel
    /// of sprite 0 overlaps an opaque pixel of the background". SMB's status
    /// bar is gated on this, and it polls `$2002` in a loop waiting for it.
    #[test]
    fn sprite_zero_hit_is_reported_when_opaque_pixels_overlap() {
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(100, 0xFF);
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert!(p.sprite0_hit());
    }

    /// x = 0..7 is suppressed only while a left-side clipping window is enabled,
    /// i.e. "if bit 2 or bit 1 of PPUMASK ($2001) is 0" (NESdev "PPU OAM" ->
    /// Sprite 0 hits). Sprite 0 is parked at x = 0 and the background is opaque
    /// only in the LEFTMOST pixel of each tile, so dot 0 is the ONE dot where
    /// sprite 0 and the background are both opaque and the mask alone decides:
    /// without the `$80` background, dots 1..7 would hit as well and the x = 0
    /// rule could not be seen at all.
    #[test]
    fn sprite_zero_hit_at_x_0_needs_the_clip_window_switched_off() {
        // $1A is $1E without bit 2: sprites are not shown at all, so the left
        // 8 pixels show the background and cannot hit.
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(0, 0x80);
        p.cpu_write(0x2001, 0x1A);
        p.run_to(&[21]);
        assert!(
            !p.sprite0_hit(),
            "a left-side clipping window must suppress the hit at x=0..7"
        );

        // $1E has both clip bits set, so there is no clipping window and x = 0 is
        // an ordinary dot: NESdev says the hit DOES happen there. SMB's status
        // bar sprite 0 sits at x = 0, so this is live behaviour.
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(0, 0x80);
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert!(
            p.sprite0_hit(),
            "with no clipping window the hit must be reported at x=0"
        );
    }

    /// x = 255: the sprite's only visible pixel is at 255. NESdev: "At x=255,
    /// for an obscure reason related to the pixel pipeline" the hit does not
    /// happen -- unconditionally, not only while clipping is on. NESdev "PPU
    /// registers" says the same. So $2001 = $1E here, with no clipping window at
    /// all.
    #[test]
    fn sprite_zero_hit_is_suppressed_at_x_255() {
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(255, 0xFF);
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert!(
            !p.sprite0_hit(),
            "sprite-0 hit must not be reported at x=255"
        );
    }

    /// A `$2002` read clears the vblank flag, the write latch and the pending
    /// NMI. It does NOT clear the sprite-0 hit: that one stays up until dot 1 of
    /// the pre-render line, which is what lets a game see the same hit for the
    /// whole rest of the frame.
    #[test]
    fn a_2002_read_does_not_clear_the_sprite_zero_hit() {
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(100, 0xFF);
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert!(p.sprite0_hit());
        let status = p.cpu_read(0x2002);
        assert_eq!(
            status & 0x40,
            0x40,
            "$2002 must report the sprite-0 hit back to the game"
        );
        assert!(
            p.sprite0_hit(),
            "reading $2002 must not clear the sprite-0 hit"
        );
    }

    /// The brief's `sprite_zero_hit_is_cleared_by_a_2002_read` is not
    /// implemented: it contradicts the two tests above and NESdev. Here is the
    /// hardware rule it should have tested -- the flag falls at dot 1 of the
    /// PRE-RENDER line, and, per the same page, it stays up through all of
    /// vblank.
    #[test]
    fn the_sprite_zero_hit_and_overflow_flags_fall_at_the_pre_render_line() {
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(100, 0xFF);
        // Nine sprites on one scanline: sprite 0 plus eight more, so the
        // overflow flag goes up as well.
        for n in 1..9 {
            p.put_sprite_for_test(n, 20, 0x02, 0x00, 8 * n as u8);
        }
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert!(p.sprite0_hit(), "the hit did not happen");
        assert!(p.sprite_overflow(), "a ninth sprite did not raise overflow");
        while p.scanline != 241 || p.dot != 2 {
            p.tick();
        }
        assert!(
            p.sprite0_hit(),
            "the sprite-0 hit must survive into vblank (it clears at 261.1)"
        );
        assert!(p.sprite_overflow());
        while p.scanline != 261 || p.dot != 2 {
            p.tick();
        }
        assert!(!p.sprite0_hit(), "sprite-0 hit must clear at 261.1");
        assert!(!p.sprite_overflow(), "overflow must clear at 261.1");
    }

    /// "At any pixel where the background or sprite pixel is transparent (2-bit
    /// colour index from the CHR pattern is %00)" -- both halves of the
    /// condition, one case each.
    #[test]
    fn sprite_zero_hit_needs_both_pixels_to_be_opaque() {
        // Background transparent everywhere: sprite 0 is opaque, still no hit.
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(100, 0x00);
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert!(
            !p.sprite0_hit(),
            "a transparent background pixel must not hit"
        );

        // Sprite 0 transparent, background opaque everywhere.
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(100, 0xFF);
        for row in 0..8u16 {
            p.chr_write_for_test(0x0020 + row, 0x00);
            p.chr_write_for_test(0x0030 + row, 0x00);
        }
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert!(
            !p.sprite0_hit(),
            "a transparent sprite 0 pixel must not hit"
        );
    }

    /// Priority decides what is DRAWN, never whether the hit happens: "Sprite
    /// 0 hit happens regardless of ... Sprite priority. Sprite 0 can still hit
    /// the background from behind." A priority-1 sprite over an opaque
    /// background shows the BACKGROUND; priority 0 shows the sprite.
    #[test]
    fn sprite_priority_decides_the_pixel_but_not_the_hit() {
        for (attr, want) in [(0x01u8, 0x00_4A_9F_FF), (0x21, 0x00_FF_FF_FF)] {
            let mut p = Ppu::new();
            p.setup_sprite0_over_background_for_test(100, 0xFF);
            p.put_sprite_for_test(0, 20, 0x02, attr, 100); // palette 1, priority
            p.cpu_write(0x2001, 0x1E);
            p.run_to(&[21]);
            assert_eq!(
                p.pixel(100, 21),
                want,
                "attribute {attr:#04X}: wrong pixel priority"
            );
            assert!(
                p.sprite0_hit(),
                "attribute {attr:#04X}: the hit does not care about priority"
            );
        }
    }

    /// A transparent sprite pixel never covers the background, and the lowest
    /// OAM index wins where sprites overlap -- "Priority between sprites is
    /// determined by their address inside OAM", and the 2C02 reference says
    /// the sprite buffer is searched in order and priority is "passed to the
    /// next serializer" when a stage is transparent.
    #[test]
    fn a_transparent_sprite_pixel_leaves_the_background_alone() {
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(100, 0xFF);
        // Sprite 0: only its leftmost pixel is opaque, the rest transparent.
        for row in 0..8u16 {
            p.chr_write_for_test(0x0020 + row, 0x80);
            p.chr_write_for_test(0x0030 + row, 0x80);
        }
        // Sprite 1, in a different palette, straight on top of the rest.
        for row in 0..8u16 {
            p.chr_write_for_test(0x0040 + row, 0xFF); // tile $04 top half
            p.chr_write_for_test(0x0048 + row, 0xFF); // colour 3
        }
        p.put_sprite_for_test(1, 20, 0x04, 0x02, 100);
        p.write_vram_for_test(0x3F1B, 0x11); // sprite palette 2 colour 3
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert_eq!(
            p.pixel(100, 21),
            0x00_4A_9F_FF,
            "sprite 0's leftmost pixel is opaque and must show through"
        );
        for x in 101..108 {
            assert_eq!(
                p.pixel(x, 21),
                PALETTE[0x11],
                "sprite 0 is transparent at x={x}, so sprite 1 must show, not the background"
            );
        }
        assert!(p.sprite0_hit(), "sprite 0 still hit at x = 100");
    }

    /// A priority-1 sprite behind an opaque background pixel hands priority on
    /// to the NEXT sprite, not to the background: "a high priority sprite in
    /// the background takes precedence over a lower priority sprite even in
    /// the foreground" (NESdev "PPU rendering", priority notes).
    #[test]
    fn a_sprite_behind_the_background_lets_the_next_sprite_through() {
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(100, 0xFF);
        p.put_sprite_for_test(0, 20, 0x02, 0x21, 100); // sprite 0, priority 1
        p.put_sprite_for_test(1, 20, 0x04, 0x02, 100); // sprite 1, in front
        for row in 0..8u16 {
            p.chr_write_for_test(0x0040 + row, 0xFF); // tile $04
            p.chr_write_for_test(0x0048 + row, 0xFF); // colour 3
        }
        p.write_vram_for_test(0x3F1B, 0x11); // sprite palette 2 colour 3
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert_eq!(
            p.pixel(100, 21),
            PALETTE[0x11],
            "sprite 1 must beat an opaque background pixel over sprite 0"
        );
    }

    /// `$2001` bit 2 clips the leftmost eight pixels of the sprite layer, and
    /// NESdev is explicit that no sprite-0 hit can happen at x=0..7 while that
    /// window (or the background's) is enabled.
    #[test]
    fn the_leftmost_eight_pixels_can_be_clipped_for_sprites_too() {
        // Sprite 0 at x=0, background opaque everywhere. With bit 2 off the
        // sprite layer is clipped for x=0..7, so those eight dots show the
        // BACKGROUND rather than the sprite -- and NESdev is explicit that
        // "sprite 0 hit does not happen at x=0 to x=7 if the left-side
        // clipping window is enabled".
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(0, 0xFF);
        p.cpu_write(0x2001, 0x1A); // $1E without bit 2
        p.run_to(&[21]);
        for x in 0..8u32 {
            assert_eq!(
                p.pixel(x, 21),
                0x00_FF_FF_FF,
                "x={x} must show the background, not the clipped sprite"
            );
        }
        assert!(
            !p.sprite0_hit(),
            "a clipped leftmost column cannot produce a sprite-0 hit"
        );
        // $1C is $1E without bit 1 (`b`): only the BACKGROUND is clipped, and
        // sprites keep bit 2 (`M`), so the sprite is drawn over the backdrop at
        // x=0..7. The sprite-0 hit still cannot happen there, because NESdev
        // puts that at x=0..7 whenever EITHER clip window is enabled.
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(0, 0xFF);
        p.cpu_write(0x2001, 0x1C);
        p.run_to(&[21]);
        for x in 0..8u32 {
            assert_eq!(
                p.pixel(x, 21),
                0x00_4A_9F_FF,
                "x={x}: sprites are not clipped, so the sprite wins over the backdrop"
            );
        }
        assert!(
            !p.sprite0_hit(),
            "the background's own clip window suppresses the hit at x=0..7 too"
        );
        // Bit 2 back on: the sprite is drawn again, at x = 0..7. (The hit
        // test above already pins x=0 alone; with the background opaque all
        // the way across, dots 1..7 legitimately hit.)
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(0, 0xFF);
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        for x in 0..8u32 {
            assert_eq!(
                p.pixel(x, 21),
                0x00_4A_9F_FF,
                "with no clipping the sprite must be drawn at x={x}"
            );
        }
    }

    /// Eight sprites on a scanline are all drawn and raise no overflow; a
    /// ninth raises it ("when found object data after this is discarded, and a
    /// flag (bit 5 of $2002) is raised").
    #[test]
    fn the_ninth_sprite_on_a_scanline_raises_overflow() {
        for (count, want_overflow) in [(8usize, false), (9, true)] {
            let mut p = Ppu::new();
            p.setup_sprite0_over_background_for_test(0, 0xFF);
            for n in 0..count {
                p.put_sprite_for_test(n, 20, 0x02, 0x01, 16 * n as u8);
            }
            p.cpu_write(0x2001, 0x1E);
            p.run_to(&[21]);
            assert_eq!(
                p.sprite_overflow(),
                want_overflow,
                "{count} sprites on one scanline: wrong overflow flag"
            );
            // All eight that fit are drawn: sprite n covers x = 16n.
            for n in 0..8usize {
                assert_eq!(
                    p.pixel(16 * n as u32, 21),
                    0x00_4A_9F_FF,
                    "sprite {n} was dropped from the scanline"
                );
            }
        }
    }

    /// Secondary OAM fills COMPACTLY: "the OAM data for each sprite found to be
    /// within range is copied into the secondary OAM" (NESdev "PPU sprite
    /// evaluation"). The eight-sprite cap counts sprites FOUND, not their OAM
    /// index, so eight in-range sprites living at OAM 20..27 fill all eight
    /// slots and raise no overflow, whatever sprite 0 is doing.
    ///
    /// Sprite 0 is parked out of range here, which also pins the reason unit 0
    /// cannot be assumed to be sprite 0: with secondary OAM compacted, unit 0 is
    /// the first sprite FOUND, so a PPU that recognises sprite 0 by unit index
    /// would raise a sprite-0 hit out of OAM entry 20. Hence the last assertion.
    #[test]
    fn eight_sprites_found_anywhere_in_oam_fill_secondary_oam() {
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(0, 0xFF);
        p.put_sprite_for_test(0, 100, 0x02, 0x01, 0); // sprite 0: out of range at row 21
        for k in 0..8usize {
            p.put_sprite_for_test(20 + k, 20, 0x02, 0x01, 16 * (k + 1) as u8);
        }
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[21]);
        assert!(
            !p.sprite_overflow(),
            "eight in-range sprites must not raise overflow"
        );
        for k in 0..8usize {
            assert_eq!(
                p.pixel(16 * (k + 1) as u32, 21),
                0x00_4A_9F_FF,
                "sprite at OAM {} was dropped: secondary OAM does not fill by OAM index",
                20 + k
            );
        }
        assert!(
            !p.sprite0_hit(),
            "sprite 0 is out of range, so no hit may come out of OAM entry 20"
        );
    }

    /// The in-range comparator is 9 bits wide, so `scanline - Y` WRAPS. That is
    /// what makes writing $EF-$FF hide a sprite: on every visible scanline those
    /// Y values land off the top of the screen (NESdev "PPU OAM": "Hide a sprite
    /// by moving it down offscreen, by writing any values between #$EF-#$FF
    /// here"). Without the wrap, $FF would be the largest Y and hide nothing.
    ///
    /// This is also the test that keeps the pre-render line's no-evaluation rule
    /// honest. Evaluation prepares the NEXT scanline, so at the pre-render line
    /// the comparator sees row 262; row 262 minus Y = $FF is 7, which IS in
    /// range, so with the power-on all-$FF OAM a PPU that evaluated there would
    /// raise overflow every single frame. Hardware does not evaluate on the
    /// pre-render line at all, and this pins that it does not here either.
    #[test]
    fn a_sprite_at_oam_y_ff_is_hidden_and_raises_no_overflow() {
        let mut p = Ppu::new();
        p.setup_sprite0_over_background_for_test(100, 0xFF);
        p.put_sprite_for_test(0, 0xFF, 0x02, 0x01, 100); // $FF: the hide value
        p.put_sprite_for_test(1, 0xEF, 0x02, 0x01, 110); // $EF: the other end of it
        p.cpu_write(0x2001, 0x1E);
        p.run_to(&[239]);
        for y in 21..37u32 {
            assert_eq!(
                p.pixel(100, y),
                0x00_FF_FF_FF,
                "row {y}: a sprite at OAM Y = $FF must be hidden, not drawn"
            );
        }
        // $EF is the LOWEST hiding value, so it is the one that can come closest
        // to the screen: with a 16-row sprite starting at row 240 its only
        // reachable row is 239, and even that must stay background. A comparator
        // off by one at the bottom (`0..=height`) shows the sprite here.
        assert_eq!(
            p.pixel(110, 239),
            0x00_FF_FF_FF,
            "a sprite at OAM Y = $EF must be hidden on the last visible row too"
        );
        assert!(
            !p.sprite_overflow(),
            "a hidden sprite must not count towards the eight-sprite cap"
        );
        assert!(!p.sprite0_hit(), "a hidden sprite 0 cannot hit");
    }
}
