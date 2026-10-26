use sdl2::controller::Button;
use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::{Keycode, Mod, Scancode};

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
mod region;
mod report;
mod rom;
mod state;
mod text;

use bus::Bus;
use cpu::Cpu;
use region::Region;
use rom::Cartridge;

const W: usize = 256;
const H: usize = 240;
const SCALE: u32 = 3;

/// One NTSC frame is 262 scanlines of 341 dots, so 89342 PPU dots at 5.369 MHz
/// is 60.0988 Hz, 16.639 ms per frame; 16_667 us is the 60 Hz budget to sleep
/// to. The emulation boundary is the PPU's own `frame_ready`, one 341x262
/// frame including the odd-frame pre-render dot skip -- this constant is only
/// the wall-clock half of the period. PAL gets its own budget below.
const FRAME_BUDGET: ::std::time::Duration = ::std::time::Duration::from_micros(16_667);

/// One PAL frame is 312 scanlines of 341 dots = 106,392 dots at 5.320342 MHz,
/// 19,997.6 us. 20 ms is the 50 Hz budget.
const FRAME_BUDGET_PAL: ::std::time::Duration = ::std::time::Duration::from_micros(20_000);

/// The wall-clock budget for one frame of `region`.
fn frame_budget(region: Region) -> ::std::time::Duration {
    match region {
        Region::Ntsc => FRAME_BUDGET,
        Region::Pal => FRAME_BUDGET_PAL,
    }
}

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

/// Quick save slots, in key order. A bare digit writes the slot under the key;
/// left Shift plus the same digit loads it, because loading throws away the run
/// in progress and should take a deliberate press. Right Shift is Select, so it
/// must not double as the load modifier. `F8` loads whichever slot was written
/// most recently.
const SLOT_KEYS: [Scancode; 9] = [
    Scancode::Num1,
    Scancode::Num2,
    Scancode::Num3,
    Scancode::Num4,
    Scancode::Num5,
    Scancode::Num6,
    Scancode::Num7,
    Scancode::Num8,
    Scancode::Num9,
];

/// Which quick save slot a key names, one-based, or `None` for any other key.
/// Matched by scancode, so the digit key is the same one under any layout and
/// with Shift held.
fn slot_for(key: Scancode) -> Option<usize> {
    SLOT_KEYS
        .iter()
        .position(|&slot_key| slot_key == key)
        .map(|index| index + 1)
}

/// Where slot `slot` lives: beside the ROM, next to its battery save, so a
/// folder of ROMs takes its states with it. `Zelda.nes` becomes
/// `Zelda.state3`.
fn state_slot_path(rom_path: &::std::path::Path, slot: usize) -> ::std::path::PathBuf {
    rom_path.with_extension(format!("state{slot}"))
}

/// The one-byte index naming the slot `F8` loads. Kept beside the states, and
/// written after every successful save, so "latest" does not depend on file
/// times -- a copied or restored save folder would otherwise reorder itself.
fn latest_state_path(rom_path: &::std::path::Path) -> ::std::path::PathBuf {
    rom_path.with_extension("state")
}

/// The slot named by the index file. Missing is `None` (nothing saved yet);
/// an unreadable, unparsable or out-of-range index is an error, because guessing
/// would load a state the player did not ask for.
fn read_latest_slot(rom_path: &::std::path::Path) -> Result<Option<usize>, String> {
    let path = latest_state_path(rom_path);
    let text = match ::std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ::std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let text = text.trim();
    let slot: usize = text.parse().map_err(|_| {
        format!(
            "{}: expected a save slot number, found {text:?}",
            path.display()
        )
    })?;
    if !(1..=SLOT_KEYS.len()).contains(&slot) {
        return Err(format!(
            "{}: names slot {slot}, but slots are 1-{}",
            path.display(),
            SLOT_KEYS.len()
        ));
    }
    Ok(Some(slot))
}

