//! blargg's APU test ROMs, run headless: the measurement the APU work is
//! accepted on.
//!
//! Two suites, two reporting channels:
//!
//! * `test_roms/apu_2005` reports its result code by **beeping** that many
//!   times (readme.txt: "Each ROM runs several tests and reports a result code
//!   on screen and by beeping a number of times. A result code of 1 always
//!   indicates that all tests were passed"). A beep is 20 ms or more of sound;
//!   100 ms of silence separates two of them.
//! * `test_roms/apu_reset` writes its status to `$6000` (`$80` running, `$81`
//!   needs the RESET button, `$00-$7F` finished with that code) and its
//!   printed report to `$6004` on, with `$DE $B0 $G1` at `$6001-$6003`
//!   identifying it as one of these ROMs. Reading the code needs no audio
//!   decoding, and it exercises the cartridge's `$6000-$7FFF` PRG RAM window.
//!
//! The ROM files are gitignored and were never committed, so every test here
//! skips cleanly when they are missing.

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::rom::Cartridge;

/// The ROM, or `None` when the gitignored file is not in the working tree.
/// Resolved from the manifest directory, so the suites read this checkout's
/// `test_roms/` whatever directory the test binary was started in.
fn rom_bytes(rel: &str) -> Option<Vec<u8>> {
    ::std::fs::read(format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel)).ok()
}

/// A powered-on NES with the two pieces the tests drive.
pub struct Machine {
    pub bus: Bus,
    pub cpu: Cpu,
}

impl Machine {
    fn boot(bytes: &[u8]) -> Machine {
        let mut bus = Bus::new(Cartridge::load(bytes).expect("test ROM must load"));
        let mut cpu = Cpu::new();
        bus.power_on();
        cpu.reset(&mut bus);
        Machine { bus, cpu }
    }

    /// One whole frame, with the frame-ready flag drained so the next call
    /// runs the next frame.
    fn frame(&mut self) {
        self.bus.run_frame(&mut self.cpu);
        let _ = self.bus.ppu.take_frame();
    }

    fn read(&mut self, addr: u16) -> u8 {
        self.bus.cpu_read(addr)
    }

    /// The RESET button: RAM survives, the CPU re-vectors, and the APU
    /// re-applies its power-on rules with the last `$4017` value.
    fn reset(&mut self) {
        self.bus.reset();
        self.cpu.reset(&mut self.bus);
    }
}

/// SMB leaves `$4017` alone for its first five seconds, so the APU stays in
/// the mode 0 it powered up in: the frame IRQ flag comes up half a frame in
/// and *stays* up. That is hardware's power-on state (`$4017 = $00`, interrupt
/// enabled) and it is harmless while the CPU keeps `I` set, which is what SMB
/// does -- an interrupt the CPU actually took would move the picture, which is
/// what the pre-plan baseline comparison in the APU plan's Task 13 checks.
///
/// So what this measures is that the flag rises once, from the frame counter
/// alone, and that nothing in the game's first 60 frames clears or inhibits it.
#[test]
fn smb_runs_sixty_frames_with_the_frame_irq_flag_up() {
    let Some(bytes) = rom_bytes("Super Mario Bros. + Duck Hunt (USA).nes") else {
        return;
    };
    let mut m = Machine::boot(&bytes);
    let mut up_at = None;
    for frame in 0..60 {
        m.frame();
        if m.bus.apu.irq_pending() {
            up_at.get_or_insert(frame);
        } else if let Some(first) = up_at {
            panic!(
                "the frame IRQ flag went down again at frame {frame} (up since {first}): \
                 this cart does not write $4017 this early"
            );
        }
    }
    let first = up_at.expect("the mode 0 flag never came up");
    assert!(first <= 1, "the mode 0 flag came up only at frame {first}");
}

/// blargg's audible result code: he beeps the code, so a passing 2005 ROM
/// (code 1) beeps exactly once.
fn beep_count(samples: &[i16]) -> usize {
    /// 10 ms of stereo samples at 48 kHz. `Machine` always mixes at 48 kHz.
    const BLOCK: usize = 480 * 2;
    /// Out of `i16`'s 32767: a pulse channel at volume 15 alone peaks near 4900.
    const THRESHOLD: i16 = 500;
    /// 20 ms of sound is a beep, 100 ms of silence separates two.
    const MIN_LOUD: usize = 2;
    const MIN_QUIET: usize = 10;

    let mut count = 0;
    let mut run = 0;
    let mut gap = MIN_QUIET;
    for block in samples.chunks(BLOCK) {
        if block.iter().any(|s| s.abs() > THRESHOLD) {
            run += 1;
        } else {
            if run >= MIN_LOUD && gap >= MIN_QUIET {
                count += 1;
            }
            if run >= MIN_LOUD {
                gap = 0;
            }
            run = 0;
            gap = gap.saturating_add(1);
        }
    }
    if run >= MIN_LOUD && gap >= MIN_QUIET {
        count += 1;
    }
    count
}

const APU_2005_ROMS: [&str; 11] = [
    "01.len_ctr.nes",
    "02.len_table.nes",
    "03.irq_flag.nes",
    "04.clock_jitter.nes",
    "05.len_timing_mode0.nes",
    "06.len_timing_mode1.nes",
    "07.irq_flag_timing.nes",
    "08.irq_timing.nes",
    "09.reset_timing.nes",
    "10.len_halt_timing.nes",
    "11.len_reload_timing.nes",
];

