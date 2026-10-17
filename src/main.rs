use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::Keycode;

mod bus;
mod controller;
mod cpu;
mod ppu;
mod rom;

use bus::Bus;
use cpu::Cpu;
use rom::Cartridge;

const W: usize = 256;
const H: usize = 240;
const SCALE: u32 = 3;

/// One NTSC frame is 262 scanlines of 341 dots, so 89342 PPU dots at 5.369 MHz
/// is 60.0988 Hz, 16.639 ms per frame; 16_667 us is the 60 Hz budget to sleep
/// to. The emulation boundary is the PPU's own `frame_ready`, one 341x262
/// frame including the odd-frame pre-render dot skip -- this constant is only
/// the wall-clock half of the period.
const FRAME_BUDGET: ::std::time::Duration = ::std::time::Duration::from_micros(16_667);

const USAGE: &str = "usage: nes <rom.nes> [--dump N[,N...]] [--play SCRIPT]\n\
    \x20 <rom.nes>          the cartridge to boot, quoted if it has spaces\n\
    \x20 --dump N[,N...]   after each listed frame write frame-N.ppm; the last\n\
    \x20                    one ends the run\n\
    \x20 --play SCRIPT      hold scripted buttons, e.g. START,WAIT60,RIGHT120\n\
    \x20                    NAME is A, B, SELECT, START, UP, DOWN, LEFT, RIGHT,\n\
    \x20                    JUMP, RUN, FIRE or WAIT; trailing digits are the\n\
    \x20                    frames to hold it (1 if none)";

/// The pad, one row per bit of `Controller::set_buttons` in its report order --
/// A, B, Select, Start, Up, Down, Left, Right -- as the SDL key that presses
/// it, the name `--play` accepts for it, and the bit itself.
const BUTTONS: [(Keycode, &str, u8); 8] = [
    (Keycode::Z, "A", 0x01),
    (Keycode::X, "B", 0x02),
    (Keycode::RShift, "SELECT", 0x04),
    (Keycode::Return, "START", 0x08),
    (Keycode::Up, "UP", 0x10),
    (Keycode::Down, "DOWN", 0x20),
    (Keycode::Left, "LEFT", 0x40),
    (Keycode::Right, "RIGHT", 0x80),
];

/// Words a script may use for a bit it does not spell out. A is jump and B is
/// run/fire in both SMB and Duck Hunt, so a script can say what it means.
const ALIASES: [(&str, &str); 3] = [("JUMP", "A"), ("RUN", "B"), ("FIRE", "B")];

/// Which button a held key presses, or `None` for a key that is not on the pad.
fn button_for(key: Keycode) -> Option<u8> {
    BUTTONS.iter().find(|row| row.0 == key).map(|row| row.2)
}

/// Which button a `--play` name presses. Aliases resolve to the real name
/// first, so there is still exactly one bit table in this file.
fn mask_for_name(name: &str) -> Option<u8> {
    let real = if BUTTONS.iter().any(|row| row.1 == name) {
        name
    } else {
        ALIASES.iter().find(|(alias, _)| *alias == name)?.1
    };
    BUTTONS.iter().find(|row| row.1 == real).map(|row| row.2)
}

/// A `--play` script: the held mask for each step and how many frames it lasts.
/// `RIGHT120` is Right held for 120 frames, `WAIT60` is nothing held for 60,
/// and a bare name is one frame. Unreadable input is an error, never a step
/// that silently holds nothing.
fn parse_script(script: &str) -> Result<Vec<(u8, u32)>, String> {
    let names = "A, B, SELECT, START, UP, DOWN, LEFT, RIGHT, JUMP, RUN, FIRE or WAIT";
    if script.trim().is_empty() {
        return Err(format!(
            "--play needs at least one step, e.g. START,WAIT60,RIGHT120"
        ));
    }
    let mut steps = Vec::new();
    for token in script.split(',') {
        let token = token.trim();
        // Split the name from its trailing frame count: the count is digits, so
        // the first non-letter ends the name.
        let split = token
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(token.len());
        let (name, count) = (&token[..split], &token[split..]);
        let frames: u32 = if count.is_empty() {
            1
        } else {
            count
                .parse()
                .map_err(|_| format!("--play step {token:?}: {count:?} is not a frame count"))?
        };
        if frames == 0 {
            return Err(format!(
                "--play step {token:?}: holding for 0 frames does nothing"
            ));
        }
        let mask = if name.eq_ignore_ascii_case("WAIT") {
            0
        } else {
            mask_for_name(name.to_ascii_uppercase().as_str()).ok_or_else(|| {
                format!("--play step {token:?}: {name:?} is not a button; use {names}")
            })?
        };
        steps.push((mask, frames));
    }
    Ok(steps)
}

