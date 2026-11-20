use sdl2::controller::{Button, GameController};
use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::{Keycode, Mod, Scancode};

mod apu;
#[cfg(test)]
mod apu_rom_test;
mod bus;
mod controller;
mod cpu;
#[cfg(test)]
mod golden_frame_test;
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

const USAGE: &str = "usage: nes <rom.nes|rom.zip> [--dump N[,N...]] [--play SCRIPT]\n\
    \x20 <rom>              the cartridge to boot, quoted if it has spaces; a\n\
    \x20                    .zip holding one .nes is read without unpacking\n\
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

/// Player 2's pad, in the same report order, on the left of the keyboard so
/// two people can share it. No key is on player 1 or the hotkey list.
const P2_BUTTONS: [(Keycode, u8); 8] = [
    (Keycode::G, 0x01),
    (Keycode::H, 0x02),
    (Keycode::T, 0x04),
    (Keycode::Y, 0x08),
    (Keycode::W, 0x10),
    (Keycode::S, 0x20),
    (Keycode::A, 0x40),
    (Keycode::D, 0x80),
];

/// The optional per-game player count, beside the executable. A missing file
/// or a game with no line is two players, so two-player input works without
/// any setup.
const PLAYERS_CONFIG: &str = "nes-players.conf";
const DEFAULT_PLAYERS: u8 = 2;

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

/// The folder holding every game's states: `saves` beside the executable, the
/// same place as the config file and the error log. Without an executable path
/// it falls back to `saves` in the current directory.
fn saves_root() -> ::std::path::PathBuf {
    report::exe_dir().unwrap_or_default().join("saves")
}

/// One game's folder under `saves_root`. The extension is dropped, so
/// `Zelda.nes` and `Zelda.zip` are the same game and share
/// `saves/Zelda/`.
fn game_save_dir(
    saves_root: &::std::path::Path,
    rom_path: &::std::path::Path,
) -> ::std::path::PathBuf {
    saves_root.join(rom_path.file_stem().unwrap_or_default())
}

/// Move states written beside the ROM by an older build into the game's
/// folder, so an upgrade does not make saved games look lost. Runs only when
/// the folder does not exist yet. Each file is copied before the old one is
/// removed, and a failure is logged rather than stopping play.
fn migrate_legacy_states(rom_path: &::std::path::Path, save_dir: &::std::path::Path) {
    if save_dir.exists() {
        return;
    }
    let states = (1..=SLOT_KEYS.len())
        .map(|slot| {
            (
                rom_path.with_extension(format!("state{slot}")),
                state_slot_path(save_dir, slot),
            )
        })
        .chain([(
            rom_path.with_extension("state"),
            latest_state_path(save_dir),
        )]);
    for (old, new) in states.filter(|(old, _)| old.exists()) {
        if let Err(e) = ::std::fs::create_dir_all(save_dir)
            .and_then(|()| ::std::fs::copy(&old, &new))
            .and_then(|_| ::std::fs::remove_file(&old))
        {
            report::log_error(&format!("cannot move {}: {e}", old.display()));
        }
    }
}

/// Where slot `slot` lives inside a game's folder: `saves/Zelda/state3`.
fn state_slot_path(save_dir: &::std::path::Path, slot: usize) -> ::std::path::PathBuf {
    save_dir.join(format!("state{slot}"))
}

/// The one-byte index naming the slot `F8` loads. Kept beside the states, and
/// written after every successful save, so "latest" does not depend on file
/// times -- a copied or restored save folder would otherwise reorder itself.
fn latest_state_path(save_dir: &::std::path::Path) -> ::std::path::PathBuf {
    save_dir.join("latest")
}

/// The slot named by the index file. Missing is `None` (nothing saved yet);
/// an unreadable, unparsable or out-of-range index is an error, because guessing
/// would load a state the player did not ask for.
fn read_latest_slot(save_dir: &::std::path::Path) -> Result<Option<usize>, String> {
    let path = latest_state_path(save_dir);
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
    save_dir: &::std::path::Path,
    slot: usize,
    bus: &Bus,
    cpu: &Cpu,
) -> Result<(), String> {
    let path = state_slot_path(save_dir, slot);
    ::std::fs::create_dir_all(save_dir)
        .map_err(|e| format!("cannot create {}: {e}", save_dir.display()))?;
    write_atomic(&path, &bus.save_state(cpu))
        .map_err(|e| format!("cannot save state slot {slot}: {e}"))?;
    write_atomic(&latest_state_path(save_dir), format!("{slot}\n").as_bytes())
        .map_err(|e| format!("slot {slot} saved, but cannot record it for F8: {e}"))
}

/// Apply a slot to a running machine. `Bus::load_state` validates and swaps the
/// whole machine, so a missing, corrupt or foreign state leaves play untouched.
fn load_state_slot(
    save_dir: &::std::path::Path,
    slot: usize,
    bus: &mut Bus,
    cpu: &mut Cpu,
) -> Result<(), String> {
    let path = state_slot_path(save_dir, slot);
    let data = ::std::fs::read(&path).map_err(|e| match e.kind() {
        ::std::io::ErrorKind::NotFound => format!("state slot {slot} is empty: {}", path.display()),
        _ => format!("cannot read {}: {e}", path.display()),
    })?;
    bus.load_state(cpu, &data)
        .map_err(|e| format!("cannot load {}: {e}", path.display()))
}

fn load_latest_state(
    save_dir: &::std::path::Path,
    bus: &mut Bus,
    cpu: &mut Cpu,
) -> Result<usize, String> {
    let slot = read_latest_slot(save_dir)?.ok_or_else(|| {
        format!(
            "no save states in {}: press 1-9 to make one",
            save_dir.display()
        )
    })?;
    load_state_slot(save_dir, slot, bus, cpu).map(|()| slot)
}

/// One quick-save slot as the picker sees it. The age is relative: the
/// question the list answers is "which of these is the run I want?".
#[derive(Debug, PartialEq, Eq)]
enum Slot {
    Empty,
    Ready(String),
    Incompatible,
}

/// What the in-game slot list shows. Opening it reads each state once and
/// checks it against the running cartridge; the result is fixed until the list
/// is reopened, so entries cannot change under the selection.
struct Picker {
    slots: [Slot; SLOT_KEYS.len()],
    selected: usize,
}

