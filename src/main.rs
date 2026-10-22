use sdl2::controller::Button;
use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::Keycode;

mod apu;
#[cfg(test)]
mod apu_rom_test;
mod bus;
mod controller;
mod cpu;
mod launcher;
#[cfg(test)]
mod nestest_test;
mod ppu;
#[cfg(test)]
mod ppu_rom_test;
mod report;
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

/// How often a battery-backed cartridge's 8 KiB of PRG RAM is flushed to disk
/// while the game runs. The process can be killed (the launcher's old drop path
/// did exactly that), so waiting for a clean exit loses every save made since
/// boot. Five seconds bounds that loss without making the disk the bottleneck.
const SAVE_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_secs(5);

/// Bytes one stereo frame of the mixer's `i16` output occupies.
const AUDIO_FRAME_BYTES: u32 = 4;

/// How much audio to keep queued in the device, in bytes: two video frames'
/// worth, about 33 ms. Slack enough to ride out a scheduling hiccup, little
/// enough that the sound stays with the picture.
const AUDIO_TARGET: u32 = 1600 * AUDIO_FRAME_BYTES;

/// A queue deeper than this is a stall -- a dragged window, a suspended
/// process -- being played back. Past it the frame's samples are dropped, so
/// the sound cannot end up seconds behind the picture with no bound at all.
/// Ten video frames is about 166 ms.
const AUDIO_MAX_QUEUED: u32 = 10 * 800 * AUDIO_FRAME_BYTES;

const USAGE: &str = "usage: nes <rom.nes> [--dump N[,N...]] [--play SCRIPT]\n\
    \x20 <rom.nes>          the cartridge to boot, quoted if it has spaces\n\
    \x20 --dump N[,N...]   after each listed frame write frame-N.ppm; the last\n\
    \x20                    one ends the run\n\
    \x20 --play SCRIPT      hold scripted buttons, e.g. START,WAIT60,RIGHT120\n\
    \x20                    NAME is A, B, SELECT, START, UP, DOWN, LEFT, RIGHT,\n\
    \x20                    JUMP, RUN, FIRE or WAIT; trailing digits are the\n\
    \x20                    frames to hold it (1 if none); several names joined\n\
    \x20                    with + are held together, e.g. RIGHT+A30";

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

/// The same eight bits from an SDL game controller. A is the bottom face
/// button (jump in SMB), B is the right one (run/fire); the D-pad is the NES
/// D-pad. Analogue sticks are deliberately not mapped: a d-pad is what the
/// games were written for and what every controller reports.
fn pad_bit(button: Button) -> Option<u8> {
    match button {
        Button::A => Some(0x01),
        Button::B => Some(0x02),
        Button::Back => Some(0x04),
        Button::Start => Some(0x08),
        Button::DPadUp => Some(0x10),
        Button::DPadDown => Some(0x20),
        Button::DPadLeft => Some(0x40),
        Button::DPadRight => Some(0x80),
        _ => None,
    }
}