/// A parsed script, played back one frame at a time.
struct Play {
    steps: Vec<(u8, u32)>,
    /// The mask currently held, the step it came from, and how many frames of
    /// that step are left to run.
    mask: u8,
    at: usize,
    left: u32,
}

impl Play {
    fn new(steps: Vec<(u8, u32)>) -> Play {
        Play {
            steps,
            mask: 0,
            at: 0,
            left: 0,
        }
    }

    /// The mask to hold for the frame about to be emulated, consuming one
    /// frame of the running step. `None` once the script has run out.
    fn mask_for_next_frame(&mut self) -> Option<u8> {
        // Step frames are always at least one (`parse_script` rejects zero), so
        // `left == 0` means "this step is done" and only then. A step holding
        // mask 0 (WAIT) is not done -- the mask is not what ends a step.
        while self.left == 0 {
            let &(mask, frames) = self.steps.get(self.at)?;
            self.mask = mask;
            self.left = frames;
            self.at += 1;
        }
        self.left -= 1;
        Some(self.mask)
    }
}

/// How long to sleep at the end of a frame that took `elapsed`. A frame that
/// overran gets no sleep at all and the next one starts fresh.
///
/// ponytail: no accumulator. A frame that blew its budget has already cost the
/// time; chasing the debt with a burst of zero-length frames only makes the
/// next one worse. Add a fixed-timestep accumulator if replay determinism
/// ever needs to decouple emulation from the clock.
fn frame_sleep(elapsed: ::std::time::Duration) -> ::std::time::Duration {
    FRAME_BUDGET.checked_sub(elapsed).unwrap_or_default()
}

