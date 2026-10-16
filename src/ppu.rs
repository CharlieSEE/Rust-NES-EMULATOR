//! PPU: registers, VRAM, palette RAM, the 8 KiB of CHR RAM this cartridge
//! carries, and the dot-by-dot background pipeline.
//!
//! References, all fetched 2026-10-16:
//! - NESdev "PPU registers" -- register bits, the v/t/x/w internal registers,
//!   the `$2002` read side effects, and the `$2007` read buffer.
//! - NESdev "PPU scrolling" -- the exact coarse-X increment and Y increment
//!   pseudocode, the dot-257 horizontal copy and the dots 280-304 vertical copy.
//! - NESdev "PPU rendering" -- the fetch timeline and the shift-register reload.
//! - NESdev "PPU frame timing" -- vblank at 241.1, the odd-frame skipped dot.
//! - NESdev "PPU palettes" -> `File:2C02G_U_wiki.pal` -- the 2C02 master palette.

// TEMPORARY: nothing constructs a `Ppu` until the CPU-bus task does, so without
// this `cargo build` emits a dead_code warning per entry point. Removing it is
// one line of follow-up there; module-wide, it would also mute later additions.
#![allow(dead_code)]

const DOTS_PER_LINE: u32 = 341;
const VISIBLE_LINES: u32 = 240;
const VBLANK_FIRST: u32 = 241;
const PRE_RENDER: u32 = 261;

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
    0x00_2F_CF_64,
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

/// The colour the PPU emits where both the background and the sprites are
/// transparent. On a stock NES the EXT pins are grounded, so this is whatever
/// palette RAM entry `$3F00` holds; the shift register model only ever asks for
/// colour 0 with no tile, and a colour-0 pattern bit pair is transparent for
/// the same reason, so this is the one colour that is never looked up per pixel.
const BACKDROP: u32 = 0x00_00_00_00;

/// Nametable RAM: 2 KiB of physical RAM. The board wires nametables VERTICALLY,
/// so `$2000` and `$2800` are the same 1 KiB and `$2400` and `$2C00` are the
/// other one; the offset inside the 2 KiB is `addr & $07FF`.
fn vram_index(addr: u16) -> usize {
    (addr & 0x0FFF) as usize & 0x07FF
}

/// Palette RAM is 32 entries: `$3F10`/`$3F14`/`$3F18`/`$3F1C` sit behind
/// `$3F00`/`$3F04`/`$3F08`/`$3F0C` and `$3F20` upwards mirrors `$3F00`.
fn palette_index(addr: u16) -> usize {
    let i = (addr & 0x1F) as usize;
    if i >= 0x20 {
        i - 0x20
    } else {
        i & 0x0F
    }
}

pub struct Ppu {
    vram: [u8; 2048],
    palette: [u8; 32],
    /// The cartridge carries 8 KiB of CHR **RAM** that the game uploads tile
    /// data into at boot, and CHR RAM is PPU-bus memory the PPU fetches from
    /// every dot, so the PPU owns it and `Cartridge` holds PRG only.
    chr: [u8; 8192],
    oam: [u8; 256],
    secondary_oam: [u8; 32],
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

impl Ppu {
    /// Power-on position. Hardware comes out of reset "at the top of the
    /// picture" with its shift registers still cold, which would leave the
    /// first 16 pixels of the first scanline unwritten. Starting on the
    /// pre-render line instead costs one blanked scanline at boot and makes
    /// every frame this PPU renders complete.
    pub fn new() -> Ppu {
        Ppu {
            vram: [0; 2048],
            palette: [0; 32],
            chr: [0; 8192],
            oam: [0; 256],
            secondary_oam: [0; 32],
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            v: 0,
            t: 0,
            fine_x: 0,
            write_latch: false,
            read_buffer: 0,
            scanline: PRE_RENDER,
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
        }
    }