impl Picker {
    fn open(save_dir: &::std::path::Path, cart: &Cartridge) -> Picker {
        let slots = ::std::array::from_fn(|index| slot_state(save_dir, index + 1, cart));
        // The slot the player saved last is the one they are most likely to
        // want; an index naming a broken or foreign file falls back to slot 1.
        let selected = read_latest_slot(save_dir)
            .ok()
            .flatten()
            .filter(|&slot| matches!(&slots[slot - 1], Slot::Ready(_)))
            .map_or(0, |slot| slot - 1);
        Picker { slots, selected }
    }

    fn lines(&self) -> Vec<String> {
        let mut lines = vec!["SAVE STATES".to_string()];
        for (index, slot) in self.slots.iter().enumerate() {
            lines.push(slot_line(index + 1, index == self.selected, slot));
        }
        lines.push("UP/DOWN choose".to_string());
        lines.push("ENTER load  ESC cancel".to_string());
        lines
    }

    fn select(&mut self, delta: isize) {
        self.selected = move_selection(self.selected, delta);
    }
}

/// Read one slot's state file and decide whether this cartridge can load it.
/// Missing is empty; any other read failure is incompatible, because the picker
/// must not offer a slot it cannot even read.
fn slot_state(save_dir: &::std::path::Path, slot: usize, cart: &Cartridge) -> Slot {
    let path = state_slot_path(save_dir, slot);
    let data = match ::std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == ::std::io::ErrorKind::NotFound => return Slot::Empty,
        Err(_) => return Slot::Incompatible,
    };
    if crate::state::unwrap(&data, cart.mapper_id(), cart.region(), cart.fingerprint()).is_err() {
        return Slot::Incompatible;
    }
    let age = ::std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .map(age_label)
        .unwrap_or_else(|| "unknown".to_string());
    Slot::Ready(age)
}

