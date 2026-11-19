//! Golden-frame regression tests: the framebuffer at chosen frames is hashed
//! and compared with values checked into this file, so a rendering change
//! fails `cargo test` instead of being spotted by eye.
//!
//! The bundled ROMs always run: one blargg PPU result screen, nestest's full
//! text screen, and a hand-built fixture with background, sprites and scrolling.
//! The real-game entries need ROMs that are not committed, so they skip on a
//! clean checkout and in CI; a local `cargo test` still catches mapper and PPU
//! regressions in them.
//!
//! A mismatch prints the actual hash. When the new picture is intended, paste
//! it into `GOLDENS`.

use crate::apu_rom_test::Machine;
use crate::rom::test_rom;
use crate::{parse_script, Play};

/// One ROM and the frames whose pictures are pinned.
struct Golden {
    /// Repository-relative ROM path; a `.zip` is read the way the emulator
    /// reads it. A path under `test_roms/` is committed and must be present.
    rom: &'static str,
    /// Buttons held each frame, in `--play` syntax; `None` leaves the pad idle.
    script: Option<&'static str>,
    /// One-based frames to hash, in increasing order, with the FNV-1a of the
    /// framebuffer at each.
    frames: &'static [(u32, u64)],
}

const GOLDENS: &[Golden] = &[
    Golden {
        rom: "test_roms/blargg_ppu_tests/palette_ram.nes",
        script: None,
        frames: &[(60, 0x5d76_5893_1a2b_0cdc), (300, 0x5d76_5893_1a2b_0cdc)],
    },
    // Mapper 0 fixture with a scrolled background, four tiles and two sprites,
    // so CI covers more than text screens. Built by `make_graphics.py`.
    Golden {
        rom: "test_roms/golden/graphics.nes",
        script: None,
        frames: &[(60, 0xc5b1_eb9f_dc80_3732), (300, 0xc5b1_eb9f_dc80_3732)],
    },
    // A full screen of text, so a tile or palette change moves the hash.
    Golden {
        rom: "test_roms/nestest/nestest.nes",
        script: None,
        frames: &[(60, 0x74ce_7b71_afa2_b4b2), (300, 0x74ce_7b71_afa2_b4b2)],
    },
    // Mapper 0, the scrolling playfield and sprite path.
    Golden {
        rom: "NES_ROMS/Super Mario Bros. + Duck Hunt (USA).zip",
        script: Some("START30,WAIT60,RIGHT120,A30,WAIT60"),
        frames: &[
            (60, 0x047f_85cb_0591_97c4),
            (180, 0x0ad4_e1a2_c8e5_3316),
            (300, 0x5182_847f_cc9a_8fc1),
        ],
    },
    // Mapper 1, battery-backed board and a static title screen.
    Golden {
        rom: "NES_ROMS/Legend of Zelda, The (USA).zip",
        script: Some("START30,WAIT60,DOWN30,WAIT60"),
        frames: &[
            (60, 0x9bdd_2647_0a63_4c8c),
            (120, 0x2f85_7d54_2ea0_f639),
            (180, 0x1e80_245c_1e5b_0bde),
        ],
    },
    // Mapper 4, MMC3 CHR banking and split-screen IRQ timing.
    Golden {
        rom: "NES_ROMS/Mega Man 2 (USA).zip",
        script: Some("START30,WAIT60,RIGHT60,A30,WAIT60"),
        frames: &[
            (60, 0x1fb2_91c0_0173_a3f9),
            (120, 0x1fb2_91c0_0173_a3f9),
            (240, 0x58db_ab8b_3111_8e59),
        ],
    },
];

/// FNV-1a, 64-bit. A hash is small enough to check in and catches any pixel
/// change; a cryptographic digest would add a dependency for nothing.
fn fnv1a(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// One framebuffer pixel is `0x00RRGGBB`; its four bytes are hashed as they
/// sit in memory so a change in any colour channel moves the hash.
fn frame_hash(frame: &[u32]) -> u64 {
    fnv1a(frame.iter().flat_map(|pixel| pixel.to_le_bytes()))
}

#[test]
fn fnv1a_matches_the_reference_vector() {
    assert_eq!(fnv1a(b"hello".iter().copied()), 0xa430_d846_80aa_bd0b);
}

#[test]
fn golden_frames_are_unchanged() {
    let mut failures = Vec::new();
    for golden in GOLDENS {
        let Some(bytes) = test_rom(golden.rom) else {
            assert!(
                !golden.rom.starts_with("test_roms/"),
                "{} ships with the test suite",
                golden.rom
            );
            continue;
        };
        assert!(
            golden.frames.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "{}: golden frames must be strictly increasing",
            golden.rom
        );
        let (last, _) = *golden
            .frames
            .last()
            .expect("a golden entry names at least one frame");
        let mut play = golden.script.map(|script| {
            Play::new(
                parse_script(script)
                    .unwrap_or_else(|e| panic!("{}: golden script: {e}", golden.rom)),
            )
        });
        let mut m = Machine::boot(&bytes);
        for frame in 1..=last {
            let mask = play
                .as_mut()
                .and_then(Play::mask_for_next_frame)
                .unwrap_or(0);
            m.bus.controllers[0].set_buttons(mask);
            m.frame();
            let Some(&(_, want)) = golden.frames.iter().find(|pair| pair.0 == frame) else {
                continue;
            };
            let actual = frame_hash(m.bus.ppu.framebuffer());
            if actual != want {
                failures.push(format!(
                    "{} frame {frame}: expected {:#018x}, got {actual:#018x}",
                    golden.rom, want
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "rendering changed:\n{}\n\
         If the new picture is intended, put the actual hashes into GOLDENS.",
        failures.join("\n")
    );
}