    /// Advance the PPU by one dot. Three of these are one CPU cycle.
    pub fn tick(&mut self) {
        // "The VBlank flag of the PPU is set at tick 1 (the second tick) of
        // scanline 241" and cleared 6820 dots later, at 261.1.
        if self.scanline == VBLANK_FIRST && self.dot == 1 {
            self.status |= 0x80;
            self.frame_done = true;
            if self.ctrl & 0x80 != 0 {
                self.nmi = true;
            }
        }
        if self.scanline == PRE_RENDER && self.dot == 1 {
            self.status &= !0x80;
        }

        // The background pipeline runs on the visible scanlines and on the
        // pre-render line. It also keeps running when only sprites are enabled:
        // a disabled component is transparent, not absent, so the other one's
        // fetches and scroll updates are unaffected (NESdev "PPUMASK").
        if self.rendering_enabled()
            && (self.scanline < VISIBLE_LINES || self.scanline == PRE_RENDER)
        {
            self.background_dot();
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
            if self.scanline == PRE_RENDER && (280..=304).contains(&self.dot) {
                self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
            }
        }

        self.dot += 1;
        if self.dot == DOTS_PER_LINE {
            // "On odd frames the cycle at the end of the pre-render scanline is
            // skipped ... by jumping directly from (339,261) to (0,0)". Only
            // with rendering enabled; with it disabled every frame is 89342
            // dots long.
            if self.scanline == PRE_RENDER && self.odd_frame && self.rendering_enabled() {
                self.dot = 0;
                self.scanline = 0;
                self.odd_frame = !self.odd_frame;
            } else {
                self.dot = 0;
                self.scanline += 1;
                if self.scanline > PRE_RENDER {
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
                1 => self.fetch.tile = self.vram[vram_index(0x2000 | (self.v & 0x0FFF))],
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
                    let shift = (((self.v >> 6) & 1) << 2) | (((self.v >> 1) & 1) << 1);
                    self.fetch.attr = (self.vram[vram_index(addr)] >> shift) & 3;
                }
                5 => self.fetch.lo = self.pattern_byte(0),
                7 => {
                    self.fetch.hi = self.pattern_byte(8);
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
        let reload =
            dot % 8 == 0 && (dot == 0 || (8..=256).contains(&dot) || (321..=336).contains(&dot));
        if reload {
            self.bg_lo = ((self.prev.lo as u16) << 8) | self.cur.lo as u16;
            self.bg_hi = ((self.prev.hi as u16) << 8) | self.cur.hi as u16;
            self.attr_prev = self.prev.attr;
            self.attr_cur = self.cur.attr;
        }
        if dot % 8 == 0 && ((8..=256).contains(&dot) || (321..=336).contains(&dot)) {
            self.increment_coarse_x();
        }
        if dot < 256 && self.scanline < VISIBLE_LINES {
            self.draw_pixel(dot);
        }
        if dot <= 256 {
            self.bg_lo <<= 1;
            self.bg_hi <<= 1;
        }
    }

    /// Pixel `dot` of the visible scanline being rendered. Sprite compositing
    /// arrives in the sprite task; until then this is the whole picture.
    fn draw_pixel(&mut self, x: u32) {
        let y = self.scanline as usize * 256 + x as usize;
        // Bit 3 off, or the leftmost 8 pixels clipped by bit 1, means backdrop.
        if self.mask & 0x08 == 0 || (x < 8 && self.mask & 0x02 == 0) {
            self.framebuffer[y] = self.backdrop();
            return;
        }
        let tap = 15 - self.fine_x as u16;
        let value = (((self.bg_hi >> tap) & 1) << 1) | ((self.bg_lo >> tap) & 1);
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
    }

    /// A background pattern byte for the tile named by the nametable byte just
    /// fetched: `$2000` bit 4 picks the table, bits 12-14 of `v` are the row
    /// inside the tile, and the low plane lives 8 bytes below the high one.
    fn pattern_byte(&self, plane: u16) -> u8 {
        let base = if self.ctrl & 0x10 != 0 {
            0x1000
        } else {
            0x0000
        };
        let addr = base + self.fetch.tile as u16 * 16 + ((self.v >> 12) & 7) + plane;
        self.chr[addr as usize & 0x1FFF]
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
        PALETTE[value as usize]
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

    pub fn framebuffer(&self) -> &[u32] {
        &self.framebuffer
    }

    pub fn take_frame(&mut self) -> Vec<u32> {
        self.frame_done = false;
        self.framebuffer.clone()
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
                // "The referenced 6-bit palette data is returned immediately
                // instead of going to the internal read buffer", and the same
                // access performs the shadowed nametable read underneath it.
                if addr >= 0x3F00 {
                    let value = self.palette[palette_index(addr)] & 0x3F;
                    self.read_buffer = self.vram[vram_index(addr)];
                    self.increment_v();
                    return value;
                }
                let value = self.read_buffer;
                self.read_buffer = self.vram[vram_index(addr)];
                self.increment_v();
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
                self.t = (self.t & !0x0C03) | (val as u16 & 0x03) | ((val as u16 & 0x03) << 10);
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
                } else {
                    // Bits 14-15 of the written value are ignored and bit 14 of
                    // `t` is forced to 0.
                    self.t = (self.t & 0x00FF) | ((val as u16 & 0x3F) << 8);
                }
                self.write_latch = !self.write_latch;
            }
            7 => {
                let addr = self.v & 0x3FFF;
                if addr >= 0x3F00 {
                    self.palette[palette_index(addr)] = val & 0x3F;
                } else {
                    self.vram[vram_index(addr)] = val;
                }
                self.increment_v();
            }
            // $2002 is read-only: "Reading from $2002 clears the vblank flag
            // ... Writes here have no effect." (2C02 technical reference.)
            _ => {}
        }
    }

    /// `$2007` advances `v` by 1, or by 32 when `$2000` bit 2 is set.
    fn increment_v(&mut self) {
        let step = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
        self.v = self.v.wrapping_add(step) & 0x7FFF;
    }
}

impl Default for Ppu {
    fn default() -> Self {
        Ppu::new()
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

    /// Write one byte of CHR RAM. The game does this through the PPU bus too,
    /// but the bus does not exist yet, so the tests go in through this door.
    fn chr_write_for_test(&mut self, addr: u16, val: u8) {
        self.chr[(addr as usize) & 0x1FFF] = val;
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
        self.read_buffer = self.vram[vram_index(addr)];
        self.read_buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Turn rendering on at a scroll position. Call this LAST: `$2006`/`$2007`
    /// leave `v` and `t` pointing wherever the last write ended, and the scroll
    /// is copied out of `t` at dot 257 and at dots 280-304, so a game sets
    /// `$2000` and `$2005` again after its VRAM writes.
    fn show(p: &mut Ppu, fine_x: u8, y: u8) {
        p.cpu_write(0x2000, 0);
        p.cpu_write(0x2005, fine_x);
        p.cpu_write(0x2005, y);
        p.cpu_write(0x2001, 0x1E);
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
        assert_eq!(p.take_frame().len(), 256 * 240);
        assert!(!p.frame_ready(), "taking the frame must clear frame_ready");
    }

    /// `$2000`'s nametable select lives in `t` at bits 10-11 and 0-1 and nothing
    /// else in `t` moves: a game that changes nametables mid-frame must not lose
    /// the scroll it parked there.
    #[test]
    fn writing_2000_only_moves_the_nametable_bits() {
        let mut p = Ppu::new();
        p.cpu_write(0x2005, 0x90); // coarse_x = 18 (bits 0-1 = 10), fine_x = 0
        p.cpu_write(0x2005, 0xC8); // coarse_y = 25, fine_y = 0
        p.cpu_write(0x2000, 0x02); // nametable select 2: d's bits 0-1 also land
                                   // in t's bits 0-1, which are coarse_x's low
                                   // two bits -- 18 already ends in 10
        assert_eq!(
            p.temp_vram_addr(),
            (2 << 10) | (25 << 5) | 18,
            "$2000 must not disturb the scroll in t"
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
}