/// Write a state to a slot with the same temp-then-rename dance as the battery
/// save, so a crash mid-write leaves the previous state intact. The index is
/// updated only after the state itself is safely on disk.
fn save_state_slot(
    rom_path: &::std::path::Path,
    slot: usize,
    bus: &Bus,
    cpu: &Cpu,
) -> Result<(), String> {
    let path = state_slot_path(rom_path, slot);
    write_atomic(&path, &bus.save_state(cpu))
        .map_err(|e| format!("cannot save state slot {slot}: {e}"))?;
    write_atomic(&latest_state_path(rom_path), format!("{slot}\n").as_bytes())
        .map_err(|e| format!("slot {slot} saved, but cannot record it for F8: {e}"))
}

/// Apply a slot to a running machine. `Bus::load_state` validates and swaps the
/// whole machine, so a missing, corrupt or foreign state leaves play untouched.
fn load_state_slot(
    rom_path: &::std::path::Path,
    slot: usize,
    bus: &mut Bus,
    cpu: &mut Cpu,
) -> Result<(), String> {
    let path = state_slot_path(rom_path, slot);
    let data = ::std::fs::read(&path).map_err(|e| match e.kind() {
        ::std::io::ErrorKind::NotFound => format!("state slot {slot} is empty: {}", path.display()),
        _ => format!("cannot read {}: {e}", path.display()),
    })?;
    bus.load_state(cpu, &data)
        .map_err(|e| format!("cannot load {}: {e}", path.display()))
}

fn load_latest_state(
    rom_path: &::std::path::Path,
    bus: &mut Bus,
    cpu: &mut Cpu,
) -> Result<usize, String> {
    let slot = read_latest_slot(rom_path)?.ok_or_else(|| {
        format!(
            "no save states for {}: press 1-9 to make one",
            rom_path.display()
        )
    })?;
    load_state_slot(rom_path, slot, bus, cpu).map(|()| slot)
}

/// A load replaces the machine's timeline: the frame the loop paused on belongs
/// to the old one, and the queued samples are noise over the loaded state. Drop
/// them, unpause so the next iteration renders what was restored, and restart
/// the queue unless the player muted it.
fn resume_after_load(paused: &mut bool, muted: bool, audio: Option<&sdl2::audio::AudioQueue<i16>>) {
    *paused = false;
    if let Some(queue) = audio {
        queue.clear();
        if !muted {
            queue.resume();
        }
    }
}

/// A save or load writes no message of its own, so a short confirmation is
/// the only sign it happened. Long enough to read, short enough to get out of
/// the way.
const CONFIRM_TOAST: ::std::time::Duration = ::std::time::Duration::from_secs(2);

/// Failures carry a path and a reason; they stay up longer, and the full text
/// is still in stderr and `nes-errors.log`.
const ERROR_TOAST: ::std::time::Duration = ::std::time::Duration::from_secs(6);

/// The overlay uses the same 8x8 bitmap font as the launcher, 28 characters
/// wide inside the 256-pixel screen, with a dark box behind the text so it
/// stays readable over a bright game.
const TOAST_COLS: usize = 28;
const TOAST_PAD: usize = 4;
const TOAST_MARGIN: usize = 8;
/// Errors from a deep path can wrap to many lines. The first screenful is
/// shown; the last line points at the log that kept the whole message.
const TOAST_MAX_LINES: usize = 10;
const TOAST_BG: u32 = 0x0010_1418;
const TOAST_FG: u32 = 0x00E8_E8E8;

/// A short message drawn over the frame. Held in the main loop rather than in
/// the emulator, so showing it can never disturb a save state.
struct Toast {
    lines: Vec<String>,
    until: ::std::time::Instant,
}

impl Toast {
    fn new(message: &str, ttl: ::std::time::Duration) -> Toast {
        let mut lines = wrap_text(message, TOAST_COLS);
        if lines.len() > TOAST_MAX_LINES {
            lines.truncate(TOAST_MAX_LINES - 1);
            lines.push("... see nes-errors.log".to_string());
        }
        Toast {
            lines,
            until: ::std::time::Instant::now() + ttl,
        }
    }

    fn visible(&self) -> bool {
        ::std::time::Instant::now() < self.until
    }
}

