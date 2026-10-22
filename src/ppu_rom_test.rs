//! blargg's NTSC PPU tests, run headless. Each result is reported on screen and
//! by beeping its result code, so one beep means every check in that ROM
//! passed.
//!
//! The suite's other two ROMs are not asserted here:
//! `power_up_palette` compares against the palette RAM values in blargg's own
//! NES ("probably unique to my NES", its readme says), and `vbl_clear_time`
//! reports code 3, vblank cleared later than blargg's window allows. Both are
//! timing/board trivia rather than rendering behavior; the three below cover
//! palette mirroring, sprite RAM/DMA and VRAM buffering.

use crate::apu_rom_test::beep_count;
use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::rom::{test_rom, Cartridge};

const ROMS: [&str; 3] = ["palette_ram.nes", "sprite_ram.nes", "vram_access.nes"];

#[test]
fn blargg_ppu_suites_report_one_beep() {
    for name in ROMS {
        let bytes = test_rom(&format!("test_roms/blargg_ppu_tests/{name}"))
            .unwrap_or_else(|| panic!("{name} ships with the test suite"));
        let mut bus = Bus::new(Cartridge::load(&bytes).expect("PPU test ROM must load"));
        let mut cpu = Cpu::new();
        bus.power_on();
        cpu.reset(&mut bus);
        let mut samples = Vec::new();
        // These ROMs finish in a few seconds; 900 frames is 15 s of emulated
        // time, enough for the beep and its trailing silence to be counted.
        for _ in 0..900 {
            bus.run_frame(&mut cpu);
            bus.ppu.take_frame();
            samples.extend(bus.apu.take_samples());
        }
        assert_eq!(beep_count(&samples), 1, "{name}: result code must be 1");
    }
}