fn main() -> Result<(), String> {
    let mut args = ::std::env::args().skip(1);
    let rom_path = args.next().ok_or_else(|| USAGE.to_string())?;
    let mut dump_at: Vec<u32> = Vec::new();
    let mut script = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dump" => {
                let list = args
                    .next()
                    .ok_or_else(|| format!("--dump needs a frame count\n{USAGE}"))?;
                for n in list.split(',') {
                    dump_at.push(n.trim().parse().map_err(|_| {
                        format!("--dump frame {n:?} is not a frame count\n{USAGE}")
                    })?);
                }
            }
            "--play" => {
                let text = args
                    .next()
                    .ok_or_else(|| format!("--play needs a script\n{USAGE}"))?;
                script = Some(parse_script(&text)?);
            }
            other => return Err(format!("unexpected argument {other:?}\n{USAGE}")),
        }
    }

    let sdl = sdl2::init().map_err(|e| e.to_string())?;
    let video = sdl.video().map_err(|e| e.to_string())?;
    let window = video
        .window("NES", (W as u32) * SCALE, (H as u32) * SCALE)
        .position_centered()
        .allow_highdpi()
        .build()
        .map_err(|e| e.to_string())?;
    let mut canvas = window.into_canvas().build().map_err(|e| e.to_string())?;
    canvas
        .set_logical_size(W as u32, H as u32)
        .map_err(|e| e.to_string())?;

    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture(
            sdl2::pixels::PixelFormatEnum::ARGB8888,
            sdl2::render::TextureAccess::Streaming,
            W as u32,
            H as u32,
        )
        .map_err(|e| e.to_string())?;

    let bytes = ::std::fs::read(&rom_path).map_err(|e| format!("{rom_path}: {e}"))?;
    let mut bus = Bus::new(Cartridge::load(&bytes)?);
    // The PPU fetches pattern memory every dot, so the CHR ROM the cartridge
    // parsed out of the file is loaded once into the PPU's banks.
    bus.ppu.load_chr_rom(bus.cart.chr_rom());
    let mut cpu = Cpu::new();
    bus.reset();
    cpu.reset(&mut bus);

    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;
    let mut play = script.map(Play::new);
    let mut buttons = 0u8;
    let mut running = true;
    let mut frames = 0u32;
    while running {
        // Started before anything else in the frame so the emulation, the
        // upload and the sleep are all inside the measured budget.
        let frame_start = ::std::time::Instant::now();
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => running = false,
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => running = false,
                Event::KeyDown {
                    keycode: Some(key), ..
                } => {
                    if let Some(bit) = button_for(key) {
                        buttons |= bit;
                    }
                }
                Event::KeyUp {
                    keycode: Some(key), ..
                } => {
                    if let Some(bit) = button_for(key) {
                        buttons &= !bit;
                    }
                }
                // No KeyUp ever arrives for a key released while another window
                // has the focus, so alt-tabbing away holding Right would latch
                // it on for good. Forget the whole mask instead.
                Event::Window {
                    win_event: WindowEvent::FocusLost,
                    ..
                } => buttons = 0,
                _ => {}
            }
        }
        // A script is not a second input path: it assigns the same mask a key
        // press assigns, and it goes through the one `set_buttons` call below.
        if let Some(script) = play.as_mut() {
            match script.mask_for_next_frame() {
                Some(mask) => buttons = mask,
                None => running = false,
            }
        }
        bus.controllers[0].set_buttons(buttons);
        // Run the CPU until the PPU has a whole frame, taking the NMI the
        // vblank flag raises. SMB gates its status bar on a sprite-0 hit and
        // its NMI handler on `$2002`, so this is where the machine actually
        // lives; Task 7 takes the timing and audio from here.
        while !bus.ppu.frame_ready() {
            // An OAM DMA halts the CPU for its 513 cycles while the PPU keeps
            // clocking; `tick` is one dot, so three of them per cycle drain it.
            if bus.dma_busy() {
                bus.tick();
                continue;
            }
            cpu.step(&mut bus);
            if bus.ppu.nmi_pending() {
                bus.ppu.take_nmi();
                cpu.nmi(&mut bus);
            }
        }
        // The 2A03's maskable interrupt is the APU's and only the APU's, and
        // `$4015` bits 7-6 are its two request flags (NESdev "APU", $4015 read:
        // "IF-D NT21"). They read 0 here because there is no APU, so this never
        // fires.
        //
        // ponytail: polled once a frame, not per instruction. Reading $4015
        // clears the frame-counter flag, so a per-instruction poll would eat the
        // IRQ before the APU task can deliver it; swap this for a level-triggered
        // IRQ line the APU raises when there is one.
        if bus.cpu_read(0x4015) & 0xC0 != 0 {
            cpu.irq(&mut bus);
        }
        // `take_frame` is what clears the ready flag, and it is the ONLY thing that
        // does -- without this the loop above exits once and never runs again.
        // Its returned clone is dropped; the upload reads the PPU's own buffer
        // by reference instead, so a frame costs one 240 KiB copy rather than
        // two.
        let _ = bus.ppu.take_frame();
        let frame = bus.ppu.framebuffer();
        frames += 1;
        if let Some(n) = dump_at.iter().find(|&&n| n == frames) {
            write_ppm(&format!("frame-{n}.ppm"), frame)?;
        }
        if play.is_none() && dump_at.last() == Some(&frames) {
            // With no script the dump list is the whole run, so the last frame
            // named ends it.
            return Ok(());
        }
        // SAFETY: u32 is plain old data and every bit pattern is a valid u32, so
        // all len*4 bytes of the frame are valid to read as u8.
        let frame_bytes =
            unsafe { ::std::slice::from_raw_parts(frame.as_ptr() as *const u8, frame.len() * 4) };
        texture
            .update(None, frame_bytes, W * 4)
            .map_err(|e| e.to_string())?;
        canvas.clear();
        canvas
            .copy(&texture, None, None)
            .map_err(|e| e.to_string())?;
        canvas.present();
        ::std::thread::sleep(frame_sleep(frame_start.elapsed()));
    }
    Ok(())
}