/// Fold one SDL event into the held masks, returning whether the run continues.
/// `false` quits. Keyboard and gamepad are separate masks so releasing one
/// source cannot clear a button the other still holds; the caller ORs them.
/// Out of `main` so the arms are reachable from a test: `Event` is a plain enum
/// value, so a headless test can build a `FocusLost` without a window ever
/// existing.
fn apply_event(ev: Event, keys: &mut u8, pad: &mut u8) -> bool {
    match ev {
        Event::Quit { .. } => false,
        Event::KeyDown {
            keycode: Some(Keycode::Escape),
            ..
        } => false,
        Event::KeyDown {
            keycode: Some(key), ..
        } => {
            if let Some(bit) = button_for(key) {
                *keys |= bit;
            }
            true
        }
        Event::KeyUp {
            keycode: Some(key), ..
        } => {
            if let Some(bit) = button_for(key) {
                *keys &= !bit;
            }
            true
        }
        Event::ControllerButtonDown { button, .. } => {
            if let Some(bit) = pad_bit(button) {
                *pad |= bit;
            }
            true
        }
        Event::ControllerButtonUp { button, .. } => {
            if let Some(bit) = pad_bit(button) {
                *pad &= !bit;
            }
            true
        }
        // No KeyUp ever arrives for a key released while another window has the
        // focus, so alt-tabbing away holding Right would latch it on for good.
        // Forget both masks instead.
        Event::Window {
            win_event: WindowEvent::FocusLost,
            ..
        } => {
            *keys = 0;
            *pad = 0;
            true
        }
        _ => true,
    }
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
/// and a bare name is one frame. `RIGHT+A30` is several buttons at once -- the
/// names are OR-ed -- because a script that can only hold one mask cannot say
/// "walk right and jump". Unreadable input is an error, never a step that
/// silently holds nothing.
fn parse_script(script: &str) -> Result<Vec<(u8, u32)>, String> {
    let names = "A, B, SELECT, START, UP, DOWN, LEFT, RIGHT, JUMP, RUN, FIRE or WAIT";
    if script.trim().is_empty() {
        return Err("--play needs at least one step, e.g. START,WAIT60,RIGHT120".to_string());
    }
    let mut steps = Vec::new();
    for token in script.split(',') {
        let token = token.trim();
        // Split the names from the trailing frame count: the count is digits,
        // so the first character that is neither a name character nor the `+`
        // separator ends the names. `+` belongs to the separator scan and not
        // to a name -- `WAIT+A5` is "WAIT and A", never a button "WAIT+".
        let split = token
            .find(|c: char| !(c.is_ascii_alphabetic() || c == '+'))
            .unwrap_or(token.len());
        let (held, count) = (&token[..split], &token[split..]);
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
        // One lookup per name, OR-ed together. Splitting on `+` first is what
        // makes an empty half an error naming that half, rather than a name
        // that is never found.
        let mut mask = 0;
        for name in held.split('+') {
            mask |= if name.eq_ignore_ascii_case("WAIT") {
                0
            } else {
                mask_for_name(name.to_ascii_uppercase().as_str()).ok_or_else(|| {
                    format!(
                        "--play step {token:?}: {name:?} is not a button; \
                         join several of them with + out of {names}"
                    )
                })?
            };
        }
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

/// How long to wait before emulating the next frame, given how much audio the
/// device has not drained yet.
///
/// The device consumes 48 kHz exactly, so its queue is the emulator's real
/// clock and this is that clock's own period: wait for as many of its frames
/// as are queued above the target. Waiting on `FRAME_BUDGET` instead runs the
/// emulator at 60.000 fps against NTSC's 60.0988, which starves a 48 kHz queue
/// by about 79 samples a second -- an audible click every few seconds.
///
/// Zero at or below the target: a queue that has drained needs the emulator to
/// run as fast as it can, not to wait. Capped at `FRAME_BUDGET`: a device that
/// accepts samples and never plays them must not slow the picture below what
/// the emulator does with no audio at all (what it holds past `AUDIO_MAX_QUEUED`
/// is dropped, so that queue cannot grow without bound either).
fn audio_sleep(queued: u32) -> ::std::time::Duration {
    let excess = queued.saturating_sub(AUDIO_TARGET) as u64;
    let drain = ::std::time::Duration::from_micros(
        excess * 1_000_000 / (48_000 * AUDIO_FRAME_BYTES as u64),
    );
    drain.min(FRAME_BUDGET)
}

/// With no script the dump list is the whole run, so the last frame named ends
/// it. This is the maximum, not the position: `--dump 5,3` still has to reach
/// frame 5, or the caller asked for a frame and silently got none of it.
fn dump_list_ends_run(dump_at: &[u32], frames: u32) -> bool {
    dump_at.iter().copied().max() == Some(frames)
}

/// The requested dump frames a run that reached `frames` never wrote. Reported
/// on stderr rather than turned into an error: the frames are simply not
/// there, and saying so is what stops it reading as "the game froze".
fn missed_dumps(dump_at: &[u32], frames: u32) -> Vec<u32> {
    let mut missed: Vec<u32> = dump_at.iter().copied().filter(|&n| n > frames).collect();
    missed.sort_unstable();
    missed.dedup();
    missed
}

/// Every failure ends here: logged beside the executable and shown in a
/// message box, because a desktop launch has no terminal to read.
fn main() {
    report::install_panic_hook();
    if let Err(e) = run() {
        report::report(&e);
        ::std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = ::std::env::args().skip(1);
    // No ROM named is the launcher: a desktop icon and a bare `nes` both land
    // here, and the launcher starts this same executable with a ROM when one
    // is picked.
    let Some(rom_path) = args.next() else {
        return launcher::run();
    };
    let mut dump_at: Vec<u32> = Vec::new();
    let mut script = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dump" => {
                let list = args
                    .next()
                    .ok_or_else(|| format!("--dump needs a frame count\n{USAGE}"))?;
                for n in list.split(',') {
                    let n: u32 = n
                        .trim()
                        .parse()
                        .map_err(|_| format!("--dump frame {n:?} is not a frame count\n{USAGE}"))?;
                    if n == 0 {
                        // Frames are counted from one, so this would run until
                        // it was killed and still write nothing.
                        return Err(format!(
                            "--dump frame 0: frames are counted from 1\n{USAGE}"
                        ));
                    }
                    dump_at.push(n);
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
    // Audio is best-effort: the picture is the primary output, so a machine
    // with no audio subsystem, no device or a refused spec still emulates,
    // silently, at full speed.
    let audio: Option<sdl2::audio::AudioQueue<i16>> = sdl.audio().ok().and_then(|audio| {
        sdl2::audio::AudioQueue::<i16>::open_queue(
            &audio,
            None,
            &sdl2::audio::AudioSpecDesired {
                freq: Some(48_000),
                channels: Some(2),
                samples: Some(1024),
            },
        )
        .ok()
    });
    let window = video
        .window("NES", (W as u32) * SCALE, (H as u32) * SCALE)
        .position_centered()
        .allow_highdpi()
        // Resizable is what gives the window manager a maximize button to grey
        // out otherwise. The canvas keeps the 256x240 logical size, so SDL
        // scales the frame into whatever window the WM makes and letterboxes
        // the rest. Starting maximized is the same scaling on the first frame,
        // with no lost screen on a desktop that has plenty.
        .resizable()
        .maximized()
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
    let mut cpu = Cpu::new();
    bus.power_on();
    cpu.reset(&mut bus);

    // Battery-backed PRG RAM (MMC1's Zelda save) lives next to the ROM. A
    // missing file starts zeroed; any other read failure is reported rather
    // than silently treated as a blank save and overwritten later.
    let save_path = bus
        .cart
        .has_battery()
        .then(|| ::std::path::Path::new(&rom_path).with_extension("sav"));
    if let Some(path) = &save_path {
        match ::std::fs::read(path) {
            Ok(saved) => {
                if saved.len() != bus.prg_ram().len() {
                    report::log_error(&format!(
                        "{}: save is {} bytes, expected {}; loading what is there",
                        path.display(),
                        saved.len(),
                        bus.prg_ram().len()
                    ));
                }
                bus.load_prg_ram(&saved);
            }
            Err(e) if e.kind() == ::std::io::ErrorKind::NotFound => {}
            Err(e) => report::log_error(&format!("cannot read {}: {e}", path.display())),
        }
    }
    let mut last_save = ::std::time::Instant::now();

    if let Some(queue) = &audio {
        // The queue is opened with no allowed changes, so a device that cannot
        // do exactly this spec fails the open instead of renegotiating it; the
        // mixer follows whatever rate the device did agree to.
        bus.apu.set_sample_rate(queue.spec().freq as u32);
        queue.resume();
    }

    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;
    // Game controllers are optional and hot-pluggable. The handles must stay
    // alive for their button events to keep arriving, so they live in `pads`
    // for the whole run even though nothing reads the vec.
    let controller_subsystem = sdl.game_controller().ok();
    let mut pads = Vec::new();
    if let Some(controllers) = &controller_subsystem {
        for index in 0..controllers.num_joysticks().unwrap_or(0) {
            if controllers.is_game_controller(index) {
                if let Ok(gamepad) = controllers.open(index) {
                    pads.push(gamepad);
                }
            }
        }
    }
    let mut play = script.map(Play::new);
    let mut keys = 0u8;
    let mut pad = 0u8;
    let mut paused = false;
    let mut muted = false;
    let mut fullscreen = false;
    let mut running = true;
    let mut frames = 0u32;
    while running {
        // Started before anything else in the frame so the emulation, the
        // upload and the sleep are all inside the measured budget.
        let frame_start = ::std::time::Instant::now();
        for event in events.poll_iter() {
            match event {
                Event::KeyDown {
                    keycode: Some(Keycode::P),
                    repeat: false,
                    ..
                } => {
                    paused = !paused;
                    if let Some(queue) = &audio {
                        if paused {
                            queue.pause();
                        } else if !muted {
                            queue.resume();
                        }
                    }
                }
                Event::KeyDown {
                    keycode: Some(Keycode::R),
                    repeat: false,
                    ..
                } => {
                    // RESET keeps RAM and the battery save; only the clock and
                    // the APU restart.
                    bus.reset();
                    cpu.reset(&mut bus);
                    if let Some(queue) = &audio {
                        queue.clear();
                    }
                }
                Event::KeyDown {
                    keycode: Some(Keycode::F11),
                    repeat: false,
                    ..
                } => {
                    fullscreen = !fullscreen;
                    let mode = if fullscreen {
                        sdl2::video::FullscreenType::Desktop
                    } else {
                        sdl2::video::FullscreenType::Off
                    };
                    canvas
                        .window_mut()
                        .set_fullscreen(mode)
                        .map_err(|e| e.to_string())?;
                }
                Event::KeyDown {
                    keycode: Some(Keycode::M),
                    repeat: false,
                    ..
                } => {
                    muted = !muted;
                    if let Some(queue) = &audio {
                        if muted {
                            queue.pause();
                            queue.clear();
                        } else if !paused {
                            queue.resume();
                        }
                    }
                }
                Event::ControllerDeviceAdded { which, .. } => {
                    if let Some(controllers) = &controller_subsystem {
                        if let Ok(gamepad) = controllers.open(which) {
                            pads.push(gamepad);
                        }
                    }
                }
                other => {
                    if !apply_event(other, &mut keys, &mut pad) {
                        running = false;
                    }
                }
            }
        }
        // A paused machine keeps its frame on screen and its script on hold;
        // time does not advance for either.
        if !paused {
            // A script is not a second input path: it assigns the same mask a
            // key press assigns, and it goes through the one `set_buttons` call
            // below.
            let mut buttons = keys | pad;
            if let Some(script) = play.as_mut() {
                match script.mask_for_next_frame() {
                    Some(mask) => buttons = mask,
                    // The script is over. Release what its last step held instead
                    // of emulating one more frame with it still down -- the frame
                    // that `running = false` still runs would be played with a key
                    // that the script already let go of.
                    None => {
                        buttons = 0;
                        running = false;
                    }
                }
            }
            bus.controllers[0].set_buttons(buttons);
            // Run the CPU until the PPU has a whole frame, taking the NMI the
            // vblank flag raises and the APU's frame IRQ off its level line. SMB
            // gates its status bar on a sprite-0 hit and its NMI handler on
            // `$2002`, so this is where the machine actually lives.
            bus.run_frame(&mut cpu);
            if let Some(queue) = &audio {
                let samples = bus.apu.take_samples();
                // Muted still drains the APU's queue, or the mix would grow
                // without bound; the samples are simply not handed to the device.
                if !muted && !samples.is_empty() {
                    // A queue past `AUDIO_MAX_QUEUED` is a stall being played
                    // back: drop this frame and let the device catch up to the
                    // picture rather than fall further behind it. A device that
                    // stops accepting samples must not stop the emulator either
                    // -- the picture is the primary output.
                    if queue.size() < AUDIO_MAX_QUEUED {
                        let _ = queue.queue_audio(&samples);
                    }
                }
            }
            // `take_frame` is what clears the ready flag, and it is the ONLY thing
            // that does -- without this the loop above exits once and never runs
            // again. The upload reads the PPU's own buffer by reference; a frame
            // is never copied.
            bus.ppu.take_frame();
            frames += 1;
            if let Some(path) = &save_path {
                if last_save.elapsed() >= SAVE_INTERVAL {
                    if let Err(e) = write_battery_save(path, bus.prg_ram()) {
                        report::log_error(&e);
                    }
                    last_save = ::std::time::Instant::now();
                }
            }
            if let Some(n) = dump_at.iter().find(|&&n| n == frames) {
                write_ppm(&format!("frame-{n}.ppm"), bus.ppu.framebuffer())?;
            }
            if play.is_none() && dump_list_ends_run(&dump_at, frames) {
                // With no script the dump list is the whole run, so the last
                // frame named ends it.
                break;
            }
        }
        let frame = bus.ppu.framebuffer();
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
        // With a device, its queue sets the pace; without one, the wall-clock
        // budget does. Either way the emulation is untouched by the wait. A
        // paused or muted run has no queue to follow, so it falls back to the
        // wall clock and keeps the window responsive.
        if paused {
            ::std::thread::sleep(FRAME_BUDGET);
        } else {
            match &audio {
                Some(queue) if !muted => ::std::thread::sleep(audio_sleep(queue.size())),
                _ => ::std::thread::sleep(frame_sleep(frame_start.elapsed())),
            }
        }
    }
    // A battery save is only useful if the process gets to exit normally, so
    // write it before the dump report can return an error.
    if let Some(path) = &save_path {
        if let Err(e) = write_battery_save(path, bus.prg_ram()) {
            report::log_error(&e);
        }
    }
    // The run stopped early -- the script ran out, or the user quit -- so any
    // dump frame past here was never written. Name them: a missing file with
    // exit code 0 is indistinguishable from a frozen game.
    let missed = missed_dumps(&dump_at, frames);
    if !missed.is_empty() {
        report::log_error(&format!(
            "--dump asked for frame{} {missed:?} but the run only reached frame {frames}",
            if missed.len() == 1 { "" } else { "s" },
        ));
    }
    Ok(())
}

/// Flush battery-backed PRG RAM without ever leaving a half-written `.sav` in
/// place. The bytes go to a sibling temp file, are synced, and only then replace
/// the save by rename; a crash or a kill during the write leaves the previous
/// save intact.
fn write_battery_save(path: &::std::path::Path, ram: &[u8]) -> Result<(), String> {
    let temp = path.with_extension("sav.tmp");
    let write = (|| {
        let mut file = ::std::fs::File::create(&temp)?;
        ::std::io::Write::write_all(&mut file, ram)?;
        file.sync_all()?;
        ::std::fs::rename(&temp, path)
    })();
    write.map_err(|e| format!("cannot write {}: {e}", path.display()))
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
    use super::{
        apply_event, audio_sleep, button_for, dump_list_ends_run, frame_sleep, mask_for_name,
        missed_dumps, parse_script, write_battery_save, Play, AUDIO_FRAME_BYTES, AUDIO_MAX_QUEUED,
        AUDIO_TARGET, FRAME_BUDGET,
    };
    use sdl2::event::{Event, WindowEvent};
    use sdl2::keyboard::Keycode;
    use std::time::Duration;

    #[test]
    fn a_battery_save_replaces_the_old_file_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("nes-save-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("game.sav");
        std::fs::write(&path, b"old save").expect("seed save");

        let ram = [0x5Au8; 8192];
        write_battery_save(&path, &ram).expect("save must be written");

        assert_eq!(std::fs::read(&path).unwrap(), ram, "the new save landed");
        assert!(
            !path.with_extension("sav.tmp").exists(),
            "the temp file must be renamed away, not left behind"
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_script_is_a_list_of_mask_and_frame_pairs() {
        assert_eq!(
            parse_script("START,WAIT60,RIGHT120").unwrap(),
            [(0x08, 1), (0, 60), (0x80, 120)],
            "Start for a frame, nothing for 60, Right for 120"
        );
    }

    /// One step can hold several buttons at once: `RIGHT+A30` is Right AND A
    /// for 30 frames. A single mask per step cannot express "walk right and
    /// jump", which is what a 3-tile-high pipe in WORLD 1-1 asks for.
    #[test]
    fn a_step_can_hold_two_buttons_at_once() {
        assert_eq!(
            parse_script("RIGHT+A30").unwrap(),
            [(0x80 | 0x01, 30)],
            "Right and A are OR-ed into one mask and last 30 frames"
        );
    }

    #[test]
    fn a_step_can_hold_three_buttons_at_once() {
        assert_eq!(
            parse_script("LEFT+B+START4").unwrap(),
            [(0x40 | 0x02 | 0x08, 4)],
            "Left, B and Start in one mask for 4 frames"
        );
    }

    /// The names are looked up one at a time, so the error has to say WHICH
    /// one was wrong -- "not a button" naming the whole step leaves the reader
    /// hunting for the bad half of it.
    #[test]
    fn an_unknown_name_inside_a_multi_button_step_names_that_name() {
        // Both halves of the assertion matter. Naming "FOO" alone is worthless:
        // the pre-fix error reads `"+FOO30" is not a frame count`, which also
        // contains the letters FOO. It has to be rejected AS A NAME.
        for bad in ["RIGHT+FOO30", "RIGHT+FOO+ALSONOPE30"] {
            let err = parse_script(bad).expect_err(&format!("{bad:?} should not parse"));
            assert!(
                err.contains("is not a button"),
                "{bad:?} must be rejected as an unknown name, not as a bad count: {err}"
            );
            for name in bad.split('+').skip(1).map(|s| {
                &s[..s
                    .find(|c: char| !c.is_ascii_alphabetic())
                    .unwrap_or(s.len())]
            }) {
                assert!(err.contains(name), "error must name {name:?}: {err}");
            }
        }
        // The same rejection as a single-name step: joining with + is not a way
        // to smuggle an unknown name past the check.
        assert!(parse_script("FOO30").is_err());
    }

    /// `+` with nothing on one side is a typo, not a step holding fewer
    /// buttons. Treating it as nothing is the worst outcome: the run looks
    /// like it works and simply does not press what was asked for.
    #[test]
    fn a_separator_with_no_button_on_one_side_is_rejected() {
        for bad in [
            "+RIGHT30",   // nothing before the separator
            "RIGHT+30",   // nothing after it, and then a frame count
            "RIGHT+",     // nothing after it at all
            "RIGHT++A30", // an empty hole between two separators
            "+",          // and a step that is only a separator
            "+30",        // which also has no name at all
        ] {
            let err = parse_script(bad).expect_err(&format!("{bad:?} should not parse"));
            assert!(!err.is_empty(), "{bad:?} gave an empty error");
        }
    }

    /// `+` is a separator and nothing else. If it were part of the name,
    /// `WAIT+A5` would look up one button called "WAIT+" and be rejected --
    /// or worse, be accepted by a lookup that merely ignored the separator and
    /// never pressed A.
    #[test]
    fn the_separator_is_never_part_of_a_button_name() {
        assert_eq!(
            parse_script("WAIT+A5").unwrap(),
            [(0x01, 5)],
            r#"WAIT contributes nothing, A contributes its bit: "WAIT+" is not a name"#
        );
        // And a name may not end in the separator, which is the same defect
        // read from the other side.
        assert!(parse_script("WAIT+5").is_err());
    }

    /// Every single-button form keeps meaning exactly what it meant before
    /// `+` existed -- `--play "WAIT297,START10,..."` is load-bearing for every
    /// verification in this project, aliases included.
    #[test]
    fn single_button_steps_are_unchanged_by_the_separator() {
        for (script, want) in [
            ("START", (0x08u8, 1u32)),
            ("WAIT297", (0, 297)),
            ("RIGHT200", (0x80, 200)),
            ("A1", (0x01, 1)),
            ("B1", (0x02, 1)),
            ("SELECT1", (0x04, 1)),
            ("UP1", (0x10, 1)),
            ("DOWN1", (0x20, 1)),
            ("LEFT1", (0x40, 1)),
            ("JUMP60", (0x01, 60)),
            ("RUN60", (0x02, 60)),
            ("FIRE60", (0x02, 60)),
            ("right60", (0x80, 60)), // still case-insensitive
        ] {
            assert_eq!(
                parse_script(script).unwrap(),
                [want],
                "{script:?} must mean exactly what it always did"
            );
        }
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

    /// The audio clock. Sleeping for the queued excess must drain exactly that
    /// excess: then a frame's worth of audio queued above the target is a
    /// frame's worth of waiting, and the queue neither grows nor starves.
    #[test]
    fn audio_sleep_waits_as_long_as_the_device_needs_to_drain_the_excess() {
        assert_eq!(audio_sleep(0), Duration::ZERO, "empty queue: run flat out");
        assert_eq!(
            audio_sleep(AUDIO_TARGET),
            Duration::ZERO,
            "at the target: run"
        );

        // One NTSC frame of audio: 48000 / 60.0988 = 798.7 stereo frames.
        let frame = (48_000.0 / 60.0988 * AUDIO_FRAME_BYTES as f64) as u32;
        let wait = audio_sleep(AUDIO_TARGET + frame);
        let drained = wait.as_secs_f64() * 48_000.0 * AUDIO_FRAME_BYTES as f64;
        assert!(
            (drained - frame as f64).abs() < 1.0,
            "{frame} bytes over the target waited out {drained} bytes"
        );
        assert!(
            (0.0163..0.0167).contains(&wait.as_secs_f64()),
            "a frame of audio is a frame of waiting, not {wait:?}"
        );

        // Deeper is longer, but a device that takes samples and never plays
        // them cannot hold the picture below the no-audio rate, and the deepest
        // queue kept is a bounded pause: past it, frames are dropped.
        assert!(audio_sleep(AUDIO_TARGET + 2 * frame) > wait);
        for deep in [AUDIO_MAX_QUEUED, u32::MAX] {
            assert!(
                audio_sleep(deep) <= FRAME_BUDGET,
                "{deep} bytes queued waits longer than a frame"
            );
        }
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

    /// `--dump 5,3` used to end the run after frame 3: frame 5 was never
    /// written, nothing was said, and the exit code was still 0. Task 9 reads
    /// dumps, so a silently dropped frame reads as "the game froze". The end of
    /// the run is the highest frame named, in whatever order they were named.
    #[test]
    fn an_unsorted_dump_list_still_runs_to_its_highest_frame() {
        for list in [&[5u32, 3][..], &[9, 3, 5][..], &[3, 5][..]] {
            let highest = list.iter().copied().max().unwrap();
            for frames in 1..=highest {
                assert_eq!(
                    dump_list_ends_run(list, frames),
                    frames == highest,
                    "--dump {list:?} must end at {highest}, not at {frames}"
                );
            }
        }
    }

    /// SDL sends no `KeyUp` for a key released while another window has the
    /// focus, so the FocusLost arm is the only thing that un-latches a held
    /// button. `Event` is a plain enum value, so this runs with no window and
    /// no X server -- it needed the handler extracted from `main` to be
    /// reachable at all, not a real alt-tab.
    #[test]
    fn a_focus_loss_clears_the_held_masks() {
        let mut keys = 0xFF;
        let mut pad = 0xFF;
        let lost = Event::Window {
            timestamp: 0,
            window_id: 0,
            win_event: WindowEvent::FocusLost,
        };
        assert!(
            apply_event(lost, &mut keys, &mut pad),
            "losing focus is not a quit"
        );
        assert_eq!((keys, pad), (0, 0), "every held bit must be forgotten");
    }

    #[test]
    fn a_key_up_clears_only_its_own_bit() {
        let down = |key| Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(key),
            scancode: None,
            keymod: sdl2::keyboard::Mod::empty(),
            repeat: false,
        };
        let up = |key| Event::KeyUp {
            timestamp: 1,
            window_id: 0,
            keycode: Some(key),
            scancode: None,
            keymod: sdl2::keyboard::Mod::empty(),
            repeat: false,
        };
        let mut keys = 0u8;
        let mut pad = 0u8;
        for key in [Keycode::Z, Keycode::Right, Keycode::Return] {
            assert!(apply_event(down(key), &mut keys, &mut pad), "{key:?} down");
        }
        assert_eq!(keys, 0x01 | 0x80 | 0x08, "Z, Right and Return held");
        assert!(
            apply_event(up(Keycode::Right), &mut keys, &mut pad),
            "Right up"
        );
        assert_eq!(
            keys,
            0x01 | 0x08,
            "only Right's bit goes; Z and Return are still down"
        );
        // Escape quits, a key with no bit does nothing, and both still say the
        // run continues.
        assert!(
            !apply_event(down(Keycode::Escape), &mut keys, &mut pad),
            "Escape ends the run"
        );
        let before = keys;
        assert!(
            apply_event(down(Keycode::F5), &mut keys, &mut pad),
            "F5 is not a quit"
        );
        assert_eq!(keys, before, "F5 is not on the pad");
    }

    /// Keyboard and gamepad are separate masks so releasing one source cannot
    /// release a button the other still holds.
    #[test]
    fn keyboard_and_gamepad_hold_the_same_bits_independently() {
        let key_down = Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(Keycode::Z),
            scancode: None,
            keymod: sdl2::keyboard::Mod::empty(),
            repeat: false,
        };
        let key_up = Event::KeyUp {
            timestamp: 1,
            window_id: 0,
            keycode: Some(Keycode::Z),
            scancode: None,
            keymod: sdl2::keyboard::Mod::empty(),
            repeat: false,
        };
        let pad_down = Event::ControllerButtonDown {
            timestamp: 2,
            which: 0,
            button: sdl2::controller::Button::A,
        };
        let pad_up = Event::ControllerButtonUp {
            timestamp: 3,
            which: 0,
            button: sdl2::controller::Button::A,
        };
        let mut keys = 0u8;
        let mut pad = 0u8;
        apply_event(key_down, &mut keys, &mut pad);
        apply_event(pad_down, &mut keys, &mut pad);
        assert_eq!((keys, pad), (0x01, 0x01), "Z and pad A both hold A");
        apply_event(key_up, &mut keys, &mut pad);
        assert_eq!((keys, pad), (0x00, 0x01), "pad A keeps A held");
        apply_event(pad_up, &mut keys, &mut pad);
        assert_eq!((keys, pad), (0x00, 0x00));
    }

    /// A dump frame past the end of the run is written nowhere and reported
    /// nowhere, which is the same silent under-delivery as stopping early.
    #[test]
    fn a_dump_frame_past_the_end_of_the_run_is_named_as_missed() {
        assert_eq!(
            missed_dumps(&[600, 1400], 1400),
            Vec::<u32>::new(),
            "all reached"
        );
        assert_eq!(missed_dumps(&[600, 1400], 7), [600, 1400], "none reached");
        assert_eq!(
            missed_dumps(&[600, 1400], 1000),
            [1400],
            "the later one only"
        );
        assert_eq!(missed_dumps(&[9, 3, 5], 7), [9], "order does not matter");
    }
}