/// Relative age in the largest unit that still says something useful.
fn age_label(elapsed: ::std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// One row of the picker. The marker sits in the first column so the selected
/// row stays obvious against the frame behind the box.
fn slot_line(slot: usize, selected: bool, state: &Slot) -> String {
    let marker = if selected { '>' } else { ' ' };
    let detail = match state {
        Slot::Empty => "empty".to_string(),
        Slot::Ready(age) => format!("saved {age}"),
        Slot::Incompatible => "incompatible".to_string(),
    };
    format!("{marker}{slot}  {detail}")
}

/// Move the selection by one row, wrapping at both ends.
fn move_selection(selected: usize, delta: isize) -> usize {
    let count = SLOT_KEYS.len() as isize;
    (selected as isize + delta).rem_euclid(count) as usize
}

/// What the modal slot list does with an event. Kept apart from the SDL and
/// audio side effects so the key map can be checked without a window.
#[derive(Debug, PartialEq, Eq)]
enum PickerCommand {
    Cancel,
    Move(isize),
    Load,
    ForgetInput,
    Quit,
    Ignore,
}

fn picker_command(event: &Event) -> PickerCommand {
    match event {
        Event::Quit { .. } => PickerCommand::Quit,
        Event::KeyDown {
            keycode: Some(Keycode::Escape),
            repeat: false,
            ..
        } => PickerCommand::Cancel,
        Event::KeyDown {
            keycode: Some(Keycode::Up),
            repeat: false,
            ..
        } => PickerCommand::Move(-1),
        Event::KeyDown {
            keycode: Some(Keycode::Down),
            repeat: false,
            ..
        } => PickerCommand::Move(1),
        Event::KeyDown {
            keycode: Some(Keycode::Return),
            repeat: false,
            ..
        } => PickerCommand::Load,
        Event::Window {
            win_event: WindowEvent::FocusLost,
            ..
        } => PickerCommand::ForgetInput,
        _ => PickerCommand::Ignore,
    }
}

/// The device plays only while the game runs at normal speed and the player has
/// not muted it. Pausing, muting and fast-forward all keep it silent.
fn audio_should_play(paused: bool, muted: bool, turbo: bool) -> bool {
    !paused && !muted && !turbo
}

/// A load replaces the machine's timeline: the frame the loop paused on belongs
/// to the old one, and the queued samples are noise over the loaded state. Drop
/// them and unpause so the next iteration renders what was restored; the
/// per-frame audio sync restarts the queue under the usual conditions. Only the
/// game buttons belong to the old run; a held Tab is host state and survives.
fn finish_load(paused: &mut bool, audio: Option<&sdl2::audio::AudioQueue<i16>>, input: &mut Input) {
    *paused = false;
    if let Some(queue) = audio {
        queue.clear();
    }
    input.forget_buttons();
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

/// Paint a centered box of text at `y`, owned by whichever overlay called it.
/// The callers bound their line width and count, so the box always fits.
fn draw_panel(fb: &mut [u32], lines: &[String], y: usize) {
    let Some(text_width) = lines.iter().map(|line| line.chars().count()).max() else {
        return;
    };
    let box_width = text_width * text::CELL + 2 * TOAST_PAD;
    let box_height = lines.len() * text::CELL + 2 * TOAST_PAD;
    let x = (W - box_width) / 2;
    for row in y..y + box_height {
        fb[row * W + x..row * W + x + box_width].fill(TOAST_BG);
    }
    for (index, line) in lines.iter().enumerate() {
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

/// The toast sits at the bottom so it covers as little of the game as possible.
fn draw_toast(fb: &mut [u32], toast: &Toast) {
    let box_height = toast.lines.len() * text::CELL + 2 * TOAST_PAD;
    draw_panel(fb, &toast.lines, H - TOAST_MARGIN - box_height);
}

/// The picker sits at the top, leaving the toast its own bottom space when a
/// load fails while the list is open.
fn draw_picker(fb: &mut [u32], picker: &Picker) {
    draw_panel(fb, &picker.lines(), TOAST_MARGIN);
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

/// Player 2's half of the same table.
fn p2_button_for(key: Keycode) -> Option<u8> {
    P2_BUTTONS.iter().find(|row| row.0 == key).map(|row| row.1)
}

/// The player count for `game` from the config text. Lines are
/// `<game> = <1|2>`; blank lines, `#` comments, and lines without `=` are
/// ignored, and the last matching line wins. A matching line with a bad value
/// is an error.
fn players_for_game(text: &str, game: &str) -> Result<Option<u8>, String> {
    let mut players = None;
    for (index, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, count)) = line.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case(game) {
            continue;
        }
        let count = count.trim();
        let count: u8 = count
            .parse()
            .map_err(|_| format!("line {}: {count:?} is not 1 or 2", index + 1))?;
        if !(1..=2).contains(&count) {
            return Err(format!("line {}: {count} is not 1 or 2", index + 1));
        }
        players = Some(count);
    }
    Ok(players)
}

/// The player count in `path` for `game`. A missing file or a game with no
/// line is `None`; an unreadable file or a broken line is an error naming the
/// file. No logging here, so the caller decides what a broken config costs.
fn players_from_file(path: &::std::path::Path, game: &str) -> Result<Option<u8>, String> {
    let text = match ::std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ::std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    players_for_game(&text, game).map_err(|e| format!("{}: {e}", path.display()))
}

/// The configured player count for `rom`, or two when there is no override.
/// A broken or unreadable config is logged and ignored: it must not stop a
/// game from booting.
fn load_players(rom: &::std::path::Path) -> u8 {
    let game = rom.file_stem().unwrap_or_default().to_string_lossy();
    let path = match report::exe_dir() {
        Ok(dir) => dir.join(PLAYERS_CONFIG),
        Err(e) => {
            report::log_error(&format!(
                "cannot locate the executable directory for {PLAYERS_CONFIG}: {e}"
            ));
            return DEFAULT_PLAYERS;
        }
    };
    match players_from_file(&path, &game) {
        Ok(Some(players)) => players,
        Ok(None) => DEFAULT_PLAYERS,
        Err(e) => {
            report::log_error(&e);
            DEFAULT_PLAYERS
        }
    }
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

/// Held input, one mask per source and player. Keyboard and gamepad stay
/// apart so releasing one source cannot clear a button the other still holds.
/// `pad_ids` names the game controller in each player's slot, so a button
/// event from the second pad reaches the second port.
#[derive(Default)]
struct Input {
    keys: [u8; 2],
    pads: [u8; 2],
    pad_ids: [Option<u32>; 2],
    /// Fast-forward is held, not toggled: release the key and normal speed
    /// returns. It lives here so a focus loss forgets it with the buttons.
    turbo: bool,
}

impl Input {
    /// Fold one SDL event into the held masks, returning whether the run
    /// continues. `false` quits.
    fn apply_event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Quit { .. } => false,
            Event::KeyDown {
                keycode: Some(Keycode::Escape),
                ..
            } => false,
            Event::KeyDown {
                keycode: Some(Keycode::Tab),
                ..
            } => {
                self.turbo = true;
                true
            }
            Event::KeyUp {
                keycode: Some(Keycode::Tab),
                ..
            } => {
                self.turbo = false;
                true
            }
            Event::KeyDown {
                keycode: Some(key), ..
            } => {
                if let Some(bit) = button_for(key) {
                    self.keys[0] |= bit;
                }
                if let Some(bit) = p2_button_for(key) {
                    self.keys[1] |= bit;
                }
                true
            }
            Event::KeyUp {
                keycode: Some(key), ..
            } => {
                if let Some(bit) = button_for(key) {
                    self.keys[0] &= !bit;
                }
                if let Some(bit) = p2_button_for(key) {
                    self.keys[1] &= !bit;
                }
                true
            }
            Event::ControllerButtonDown { which, button, .. } => {
                if let Some(slot) = self.pad_slot(which) {
                    if let Some(bit) = pad_bit(button) {
                        self.pads[slot] |= bit;
                    }
                }
                true
            }
            Event::ControllerButtonUp { which, button, .. } => {
                if let Some(slot) = self.pad_slot(which) {
                    if let Some(bit) = pad_bit(button) {
                        self.pads[slot] &= !bit;
                    }
                }
                true
            }
            // No KeyUp ever arrives for a key released while another window
            // has the focus, so alt-tabbing away holding Right would latch it
            // on for good. Forget the held masks, not the pad assignment.
            Event::Window {
                win_event: WindowEvent::FocusLost,
                ..
            } => {
                self.forget();
                true
            }
            _ => true,
        }
    }

    /// The controller masks to hand the bus. A one-player game folds both
    /// keyboard sets and both pads into controller 1, so any input plays it.
    fn controller_masks(&self, players: u8) -> [u8; 2] {
        let one = self.keys[0] | self.pads[0];
        let two = self.keys[1] | self.pads[1];
        if players == 1 {
            [one | two, 0]
        } else {
            [one, two]
        }
    }

    /// Put a newly opened pad in the first free player slot. The same pad is
    /// never opened twice, and a third pad has no slot.
    fn add_pad(&mut self, id: u32) -> Option<usize> {
        if self.pad_ids.contains(&Some(id)) {
            return None;
        }
        let slot = self.pad_ids.iter().position(Option::is_none)?;
        self.pad_ids[slot] = Some(id);
        Some(slot)
    }

    /// Unplug the pad with instance `id`, releasing anything it held.
    fn remove_pad(&mut self, id: u32) -> Option<usize> {
        let slot = self.pad_slot(id)?;
        self.pad_ids[slot] = None;
        self.pads[slot] = 0;
        Some(slot)
    }

    fn pad_slot(&self, id: u32) -> Option<usize> {
        self.pad_ids.iter().position(|&slot| slot == Some(id))
    }

    /// Drop every held game button without touching the host-side holds or the
    /// pad assignment.
    fn forget_buttons(&mut self) {
        self.keys = [0; 2];
        self.pads = [0; 2];
    }

    /// Drop every held button and host hold without forgetting which pad is in
    /// which slot.
    fn forget(&mut self) {
        self.forget_buttons();
        self.turbo = false;
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

/// Whether the loop waits out the frame period. Fast-forward is the only
/// unpaused state that does not; a paused game still waits so the window stays
/// responsive while its frame is frozen.
fn waits_after_frame(paused: bool, turbo: bool) -> bool {
    paused || !turbo
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
    // States are keyed by game, so moving the ROM does not lose them.
    let rom = ::std::path::Path::new(&rom_path);
    let save_dir = game_save_dir(&saves_root(), rom);
    migrate_legacy_states(rom, &save_dir);
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

    let players = load_players(rom);
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

    let bytes = rom::read_rom(::std::path::Path::new(&rom_path))?;
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

    let mut audio_playing = false;
    if let Some(queue) = &audio {
        // The queue is opened with no allowed changes, so a device that cannot
        // do exactly this spec fails the open instead of renegotiating it; the
        // mixer follows whatever rate the device did agree to.
        bus.apu.set_sample_rate(queue.spec().freq as u32);
        queue.resume();
        audio_playing = true;
    }

    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;
    // Game controllers are optional and hot-pluggable. The handles must stay
    // alive for their button events to keep arriving, so they live in this
    // array for the whole run even though nothing reads it directly.
    let controller_subsystem = sdl.game_controller().ok();
    let mut input = Input::default();
    let mut pad_handles: [Option<GameController>; 2] = [None, None];
    if let Some(controllers) = &controller_subsystem {
        for index in 0..controllers.num_joysticks().unwrap_or(0) {
            if controllers.is_game_controller(index) {
                if let Ok(gamepad) = controllers.open(index) {
                    let id = gamepad.instance_id();
                    if let Some(slot) = input.add_pad(id) {
                        pad_handles[slot] = Some(gamepad);
                    }
                }
            }
        }
    }
    let mut play = script.map(Play::new);
    let mut paused = false;
    let mut muted = false;
    let mut fullscreen = false;
    let mut running = true;
    let mut toast: Option<Toast> = None;
    let mut picker: Option<Picker> = None;
    let mut picker_was_paused = false;
    let mut overlay = vec![0u32; W * H];
    let mut frames = 0u32;
    while running {
        // Started before anything else in the frame so the emulation, the
        // upload and the sleep are all inside the measured budget.
        let frame_start = ::std::time::Instant::now();
        for event in events.poll_iter() {
            // A controller can arrive or leave at any time, including while
            // the modal list owns the input; hotplug must not be lost to the
            // picker.
            if let Event::ControllerDeviceAdded { which, .. } = &event {
                if let Some(controllers) = &controller_subsystem {
                    if let Ok(gamepad) = controllers.open(*which) {
                        let id = gamepad.instance_id();
                        if let Some(slot) = input.add_pad(id) {
                            pad_handles[slot] = Some(gamepad);
                        }
                    }
                }
                continue;
            }
            if let Event::ControllerDeviceRemoved { which, .. } = &event {
                if let Some(slot) = input.remove_pad(*which) {
                    pad_handles[slot] = None;
                }
                continue;
            }
            // The picker is modal: it owns the keys while it is open, so a
            // digit cannot save over a slot the player is choosing and Escape
            // cancels the list instead of quitting the game.
            if picker.is_some() {
                match picker_command(&event) {
                    PickerCommand::Quit => running = false,
                    PickerCommand::Cancel => {
                        picker = None;
                        paused = picker_was_paused;
                        input.forget();
                    }
                    PickerCommand::Move(delta) => {
                        if let Some(open) = picker.as_mut() {
                            open.select(delta);
                        }
                    }
                    PickerCommand::Load => {
                        let slot = picker.as_ref().expect("the guard checked").selected + 1;
                        match load_state_slot(&save_dir, slot, &mut bus, &mut cpu) {
                            Ok(()) => {
                                picker = None;
                                // A load replaces the paused frame with the
                                // restored one, so the game resumes either way.
                                finish_load(&mut paused, audio.as_ref(), &mut input);
                                toast =
                                    Some(Toast::new(&format!("Loaded slot {slot}"), CONFIRM_TOAST));
                            }
                            Err(e) => toast_error(&mut toast, &e),
                        }
                    }
                    PickerCommand::ForgetInput => input.forget(),
                    PickerCommand::Ignore => {}
                }
                continue;
            }
            match event {
                Event::KeyDown {
                    keycode: Some(Keycode::P),
                    repeat: false,
                    ..
                } => {
                    paused = !paused;
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
                    if muted {
                        if let Some(queue) = &audio {
                            queue.clear();
                        }
                    }
                }
                Event::KeyDown {
                    keycode: Some(Keycode::F7),
                    repeat: false,
                    ..
                } => {
                    picker_was_paused = paused;
                    picker = Some(Picker::open(&save_dir, &bus.cart));
                    input.forget();
                    paused = true;
                }
                Event::KeyDown {
                    scancode: Some(key),
                    keymod,
                    repeat: false,
                    ..
                } if slot_for(key).is_some() => {
                    let slot = slot_for(key).expect("the guard just matched a slot key");
                    if keymod.contains(Mod::LSHIFTMOD) {
                        match load_state_slot(&save_dir, slot, &mut bus, &mut cpu) {
                            Ok(()) => {
                                finish_load(&mut paused, audio.as_ref(), &mut input);
                                toast =
                                    Some(Toast::new(&format!("Loaded slot {slot}"), CONFIRM_TOAST));
                            }
                            Err(e) => toast_error(&mut toast, &e),
                        }
                    } else {
                        match save_state_slot(&save_dir, slot, &bus, &cpu) {
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
                } => match load_latest_state(&save_dir, &mut bus, &mut cpu) {
                    Ok(slot) => {
                        finish_load(&mut paused, audio.as_ref(), &mut input);
                        toast = Some(Toast::new(&format!("Loaded slot {slot}"), CONFIRM_TOAST));
                    }
                    Err(e) => toast_error(&mut toast, &e),
                },
                other => {
                    if !input.apply_event(other) {
                        running = false;
                    }
                }
            }
        }
        // Pause, mute and fast-forward all stop the device; only this sync
        // touches the queue, so a hotkey handled anywhere above cannot leave it
        // playing when the game is silent. Fast-forward also drops the backlog
        // it inherited, or releasing the key would play stale sound.
        let should_play = audio_should_play(paused, muted, input.turbo);
        if should_play != audio_playing {
            if let Some(queue) = &audio {
                if should_play {
                    queue.resume();
                } else {
                    queue.pause();
                    if input.turbo {
                        queue.clear();
                    }
                }
            }
            audio_playing = should_play;
        }
        // A paused machine keeps its frame on screen and its script on hold;
        // time does not advance for either.
        if !paused {
            // A script is not a second input path: it assigns the same mask a
            // key press assigns, and it goes through the same `set_buttons`
            // calls below. It drives player 1 only.
            let mut masks = input.controller_masks(players);
            if let Some(script) = play.as_mut() {
                match script.mask_for_next_frame() {
                    Some(mask) => masks[0] = mask,
                    // The script is over. Release what its last step held instead
                    // of emulating one more frame with it still down -- the frame
                    // that `running = false` still runs would be played with a key
                    // that the script already let go of.
                    None => {
                        masks = [0, 0];
                        running = false;
                    }
                }
            }
            bus.controllers[0].set_buttons(masks[0]);
            bus.controllers[1].set_buttons(masks[1]);
            // Run the CPU until the PPU has a whole frame, taking the NMI the
            // vblank flag raises and the APU's frame IRQ off its level line. SMB
            // gates its status bar on a sprite-0 hit and its NMI handler on
            // `$2002`, so this is where the machine actually lives.
            bus.run_frame(&mut cpu);
            if let Some(queue) = &audio {
                let samples = bus.apu.take_samples();
                // Muted and fast-forward still drain the APU's queue, or the mix
                // would grow without bound; the samples are simply not handed to
                // the device.
                if !muted && !input.turbo && !samples.is_empty() {
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
        let frame = if picker.is_some() || toast.is_some() {
            overlay.copy_from_slice(bus.ppu.framebuffer());
            if let Some(picker) = picker.as_ref() {
                draw_picker(&mut overlay, picker);
            }
            if let Some(toast) = toast.as_ref() {
                draw_toast(&mut overlay, toast);
            }
            &overlay[..]
        } else {
            bus.ppu.framebuffer()
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
        // wall clock and keeps the window responsive. Fast-forward waits for
        // neither: the hold exists to run the machine as fast as it can.
        if waits_after_frame(paused, input.turbo) {
            if paused {
                ::std::thread::sleep(budget);
            } else {
                match &audio {
                    Some(queue) if !muted => {
                        ::std::thread::sleep(audio_sleep(queue.size(), budget))
                    }
                    _ => ::std::thread::sleep(frame_sleep(frame_start.elapsed(), budget)),
                }
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
        age_label, audio_should_play, audio_sleep, button_for, draw_toast, dump_list_ends_run,
        finish_load, frame_budget, frame_sleep, game_save_dir, latest_state_path,
        load_latest_state, load_state_slot, mask_for_name, migrate_legacy_states, missed_dumps,
        move_selection, p2_button_for, pad_bit, parse_script, picker_command, players_for_game,
        players_from_file, read_latest_slot, save_state_slot, slot_for, slot_line, state_slot_path,
        waits_after_frame, wrap_text, write_atomic, Bus, Cartridge, Cpu, Input, Picker,
        PickerCommand, Play, Region, Slot, Toast, AUDIO_FRAME_BYTES, AUDIO_MAX_QUEUED,
        AUDIO_TARGET, FRAME_BUDGET, H, P2_BUTTONS, TOAST_BG, TOAST_FG, W,
    };
    use sdl2::controller::Button;
    use sdl2::event::{Event, WindowEvent};
    use sdl2::keyboard::{Keycode, Scancode};
    use std::time::Duration;

    fn key_event(key: Keycode, down: bool) -> Event {
        let timestamp = u32::from(!down);
        if down {
            Event::KeyDown {
                timestamp,
                window_id: 0,
                keycode: Some(key),
                scancode: None,
                keymod: sdl2::keyboard::Mod::empty(),
                repeat: false,
            }
        } else {
            Event::KeyUp {
                timestamp,
                window_id: 0,
                keycode: Some(key),
                scancode: None,
                keymod: sdl2::keyboard::Mod::empty(),
                repeat: false,
            }
        }
    }

    fn pad_event(which: u32, button: Button, down: bool) -> Event {
        if down {
            Event::ControllerButtonDown {
                timestamp: 0,
                which,
                button,
            }
        } else {
            Event::ControllerButtonUp {
                timestamp: 1,
                which,
                button,
            }
        }
    }

    fn focus_lost() -> Event {
        Event::Window {
            timestamp: 0,
            window_id: 0,
            win_event: WindowEvent::FocusLost,
        }
    }

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
    fn a_state_slot_lives_in_a_folder_per_game() {
        let root = std::path::Path::new("/saves");
        let rom = std::path::Path::new("/roms/Zelda.nes");
        let save_dir = game_save_dir(root, rom);
        assert_eq!(save_dir, std::path::PathBuf::from("/saves/Zelda"));
        assert_eq!(
            state_slot_path(&save_dir, 3),
            std::path::PathBuf::from("/saves/Zelda/state3")
        );
        assert_eq!(
            latest_state_path(&save_dir),
            std::path::PathBuf::from("/saves/Zelda/latest")
        );
        assert_eq!(
            game_save_dir(root, std::path::Path::new("/roms/Zelda.zip")),
            save_dir,
            "the same game as a zip shares its states"
        );
    }

    #[test]
    fn legacy_states_move_into_the_game_folder() {
        let dir =
            std::env::temp_dir().join(format!("nes-legacy-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let rom_path = dir.join("game.nes");
        let save_dir = dir.join("saves").join("game");
        std::fs::write(rom_path.with_extension("state3"), b"slot 3").expect("legacy slot");
        std::fs::write(rom_path.with_extension("state"), b"3\n").expect("legacy index");

        migrate_legacy_states(&rom_path, &save_dir);

        assert_eq!(
            std::fs::read(state_slot_path(&save_dir, 3)).unwrap(),
            b"slot 3"
        );
        assert_eq!(std::fs::read(latest_state_path(&save_dir)).unwrap(), b"3\n");
        assert!(
            !rom_path.with_extension("state3").exists(),
            "the old slot is moved, not left behind"
        );
        assert!(!rom_path.with_extension("state").exists());
        std::fs::remove_dir_all(&dir).expect("cleanup");
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
        let save_dir = dir.join("saves").join("game");
        let mut bus = Bus::new(test_cartridge());
        let mut cpu = Cpu::new();
        bus.power_on();
        cpu.reset(&mut bus);

        let saved = bus.save_state(&cpu);
        save_state_slot(&save_dir, 3, &bus, &cpu).expect("state must be written");
        assert_eq!(
            read_latest_slot(&save_dir).expect("index readable"),
            Some(3),
            "a save records itself for F8"
        );

        bus.run_frame(&mut cpu);
        bus.ppu.take_frame();
        assert_ne!(bus.save_state(&cpu), saved, "the machine must move on");

        load_state_slot(&save_dir, 3, &mut bus, &mut cpu).expect("state must load");
        assert_eq!(
            bus.save_state(&cpu),
            saved,
            "the loaded machine differs from the saved one"
        );
        assert_eq!(
            load_latest_state(&save_dir, &mut bus, &mut cpu),
            Ok(3),
            "the confirmation must name the slot F8 chose"
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// Recency is what the player needs; the exact wall-clock second is noise.
    #[test]
    fn a_slot_timestamp_reads_as_an_age() {
        assert_eq!(age_label(Duration::ZERO), "just now");
        assert_eq!(age_label(Duration::from_secs(59)), "just now");
        assert_eq!(age_label(Duration::from_secs(60)), "1m ago");
        assert_eq!(age_label(Duration::from_secs(3599)), "59m ago");
        assert_eq!(age_label(Duration::from_secs(3600)), "1h ago");
        assert_eq!(age_label(Duration::from_secs(86_399)), "23h ago");
        assert_eq!(age_label(Duration::from_secs(86_400)), "1d ago");
    }

    /// The picker lists all nine slots, marks the selected one and says what
    /// each slot is: a save with its age, empty, or incompatible.
    #[test]
    fn the_slot_list_marks_the_selection_and_its_state() {
        let mut picker = Picker {
            slots: std::array::from_fn(|index| match index {
                0 => Slot::Ready("2m ago".to_string()),
                1 => Slot::Empty,
                2 => Slot::Incompatible,
                _ => Slot::Empty,
            }),
            selected: 0,
        };
        let lines = picker.lines();
        assert_eq!(lines[0], "SAVE STATES");
        assert_eq!(lines[1], ">1  saved 2m ago");
        assert_eq!(lines[2], " 2  empty");
        assert_eq!(lines[3], " 3  incompatible");
        assert_eq!(lines.len(), 12, "header, nine slots, two help lines");
        assert_eq!(lines[10], "UP/DOWN choose");
        assert_eq!(lines[11], "ENTER load  ESC cancel");
        assert_eq!(slot_line(9, true, &Slot::Empty), ">9  empty");
        picker.select(1);
        assert_eq!(
            picker.lines()[2],
            ">2  empty",
            "the marker follows the selection"
        );
    }

    /// The picker owns the keyboard while it is open; every key that is not a
    /// list command is ignored, so a digit cannot save over the slot being
    /// chosen and F7 cannot close the list it opened.
    #[test]
    fn the_picker_key_map_is_modal() {
        let key = |keycode, repeat| Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(keycode),
            scancode: None,
            keymod: sdl2::keyboard::Mod::empty(),
            repeat,
        };
        assert_eq!(
            picker_command(&key(Keycode::Escape, false)),
            PickerCommand::Cancel
        );
        assert_eq!(
            picker_command(&key(Keycode::Up, false)),
            PickerCommand::Move(-1)
        );
        assert_eq!(
            picker_command(&key(Keycode::Down, false)),
            PickerCommand::Move(1)
        );
        assert_eq!(
            picker_command(&key(Keycode::Return, false)),
            PickerCommand::Load
        );
        assert_eq!(
            picker_command(&key(Keycode::F7, false)),
            PickerCommand::Ignore,
            "F7 must not close the list it opened"
        );
        assert_eq!(
            picker_command(&key(Keycode::Escape, true)),
            PickerCommand::Ignore,
            "key repeat must not act twice"
        );
        assert_eq!(
            picker_command(&Event::Quit { timestamp: 0 }),
            PickerCommand::Quit
        );
        let lost = Event::Window {
            timestamp: 0,
            window_id: 0,
            win_event: WindowEvent::FocusLost,
        };
        assert_eq!(picker_command(&lost), PickerCommand::ForgetInput);
    }

    /// The device plays only for a running, unmuted game at normal speed; every
    /// other state keeps it silent.
    #[test]
    fn pause_mute_and_fast_forward_all_silence_audio() {
        assert!(
            audio_should_play(false, false, false),
            "normal play is audible"
        );
        assert!(
            !audio_should_play(true, false, false),
            "a paused game is silent"
        );
        assert!(
            !audio_should_play(false, true, false),
            "a muted game is silent"
        );
        assert!(
            !audio_should_play(false, false, true),
            "fast-forward is silent"
        );
    }

    /// Up from the first slot wraps to the last, and down from the last wraps
    /// back to the first: the list is short enough that stopping is annoying.
    #[test]
    fn slot_selection_wraps_at_both_ends() {
        assert_eq!(move_selection(0, -1), 8);
        assert_eq!(move_selection(8, 1), 0);
        assert_eq!(move_selection(4, -1), 3);
    }

    /// The picker checks every save against the running cartridge before it
    /// offers it: foreign or corrupt states are marked, never loaded by mistake.
    #[test]
    fn the_picker_checks_each_slot_against_the_running_cartridge() {
        let dir = std::env::temp_dir().join(format!("nes-picker-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let save_dir = dir.join("saves").join("game");
        let mut bus = Bus::new(test_cartridge());
        let mut cpu = Cpu::new();
        bus.power_on();
        cpu.reset(&mut bus);

        save_state_slot(&save_dir, 1, &bus, &cpu).expect("state must be written");
        std::fs::write(
            state_slot_path(&save_dir, 2),
            crate::state::wrap(&[], 0, Region::Ntsc, 0),
        )
        .expect("foreign state written");
        save_state_slot(&save_dir, 5, &bus, &cpu).expect("latest state must be written");

        let picker = Picker::open(&save_dir, &bus.cart);
        assert!(
            matches!(picker.slots[0], Slot::Ready(_)),
            "slot 1 is this ROM"
        );
        assert_eq!(picker.slots[1], Slot::Incompatible, "slot 2 is another ROM");
        assert_eq!(picker.slots[2], Slot::Empty, "slot 3 was never saved");
        assert!(
            matches!(picker.slots[4], Slot::Ready(_)),
            "slot 5 is this ROM"
        );
        assert_eq!(picker.selected, 4, "the list opens on the last saved slot");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// A quickload replaces the machine, including the frame that was on
    /// screen when the loop paused. Staying paused would keep showing that old
    /// frame, and a button held during the old run must not press on the
    /// restored first frame. A held Tab is host state, so it survives.
    #[test]
    fn a_loaded_state_unpauses_and_drops_game_buttons() {
        let mut paused = true;
        let mut input = Input {
            keys: [0xFF, 0xFF],
            pads: [0xFF, 0xFF],
            turbo: true,
            ..Default::default()
        };
        finish_load(&mut paused, None, &mut input);
        assert!(!paused, "a load must render the state it just restored");
        assert_eq!(
            (input.keys, input.pads),
            ([0, 0], [0, 0]),
            "the old run's held buttons are gone"
        );
        assert!(
            input.turbo,
            "a held Tab is host state, not part of the restored run"
        );
    }

    #[test]
    fn the_latest_slot_comes_from_the_index_not_file_times() {
        let dir = std::env::temp_dir().join(format!("nes-latest-slot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let save_dir = dir.join("saves").join("game");
        std::fs::create_dir_all(&save_dir).expect("save folder");
        let index = latest_state_path(&save_dir);

        assert_eq!(read_latest_slot(&save_dir), Ok(None), "nothing saved yet");

        std::fs::write(&index, b"2\n").expect("index written");
        assert_eq!(read_latest_slot(&save_dir), Ok(Some(2)));

        std::fs::write(&index, b"0\n").expect("index written");
        assert!(
            read_latest_slot(&save_dir).unwrap_err().contains("1-9"),
            "an out-of-range slot names the valid range"
        );

        std::fs::write(&index, b"nine\n").expect("index written");
        assert!(
            read_latest_slot(&save_dir).unwrap_err().contains("nine"),
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

    /// Fast-forward is the one unpaused state that does not wait after a
    /// frame; a paused game still waits so the window stays responsive.
    #[test]
    fn fast_forward_skips_the_frame_wait() {
        assert!(waits_after_frame(false, false), "normal play is paced");
        assert!(!waits_after_frame(false, true), "fast-forward is uncapped");
        assert!(waits_after_frame(true, false), "a paused game still waits");
        assert!(
            waits_after_frame(true, true),
            "holding Tab while paused must not spin the loop"
        );
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

    /// Player 2 has a second keyboard set on the left of the board. It must
    /// press the same eight bits in the same order, and no key may belong to
    /// both players -- a shared key would press two buttons at once.
    #[test]
    fn player_two_keys_are_a_second_distinct_pad() {
        assert_eq!(
            P2_BUTTONS,
            [
                (Keycode::G, 0x01),
                (Keycode::H, 0x02),
                (Keycode::T, 0x04),
                (Keycode::Y, 0x08),
                (Keycode::W, 0x10),
                (Keycode::S, 0x20),
                (Keycode::A, 0x40),
                (Keycode::D, 0x80),
            ],
            "the agreed player 2 keys, in report order"
        );
        for (index, (key, bit)) in P2_BUTTONS.iter().enumerate() {
            assert_eq!(*bit, 0x01 << index, "{key:?} is bit {index} of the report");
            assert_eq!(p2_button_for(*key), Some(*bit), "{key:?}");
            assert_eq!(button_for(*key), None, "{key:?} must not also be player 1");
        }
        assert_eq!(p2_button_for(Keycode::Z), None, "Z stays player 1");
    }

    #[test]
    fn every_controller_button_maps_to_its_nes_bit() {
        for (button, bit) in [
            (Button::A, 0x01),
            (Button::B, 0x02),
            (Button::Back, 0x04),
            (Button::Start, 0x08),
            (Button::DPadUp, 0x10),
            (Button::DPadDown, 0x20),
            (Button::DPadLeft, 0x40),
            (Button::DPadRight, 0x80),
        ] {
            assert_eq!(pad_bit(button), Some(bit), "{button:?}");
        }
        assert_eq!(pad_bit(Button::X), None, "a button with no NES twin");
    }

    #[test]
    fn player_two_keys_hold_their_own_mask() {
        let mut input = Input::default();
        assert!(input.apply_event(key_event(Keycode::G, true)));
        assert_eq!(input.keys, [0, 0x01], "G is player 2's A");
        assert!(input.apply_event(key_event(Keycode::G, false)));
        assert_eq!(input.keys, [0, 0]);

        input.apply_event(key_event(Keycode::Z, true));
        input.apply_event(key_event(Keycode::H, true));
        assert_eq!(input.keys, [0x01, 0x02], "both players can hold a button");

        input.add_pad(7);
        input.apply_event(key_event(Keycode::G, true));
        input.apply_event(pad_event(7, Button::A, true));
        assert_eq!(
            (input.keys, input.pads),
            ([0x01, 0x03], [0x01, 0]),
            "player 2's key and pad both hold A"
        );
        input.apply_event(key_event(Keycode::G, false));
        assert_eq!(
            (input.keys, input.pads),
            ([0x01, 0x02], [0x01, 0]),
            "releasing the key leaves the pad holding A"
        );
    }

    #[test]
    fn a_key_up_clears_only_its_own_bit() {
        let mut input = Input::default();
        for key in [Keycode::Z, Keycode::Right, Keycode::Return] {
            assert!(input.apply_event(key_event(key, true)), "{key:?} down");
        }
        assert_eq!(input.keys[0], 0x01 | 0x80 | 0x08, "Z, Right and Return");
        assert!(input.apply_event(key_event(Keycode::Right, false)));
        assert_eq!(
            input.keys[0],
            0x01 | 0x08,
            "only Right's bit goes; Z and Return are still down"
        );
        // Escape quits, a key with no bit does nothing, and both still say the
        // run continues.
        assert!(
            !input.apply_event(key_event(Keycode::Escape, true)),
            "Escape ends the run"
        );
        let before = input.keys;
        assert!(
            input.apply_event(key_event(Keycode::F5, true)),
            "F5 is not a quit"
        );
        assert_eq!(input.keys, before, "F5 is not on either pad");
    }

    /// Keyboard and gamepad are separate masks so releasing one source cannot
    /// release a button the other still holds.
    #[test]
    fn keyboard_and_gamepad_hold_the_same_bits_independently() {
        let mut input = Input::default();
        input.add_pad(0);
        input.apply_event(key_event(Keycode::Z, true));
        input.apply_event(pad_event(0, Button::A, true));
        assert_eq!((input.keys, input.pads), ([0x01, 0], [0x01, 0]));
        input.apply_event(key_event(Keycode::Z, false));
        assert_eq!((input.keys, input.pads), ([0x00, 0], [0x01, 0]));
        input.apply_event(pad_event(0, Button::A, false));
        assert_eq!((input.keys, input.pads), ([0, 0], [0, 0]));
    }

    /// Tab is fast-forward, not a pad button: holding it must never press A,
    /// B or anything a game can read.
    #[test]
    fn tab_holds_fast_forward_without_pressing_a_button() {
        let mut input = Input::default();
        assert!(
            input.apply_event(key_event(Keycode::Tab, true)),
            "pressing Tab is not a quit"
        );
        assert!(input.turbo, "Tab starts fast-forward");
        assert_eq!(
            (input.keys, input.pads),
            ([0, 0], [0, 0]),
            "Tab is not on either pad"
        );
        input.apply_event(key_event(Keycode::Tab, false));
        assert!(!input.turbo, "releasing Tab stops it");
    }

    /// SDL sends no `KeyUp` for a key released while another window has the
    /// focus, so the FocusLost arm is the only thing that un-latches a held
    /// button. `Event` is a plain enum value, so this runs with no window and
    /// no X server -- it needed the handler extracted from `main` to be
    /// reachable at all, not a real alt-tab.
    #[test]
    fn a_focus_loss_clears_both_players_but_remembers_the_pads() {
        let mut input = Input::default();
        input.add_pad(7);
        input.add_pad(9);
        input.keys = [0xFF, 0xFF];
        input.pads = [0xFF, 0xFF];
        input.turbo = true;
        assert!(
            input.apply_event(focus_lost()),
            "losing focus is not a quit"
        );
        assert_eq!(
            (input.keys, input.pads),
            ([0, 0], [0, 0]),
            "every held bit must be forgotten"
        );
        assert!(!input.turbo, "a held Tab must not survive the alt-tab");
        assert_eq!(
            input.pad_ids,
            [Some(7), Some(9)],
            "the pads are still plugged in"
        );
    }

    #[test]
    fn a_two_player_game_keeps_the_players_apart() {
        let input = Input {
            keys: [0x01, 0x02],
            pads: [0x04, 0x08],
            ..Default::default()
        };
        assert_eq!(input.controller_masks(2), [0x05, 0x0A]);
    }

    /// A one-player game folds every input source into controller 1, so the
    /// second keyboard set or the second pad can play it alone.
    #[test]
    fn a_one_player_game_folds_both_input_sets_into_controller_one() {
        let input = Input {
            keys: [0x01, 0x02],
            pads: [0x04, 0x08],
            ..Default::default()
        };
        assert_eq!(input.controller_masks(1), [0x0F, 0]);
    }

    #[test]
    fn gamepads_take_the_first_free_player_slot_and_leave_it_on_removal() {
        let mut input = Input::default();
        assert_eq!(input.add_pad(7), Some(0), "the first pad is player 1");
        assert_eq!(input.add_pad(9), Some(1), "the second pad is player 2");
        assert_eq!(input.add_pad(11), None, "a third pad has no player");
        assert_eq!(input.add_pad(7), None, "the same pad is not opened twice");
        input.pads[1] = 0x01;
        assert_eq!(input.remove_pad(9), Some(1));
        assert_eq!(input.pads[1], 0, "a removed pad releases what it held");
        assert_eq!(input.add_pad(11), Some(1), "the freed slot is reused");
    }

    #[test]
    fn a_gamepad_button_reaches_the_player_whose_slot_holds_it() {
        let mut input = Input::default();
        input.add_pad(7);
        input.add_pad(9);
        input.apply_event(pad_event(9, Button::A, true));
        assert_eq!(input.pads, [0, 0x01], "the second pad is player 2");
        input.apply_event(pad_event(9, Button::A, false));
        assert_eq!(input.pads, [0, 0]);
        input.apply_event(pad_event(7, Button::B, true));
        assert_eq!(input.pads, [0x02, 0]);
        input.apply_event(pad_event(11, Button::B, true));
        assert_eq!(input.pads, [0x02, 0], "an unassigned pad is ignored");
    }

    /// A typo in another game's line must not switch this game back to two
    /// players: the valid override below it still applies.
    #[test]
    fn a_broken_line_for_another_game_does_not_hide_a_valid_override() {
        assert_eq!(
            players_for_game("Some Game Without Equals\nZelda = 1\n", "Zelda"),
            Ok(Some(1))
        );
        assert_eq!(
            players_for_game("Mario = three\nZelda = 1\n", "Zelda"),
            Ok(Some(1))
        );
        assert_eq!(
            players_for_game("Zelda 1\n", "Zelda"),
            Ok(None),
            "a line without `=` is ignored, not guessed"
        );
    }

    /// The file wrapper: a found line applies, while a missing file, a game
    /// with no line, and a broken line all fall back instead of stopping play.
    #[test]
    fn the_player_config_file_applies_found_lines_and_defaults_on_trouble() {
        let dir = std::env::temp_dir().join(format!("nes-players-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nes-players.conf");

        assert_eq!(
            players_from_file(&path, "Zelda"),
            Ok(None),
            "a missing file is the default"
        );

        std::fs::write(&path, "Zelda = 1\n").unwrap();
        assert_eq!(players_from_file(&path, "Zelda"), Ok(Some(1)));
        assert_eq!(
            players_from_file(&path, "Mario"),
            Ok(None),
            "no line for the game is the default"
        );

        std::fs::write(&path, "Zelda = 0\n").unwrap();
        assert!(
            players_from_file(&path, "Zelda").is_err(),
            "a broken line is an error the caller logs"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_player_override_is_one_line_per_game() {
        let text = "\
# games that are not two-player
Contra (USA) = 2
Zelda = 1   # solo
";
        assert_eq!(players_for_game(text, "Zelda"), Ok(Some(1)));
        assert_eq!(
            players_for_game(text, "contra (usa)"),
            Ok(Some(2)),
            "the game name is case-insensitive"
        );
        assert_eq!(
            players_for_game(text, "Mario"),
            Ok(None),
            "no line means the default"
        );
        assert_eq!(
            players_for_game("Zelda = 1\nZelda = 2\n", "Zelda"),
            Ok(Some(2)),
            "the last matching line wins"
        );
    }

    #[test]
    fn a_broken_player_override_is_an_error() {
        for (text, needle) in [
            ("Zelda = 0\n", "1 or 2"),
            ("Zelda = 3\n", "1 or 2"),
            ("Zelda = one\n", "one"),
        ] {
            let err = players_for_game(text, "Zelda").unwrap_err();
            assert!(err.contains(needle), "{text:?} gave {err:?}");
        }
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
            wrap_text("state slot 9 is empty: /saves/Legend of Zelda/state9", 28),
            ["state slot 9 is empty:", "/saves/Legend of", "Zelda/state9"]
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