/// Every apu_reset ROM and the code it reports. Every check passes (code 0)
/// with one exception, measured and explained rather than waved through:
///
/// * `works_immediately` wants `$4015` to read `$1F` -- the four length
///   counters plus the DMC's "sample playing" bit -- straight after
///   `$4015 = $1F` loads a sample length. The DMC is cut (`apu.rs`), so that
///   bit reads 0 and the ROM reports its code 2, "At power, writes should work
///   immediately". Reading `$4015` back as `$0F` there is the cut showing, not
///   a timing bug.
const APU_RESET_ROMS: [(&str, u8); 6] = [
    ("4015_cleared.nes", 0),
    ("4017_timing.nes", 0),
    ("4017_written.nes", 0),
    ("irq_flag_cleared.nes", 0),
    ("len_ctrs_enabled.nes", 0),
    ("works_immediately.nes", 2),
];

/// Ten emulated seconds each: the suite's offsets run to tens of thousands of
/// CPU cycles and several ROMs wait many frames between checks.
const APU_2005_FRAMES: u32 = 600;

/// Twenty emulated seconds, which is room for a ROM to run its power-on
/// checks, ask for the RESET button and run its reset checks.
const APU_RESET_FRAMES: u32 = 1200;

/// Every 2005 ROM beeps its result code. A pass is exactly one beep.
#[test]
fn the_2005_frame_counter_suite_reports_one_beep() {
    let roms: Vec<(&str, Vec<u8>)> = APU_2005_ROMS
        .iter()
        .filter_map(|name| {
            rom_bytes(&format!("test_roms/apu_2005/{name}")).map(|bytes| (*name, bytes))
        })
        .collect();
    if roms.is_empty() {
        eprintln!("skipped: no ROMs in test_roms/apu_2005");
        return;
    }
    // Half a suite means a broken environment, not a clean skip: green here has
    // to mean all eleven ROMs reported.
    assert_eq!(
        roms.len(),
        APU_2005_ROMS.len(),
        "test_roms/apu_2005 is incomplete: {} of {} ROMs present",
        roms.len(),
        APU_2005_ROMS.len()
    );

    let mut failures = Vec::new();
    for (name, bytes) in roms {
        let mut m = Machine::boot(&bytes);
        let mut samples = Vec::new();
        for _ in 0..APU_2005_FRAMES {
            m.frame();
            samples.extend(m.bus.apu.take_samples());
        }
        let beeps = beep_count(&samples);
        println!("apu_2005/{name}: {beeps} beep(s)");
        if beeps != 1 {
            failures.push(format!("{name}: reported {beeps} beep(s)"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every apu_reset ROM reports the code in `APU_RESET_ROMS` at `$6000`.
#[test]
fn the_apu_reset_suite_reports_the_expected_codes() {
    let roms: Vec<(&str, u8, Vec<u8>)> = APU_RESET_ROMS
        .iter()
        .filter_map(|(name, want)| {
            rom_bytes(&format!("test_roms/apu_reset/{name}")).map(|bytes| (*name, *want, bytes))
        })
        .collect();
    if roms.is_empty() {
        eprintln!("skipped: no ROMs in test_roms/apu_reset");
        return;
    }
    assert_eq!(
        roms.len(),
        APU_RESET_ROMS.len(),
        "test_roms/apu_reset is incomplete: {} of {} ROMs present",
        roms.len(),
        APU_RESET_ROMS.len()
    );

    let mut failures = Vec::new();
    for (name, want, bytes) in roms {
        let status = run_reset_rom(&bytes);
        println!("apu_reset/{name}: code {status}, want {want}");
        if status != want {
            failures.push(format!("{name}: reported code {status}, want {want}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Runs one apu_reset ROM to its final code: waits for it to identify itself by
/// writing `$80` (or asking for RESET with `$81`), presses RESET when it does,
/// and returns the first other code it writes.
fn run_reset_rom(bytes: &[u8]) -> u8 {
    /// `$6001-$6003`: "To allow an emulator to know when one of these tests is
    /// running ... $DE $B0 $G1 is written to $6001-$6003."
    const MARKER: [u8; 3] = [0xDE, 0xB0, 0x61];
    /// `$81` asks for the reset button "delayed by at least 100 msec from now".
    const RESET_DELAY_FRAMES: usize = 8;

    let mut m = Machine::boot(bytes);
    // `$6000` is zeroed at power-up, so a zero alone proves nothing: only a
    // code written after the ROM identified itself counts.
    let mut identified = false;
    let mut presses = 0;
    let mut prev = 0;
    for _ in 0..APU_RESET_FRAMES {
        m.frame();
        let status = m.read(0x6000);
        if status == 0x80 || status == 0x81 {
            assert_eq!(
                [m.read(0x6001), m.read(0x6002), m.read(0x6003)],
                MARKER,
                "a running apu_reset ROM identifies itself at $6001"
            );
            identified = true;
        }
        // `$81` survives the reset it asks for: the ROM only overwrites it
        // once it is running again. Press only on the change into `$81`, or
        // the stale value buys the ROM a reset it never asked for.
        let asking = status == 0x81 && prev != 0x81;
        if asking && presses < 3 {
            presses += 1;
            for _ in 0..RESET_DELAY_FRAMES {
                m.frame();
            }
            m.reset();
        } else if identified && status != 0x80 && status != 0x81 {
            println!("{}", report_text(&mut m));
            return status;
        }
        prev = status;
    }
    panic!(
        "apu_reset ROM never finished: identified={identified}, last status ${:02X}",
        m.read(0x6000)
    );
}

/// The ROM's own printed report, from `$6004` to its zero terminator: "All
/// text output is written starting at $6004, with a zero-byte terminator at
/// the end" (readme.txt).
fn report_text(m: &mut Machine) -> String {
    let mut text = String::new();
    for addr in 0x6004..0x6400u16 {
        let byte = m.read(addr);
        if byte == 0 {
            break;
        }
        if byte.is_ascii_graphic() || byte == b' ' || byte == b'\n' {
            text.push(byte as char);
        }
    }
    text
}
