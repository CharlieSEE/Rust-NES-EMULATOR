//! The two console timings this emulator knows. The region comes from the
//! cartridge header; everything a clock depends on lives here so the PPU, APU
//! and player pace agree.

/// RP2C02/RP2A03 and RP2C07/RP2A07.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Region {
    #[default]
    Ntsc,
    Pal,
}

impl Region {
    /// The CPU clock; the APU runs at half this.
    pub fn cpu_hz(self) -> u32 {
        match self {
            Region::Ntsc => 1_789_773,
            Region::Pal => 1_662_607,
        }
    }

    /// Scanlines in one frame, pre-render line included.
    pub fn scanlines(self) -> u32 {
        match self {
            Region::Ntsc => 262,
            Region::Pal => 312,
        }
    }
}