/// One framebuffer pixel is `0x00RRGGBB` (see `Ppu::colour`), and binary PPM
/// `P6` wants three bytes per pixel in R, G, B order, so the only work is
/// dropping the byte the mask left clear. Convert to PNG outside the crate --
/// `sdl2` is the only dependency this project has.
fn write_ppm(path: &str, frame: &[u32]) -> Result<(), String> {
    let rgb: Vec<u8> = frame
        .iter()
        .flat_map(|&px| [(px >> 16) as u8, (px >> 8) as u8, px as u8])
        .collect();
    let mut out = Vec::with_capacity(rgb.len() + 32);
    out.extend_from_slice(format!("P6\n{W} {H}\n255\n").as_bytes());
    out.extend_from_slice(&rgb);
    ::std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{button_for, frame_sleep, mask_for_name, parse_script, Play, FRAME_BUDGET};
    use sdl2::keyboard::Keycode;
    use std::time::Duration;

    #[test]
    fn a_script_is_a_list_of_mask_and_frame_pairs() {
        assert_eq!(
            parse_script("START,WAIT60,RIGHT120").unwrap(),
            [(0x08, 1), (0, 60), (0x80, 120)],
            "Start for a frame, nothing for 60, Right for 120"
        );
    }

    #[test]
    fn every_button_name_presses_its_own_distinct_bit() {
        let names = ["A", "B", "SELECT", "START", "UP", "DOWN", "LEFT", "RIGHT"];
        for (i, name) in names.iter().enumerate() {
            let bit = mask_for_name(name).unwrap_or_else(|| panic!("{name} has no bit"));
            assert_eq!(
                bit,
                0x01 << i,
                "{name} is bit {i} of the controller report order"
            );
            // Distinct bits, or a step could not tell two buttons apart.
            for other in &names[i + 1..] {
                assert_ne!(
                    mask_for_name(other),
                    Some(bit),
                    "{name} and {other} share bit {bit:#04x}"
                );
            }
        }
        assert_eq!(mask_for_name("JUMP"), Some(0x01), "JUMP is A");
        assert_eq!(mask_for_name("RUN"), Some(0x02), "RUN is B");
        assert_eq!(mask_for_name("FIRE"), Some(0x02), "FIRE is B");
    }

    #[test]
    fn a_key_presses_the_bit_its_script_name_promises() {
        // One bit table, two front ends: if these drift, a script and a key
        // would press different buttons.
        for (key, name, bit) in super::BUTTONS {
            assert_eq!(button_for(key), Some(bit), "{name} key");
            assert_eq!(mask_for_name(name), Some(bit), "{name} script");
        }
        assert_eq!(button_for(Keycode::Escape), None, "Escape quits");
        assert_eq!(button_for(Keycode::LShift), None, "not on the pad");
    }

    /// Garbage in must be an error, not a script that quietly does nothing --
    /// a silent no-op looks exactly like a working run until you look at a
    /// screenshot.
    #[test]
    fn a_script_it_cannot_read_is_an_error_naming_the_offending_step() {
        for bad in [
            "",                                // empty
            "   ",                             // whitespace only
            "FOO",                             // not a button
            "RIGHT0",                          // zero frames
            "12",                              // no button at all
            "RIGHTx",                          // frame count is not a number
            "RIGHT-3",                         // negative frames
            "START,,R",                        // empty step
            "RIGHT 4",                         // space in the count
            "START,RIGHT99999999999999999999", // count overflows u32
        ] {
            let err = parse_script(bad).expect_err(&format!("{bad:?} should not parse"));
            assert!(!err.is_empty(), "{bad:?} gave an empty error");
        }
        let err = parse_script("FOO12").unwrap_err();
        assert!(err.contains("FOO"), "error should name the bad step: {err}");
    }

    #[test]
    fn a_script_plays_its_steps_in_order_then_runs_out() {
        let mut play = Play::new(vec![(0x08, 2), (0, 2), (0x80, 3)]);
        let got: Vec<Option<u8>> = (0..9).map(|_| play.mask_for_next_frame()).collect();
        assert_eq!(
            got,
            [
                Some(0x08),
                Some(0x08),
                Some(0),
                Some(0),
                Some(0x80),
                Some(0x80),
                Some(0x80),
                None, // 7 frames of script, then out
                None, // and it stays out
            ],
            "Start, Start, nothing, nothing, Right x3"
        );
    }

    #[test]
    fn a_sleep_fills_out_the_frame_budget_and_never_goes_negative() {
        assert_eq!(frame_sleep(Duration::ZERO), FRAME_BUDGET, "free frame");
        assert_eq!(
            frame_sleep(FRAME_BUDGET / 2),
            FRAME_BUDGET / 2,
            "half a frame used, half to sleep"
        );
        assert_eq!(
            frame_sleep(FRAME_BUDGET),
            Duration::ZERO,
            "exactly on budget, no sleep"
        );
        // Behind: drop the debt instead of chasing it, and certainly do not
        // wrap around into an enormous sleep.
        for over in [
            FRAME_BUDGET * 3,
            Duration::from_secs(5),
            Duration::MAX,
            Duration::from_nanos(u64::MAX),
        ] {
            assert_eq!(frame_sleep(over), Duration::ZERO, "{over:?} overran");
        }
    }
}