/// Greedy word wrap at `width` characters. A word longer than a whole line --
/// a path or a filename -- is cut at the edge rather than dropped, so the
/// start of it still reaches the player.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    if width == 0 {
        return lines;
    }
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word = word;
        loop {
            let used = line.chars().count();
            let room = width.saturating_sub(used + usize::from(used > 0));
            if room == 0 {
                lines.push(::std::mem::take(&mut line));
                continue;
            }
            let word_len = word.chars().count();
            if word_len <= room {
                if used > 0 {
                    line.push(' ');
                }
                line.push_str(word);
                break;
            }
            // A word that does not fit moves to a fresh line whole; only a
            // word longer than the screen is cut, and only after it starts a
            // line.
            if used > 0 {
                lines.push(::std::mem::take(&mut line));
                continue;
            }
            let cut = word
                .char_indices()
                .nth(room)
                .map_or(word.len(), |(index, _)| index);
            let (head, tail) = word.split_at(cut);
            line.push_str(head);
            word = tail;
            if word.is_empty() {
                break;
            }
            lines.push(::std::mem::take(&mut line));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Paint a centered box holding the toast's lines near the bottom of the
/// frame. `Toast::new` bounds the width and line count, so the box always
/// fits.
fn draw_toast(fb: &mut [u32], toast: &Toast) {
    let Some(text_width) = toast.lines.iter().map(|line| line.chars().count()).max() else {
        return;
    };
    let box_width = text_width * text::CELL + 2 * TOAST_PAD;
    let box_height = toast.lines.len() * text::CELL + 2 * TOAST_PAD;
    let x = (W - box_width) / 2;
    let y = H - TOAST_MARGIN - box_height;
    for row in y..y + box_height {
        fb[row * W + x..row * W + x + box_width].fill(TOAST_BG);
    }
    for (index, line) in toast.lines.iter().enumerate() {
        text::put_text(
            fb,
            W,
            H,
            x + TOAST_PAD,
            y + TOAST_PAD + index * text::CELL,
            line,
            TOAST_FG,
        );
    }
}

/// Record a failed save or load and put the same text on screen. The log line
/// stays the durable half; the toast is what a fullscreen player can see.
fn toast_error(toast: &mut Option<Toast>, message: &str) {
    report::log_error(message);
    *toast = Some(Toast::new(message, ERROR_TOAST));
}

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
fn frame_sleep(
    elapsed: ::std::time::Duration,
    budget: ::std::time::Duration,
) -> ::std::time::Duration {
    budget.checked_sub(elapsed).unwrap_or_default()
}

/// How long to wait before emulating the next frame, given how much audio the
/// device has not drained yet.
///
/// The device consumes 48 kHz exactly, so its queue is the emulator's real
/// clock and this is that clock's own period: wait for as many of its frames
/// as are queued above the target. Waiting on the frame budget instead runs the
/// emulator at a round 60 fps against NTSC's 60.0988, which starves a 48 kHz
/// queue by about 79 samples a second -- an audible click every few seconds.
///
/// Zero at or below the target: a queue that has drained needs the emulator to
/// run as fast as it can, not to wait. Capped at `budget`: a device that
/// accepts samples and never plays them must not slow the picture below what
/// the emulator does with no audio at all (what it holds past `AUDIO_MAX_QUEUED`
/// is dropped, so that queue cannot grow without bound either).
fn audio_sleep(queued: u32, budget: ::std::time::Duration) -> ::std::time::Duration {
    let excess = queued.saturating_sub(AUDIO_TARGET) as u64;
    let drain = ::std::time::Duration::from_micros(
        excess * 1_000_000 / (48_000 * AUDIO_FRAME_BYTES as u64),
    );
    drain.min(budget)
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
    let budget = frame_budget(bus.cart.region());
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
    let mut toast: Option<Toast> = None;
    let mut overlay = vec![0u32; W * H];
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
                Event::KeyDown {
                    scancode: Some(key),
                    keymod,
                    repeat: false,
                    ..
                } if slot_for(key).is_some() => {
                    let slot = slot_for(key).expect("the guard just matched a slot key");
                    if keymod.contains(Mod::LSHIFTMOD) {
                        match load_state_slot(
                            ::std::path::Path::new(&rom_path),
                            slot,
                            &mut bus,
                            &mut cpu,
                        ) {
                            Ok(()) => {
                                resume_after_load(&mut paused, muted, audio.as_ref());
                                toast =
                                    Some(Toast::new(&format!("Loaded slot {slot}"), CONFIRM_TOAST));
                            }
                            Err(e) => toast_error(&mut toast, &e),
                        }
                    } else {
                        match save_state_slot(::std::path::Path::new(&rom_path), slot, &bus, &cpu) {
                            Ok(()) => {
                                toast =
                                    Some(Toast::new(&format!("Saved slot {slot}"), CONFIRM_TOAST))
                            }
                            Err(e) => toast_error(&mut toast, &e),
                        }
                    }
                }
                Event::KeyDown {
                    keycode: Some(Keycode::F8),
                    repeat: false,
                    ..
                } => match load_latest_state(::std::path::Path::new(&rom_path), &mut bus, &mut cpu)
                {
                    Ok(slot) => {
                        resume_after_load(&mut paused, muted, audio.as_ref());
                        toast = Some(Toast::new(&format!("Loaded slot {slot}"), CONFIRM_TOAST));
                    }
                    Err(e) => toast_error(&mut toast, &e),
                },
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
                    if let Err(e) = write_atomic(path, bus.prg_ram()) {
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
        if toast.as_ref().is_some_and(|toast| !toast.visible()) {
            toast = None;
        }
        let frame = match toast.as_ref() {
            Some(toast) => {
                overlay.copy_from_slice(bus.ppu.framebuffer());
                draw_toast(&mut overlay, toast);
                &overlay[..]
            }
            None => bus.ppu.framebuffer(),
        };
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
            ::std::thread::sleep(budget);
        } else {
            match &audio {
                Some(queue) if !muted => ::std::thread::sleep(audio_sleep(queue.size(), budget)),
                _ => ::std::thread::sleep(frame_sleep(frame_start.elapsed(), budget)),
            }
        }
    }
    // A battery save is only useful if the process gets to exit normally, so
    // write it before the dump report can return an error.
    if let Some(path) = &save_path {
        if let Err(e) = write_atomic(path, bus.prg_ram()) {
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

/// Replace a file without ever leaving a half-written one in place. The bytes
/// go to a sibling temp file, are synced, and only then replace the target by
/// rename; a crash or a kill during the write leaves the previous file intact.
fn write_atomic(path: &::std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let mut temp = path.as_os_str().to_os_string();
    temp.push(".tmp");
    let temp = ::std::path::PathBuf::from(temp);
    let write = (|| {
        let mut file = ::std::fs::File::create(&temp)?;
        ::std::io::Write::write_all(&mut file, bytes)?;
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
        apply_event, audio_sleep, button_for, draw_toast, dump_list_ends_run, frame_budget,
        frame_sleep, latest_state_path, load_latest_state, load_state_slot, mask_for_name,
        missed_dumps, parse_script, read_latest_slot, resume_after_load, save_state_slot, slot_for,
        state_slot_path, wrap_text, write_atomic, Bus, Cartridge, Cpu, Play, Region, Toast,
        AUDIO_FRAME_BYTES, AUDIO_MAX_QUEUED, AUDIO_TARGET, FRAME_BUDGET, H, TOAST_BG, TOAST_FG, W,
    };
    use sdl2::event::{Event, WindowEvent};
    use sdl2::keyboard::{Keycode, Scancode};
    use std::time::Duration;

    #[test]
    fn a_battery_save_replaces_the_old_file_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("nes-save-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("game.sav");
        std::fs::write(&path, b"old save").expect("seed save");

        let ram = [0x5Au8; 8192];
        write_atomic(&path, &ram).expect("save must be written");

        assert_eq!(std::fs::read(&path).unwrap(), ram, "the new save landed");
        assert!(
            !path.with_extension("sav.tmp").exists(),
            "the temp file must be renamed away, not left behind"
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn quick_save_keys_are_the_nine_digits_and_nothing_else() {
        assert_eq!(slot_for(Scancode::Num1), Some(1));
        assert_eq!(slot_for(Scancode::Num9), Some(9));
        assert_eq!(slot_for(Scancode::Num0), None);
        assert_eq!(
            slot_for(Scancode::F8),
            None,
            "F8 is load-latest, not a slot"
        );
        assert_eq!(slot_for(Scancode::Z), None, "a game button is not a slot");
    }

    #[test]
    fn a_state_slot_is_a_sibling_of_the_rom() {
        assert_eq!(
            state_slot_path(std::path::Path::new("/roms/Zelda.nes"), 3),
            std::path::PathBuf::from("/roms/Zelda.state3")
        );
        assert_eq!(
            latest_state_path(std::path::Path::new("/roms/Zelda.nes")),
            std::path::PathBuf::from("/roms/Zelda.state")
        );
    }

    /// A 16 KiB NROM image whose CPU sits in a jump-to-self loop, enough to
    /// own a real machine without a game file.
    fn test_cartridge() -> Cartridge {
        let mut bytes = crate::rom::ines_header(1, 1, 0).to_vec();
        let mut prg = vec![0u8; 0x4000];
        prg[..3].copy_from_slice(&[0x4C, 0x00, 0x80]); // JMP $8000
        prg[0x3FFA..0x4000].copy_from_slice(&[0x00, 0x81, 0x00, 0x80, 0x00, 0x80]);
        bytes.extend(prg);
        bytes.extend((0..0x2000).map(|i| i as u8));
        Cartridge::load(&bytes).expect("synthetic NROM image")
    }

    #[test]
    fn a_saved_slot_reloads_into_the_running_machine() {
        let dir = std::env::temp_dir().join(format!("nes-state-slot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let rom_path = dir.join("game.nes");
        let mut bus = Bus::new(test_cartridge());
        let mut cpu = Cpu::new();
        bus.power_on();
        cpu.reset(&mut bus);

        let saved = bus.save_state(&cpu);
        save_state_slot(&rom_path, 3, &bus, &cpu).expect("state must be written");
        assert_eq!(
            read_latest_slot(&rom_path).expect("index readable"),
            Some(3),
            "a save records itself for F8"
        );

        bus.run_frame(&mut cpu);
        bus.ppu.take_frame();
        assert_ne!(bus.save_state(&cpu), saved, "the machine must move on");

        load_state_slot(&rom_path, 3, &mut bus, &mut cpu).expect("state must load");
        assert_eq!(
            bus.save_state(&cpu),
            saved,
            "the loaded machine differs from the saved one"
        );
        assert_eq!(
            load_latest_state(&rom_path, &mut bus, &mut cpu),
            Ok(3),
            "the confirmation must name the slot F8 chose"
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// A load replaces the machine, including the frame that was on screen
    /// when the loop paused. Staying paused would keep showing that old frame.
    #[test]
    fn a_loaded_state_unpauses_the_frame_loop() {
        let mut paused = true;
        resume_after_load(&mut paused, false, None);
        assert!(!paused, "a load must render the state it just restored");
    }

    #[test]
    fn the_latest_slot_comes_from_the_index_not_file_times() {
        let dir = std::env::temp_dir().join(format!("nes-latest-slot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let rom_path = dir.join("game.nes");
        let index = latest_state_path(&rom_path);

        assert_eq!(read_latest_slot(&rom_path), Ok(None), "nothing saved yet");

        std::fs::write(&index, b"2\n").expect("index written");
        assert_eq!(read_latest_slot(&rom_path), Ok(Some(2)));

        std::fs::write(&index, b"0\n").expect("index written");
        assert!(
            read_latest_slot(&rom_path).unwrap_err().contains("1-9"),
            "an out-of-range slot names the valid range"
        );

        std::fs::write(&index, b"nine\n").expect("index written");
        assert!(
            read_latest_slot(&rom_path).unwrap_err().contains("nine"),
            "unreadable text is quoted back"
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
        let budget = frame_budget(Region::Ntsc);
        assert_eq!(
            audio_sleep(0, budget),
            Duration::ZERO,
            "empty queue: run flat out"
        );
        assert_eq!(
            audio_sleep(AUDIO_TARGET, budget),
            Duration::ZERO,
            "at the target: run"
        );

        // One NTSC frame of audio: 48000 / 60.0988 = 798.7 stereo frames.
        let frame = (48_000.0 / 60.0988 * AUDIO_FRAME_BYTES as f64) as u32;
        let wait = audio_sleep(AUDIO_TARGET + frame, budget);
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
        assert!(audio_sleep(AUDIO_TARGET + 2 * frame, budget) > wait);
        for deep in [AUDIO_MAX_QUEUED, u32::MAX] {
            assert!(
                audio_sleep(deep, budget) <= budget,
                "{deep} bytes queued waits longer than a frame"
            );
        }
    }

    /// PAL is a 50 Hz console: one 312-line frame is 19,997.6 us, and the
    /// wall clock gets 20 ms so a run without audio is paced at 50 fps.
    #[test]
    fn a_pal_frame_gets_a_twenty_millisecond_budget() {
        assert_eq!(frame_budget(Region::Ntsc), FRAME_BUDGET);
        assert_eq!(frame_budget(Region::Pal), Duration::from_micros(20_000));
    }

    #[test]
    fn a_sleep_fills_out_the_frame_budget_and_never_goes_negative() {
        let budget = frame_budget(Region::Ntsc);
        assert_eq!(
            frame_sleep(Duration::ZERO, budget),
            FRAME_BUDGET,
            "free frame"
        );
        assert_eq!(
            frame_sleep(budget / 2, budget),
            budget / 2,
            "half a frame used, half to sleep"
        );
        assert_eq!(
            frame_sleep(budget, budget),
            Duration::ZERO,
            "exactly on budget, no sleep"
        );
        // Behind: drop the debt instead of chasing it, and certainly do not
        // wrap around into an enormous sleep.
        for over in [
            budget * 3,
            Duration::from_secs(5),
            Duration::MAX,
            Duration::from_nanos(u64::MAX),
        ] {
            assert_eq!(
                frame_sleep(over, budget),
                Duration::ZERO,
                "{over:?} overran"
            );
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

    /// A status message must fit the 256-pixel screen, so it wraps before it
    /// is drawn. A long unbroken word -- a path or a filename -- is cut at the
    /// edge instead of running off it.
    #[test]
    fn a_status_message_wraps_to_the_screen_width() {
        assert_eq!(wrap_text("Saved slot 3", 28), ["Saved slot 3"]);
        assert_eq!(
            wrap_text("state slot 9 is empty: /roms/Legend of Zelda.state9", 28),
            ["state slot 9 is empty:", "/roms/Legend of Zelda.state9"]
        );
        let long = "x".repeat(60);
        let lines = wrap_text(&long, 28);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.chars().count())
                .collect::<Vec<_>>(),
            [28, 28, 4],
            "a word longer than the screen is cut, not dropped"
        );
        assert_eq!(lines.concat(), long, "cutting loses no characters");
    }

    /// The confirmation is brief: a zero lifetime is already over, a real one
    /// is still on screen.
    #[test]
    fn a_status_message_expires() {
        assert!(!Toast::new("Saved slot 3", Duration::ZERO).visible());
        assert!(Toast::new("Saved slot 3", Duration::from_secs(2)).visible());
    }

    /// The box has a fixed height. A cut-off message must say so rather than
    /// looking complete, and point at the log that kept the rest.
    #[test]
    fn a_long_status_message_says_where_the_rest_is() {
        let message = "word ".repeat(80);
        let toast = Toast::new(&message, Duration::from_secs(6));
        assert_eq!(toast.lines.len(), 10);
        assert_eq!(toast.lines.last().unwrap(), "... see nes-errors.log");
    }

    /// A path longer than the screen still starts on a fresh line instead of
    /// filling whatever room the preceding words left.
    #[test]
    fn a_word_too_long_for_the_line_starts_a_fresh_one() {
        let long = "x".repeat(40);
        assert_eq!(
            wrap_text(&format!("load {long}"), 28),
            ["load".to_string(), "x".repeat(28), "x".repeat(12)]
        );
    }

    /// The overlay owns its box and leaves the rest of the frame alone.
    #[test]
    fn a_status_message_paints_a_box_over_the_frame() {
        let mut fb = vec![0u32; W * H];
        let toast = Toast::new("Saved slot 3", Duration::from_secs(2));
        draw_toast(&mut fb, &toast);
        assert_eq!(fb[0], 0, "the top-left corner is untouched");
        assert!(fb.contains(&TOAST_BG), "the box background is drawn");
        assert!(fb.contains(&TOAST_FG), "the message text is drawn");
    }
}
