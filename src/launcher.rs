//! A ROM picker: list the `.nes` files in a folder and start the emulator on
//! the one chosen.
//!
//! This is a separate mode of the same binary. Launching hides this window and
//! runs `nes <rom>` as a child, so the game's frame loop is untouched and the
//! launcher is back when the game exits.

use font8x8::{UnicodeFonts, BASIC_FONTS};
use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::Keycode;
use sdl2::mouse::MouseButton;
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Canvas, Texture, TextureAccess};
use sdl2::video::Window;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Logical size, the same as the NES frame: the window starts at 3x and SDL
/// scales whatever the window manager makes of it.
const W: usize = 320;
const H: usize = 240;
const SCALE: u32 = 3;

const MARGIN: usize = 8;
const CELL: usize = 8;
const LIST_TOP: usize = 44;
const LIST_BOTTOM: usize = H - 16;

const BG: u32 = 0x0010_1418;
const FG: u32 = 0x00E8_E8E8;
const DIM: u32 = 0x0088_929A;
const SEL_BG: u32 = 0x0030_6090;

/// How many list rows fit between the header and the footer.
fn visible_rows() -> usize {
    (LIST_BOTTOM - LIST_TOP) / CELL
}

/// A file this launcher will boot: any case of `.nes`, nothing else.
fn is_rom(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nes"))
}

/// The `.nes` files directly in `dir`, sorted by name as the eye reads it.
/// Subfolders are not walked -- a launcher that silently reaches into folders
/// the user did not open is harder to explain than one that does not.
fn scan(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut roms: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_rom(path))
        .collect();
    roms.sort_by_key(|path| {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
    });
    Ok(roms)
}

/// The folder holding this executable. Both a terminal run and a desktop icon
/// land here.
fn exe_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    Ok(exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".")))
}

/// The launcher's one setting lives beside the executable, the same place the
/// ROM list defaults to.
const CONFIG_NAME: &str = "nes-launcher.conf";

fn config_path() -> Result<PathBuf, String> {
    Ok(exe_dir()?.join(CONFIG_NAME))
}

/// The folder named by the config file. Whitespace and an empty file mean
/// nothing was saved. The path is not checked here.
fn read_saved(text: &str) -> Option<PathBuf> {
    let path = text.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// A remembered folder that is still there. A folder that was moved or
/// unmounted is ignored, so the next start falls back instead of failing.
fn saved_dir() -> Option<PathBuf> {
    let text = std::fs::read_to_string(config_path().ok()?).ok()?;
    let saved = read_saved(&text)?;
    saved.is_dir().then_some(saved)
}

/// Remember `dir` for the next start. A read-only install directory just means
/// the setting is not kept; it must not stop the launcher.
fn remember(dir: &Path) {
    if let Ok(path) = config_path() {
        let _ = std::fs::write(path, format!("{}\n", dir.display()));
    }
}

/// Keep the selected row inside the visible window with as little movement as
/// possible: only scroll when the selection leaves it.
fn keep_visible(selected: usize, scroll: usize, visible: usize, len: usize) -> usize {
    if visible == 0 || len == 0 {
        return 0;
    }
    let visible = visible.min(len);
    let max_scroll = len - visible;
    let mut scroll = scroll.min(max_scroll);
    if selected < scroll {
        scroll = selected;
    } else if selected >= scroll + visible {
        scroll = selected + 1 - visible;
    }
    scroll.min(max_scroll)
}

/// Which entry a click at list-relative pixel `y` lands on, if any.
fn row_at(y: usize, top: usize, visible: usize, len: usize, scroll: usize) -> Option<usize> {
    if y < top {
        return None;
    }
    let row = (y - top) / CELL;
    if row >= visible {
        return None;
    }
    let index = scroll + row;
    (index < len).then_some(index)
}

/// Cut `text` to `max` characters with an ellipsis. `keep_tail` keeps the end,
/// which is the part of a folder path that says where you are; otherwise the
/// start is kept, which is how file names are told apart.
fn elide(text: &str, max: usize, keep_tail: bool) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(3);
    let kept: String = if keep_tail {
        text.chars().skip(count - keep).collect()
    } else {
        text.chars().take(keep).collect()
    };
    if keep_tail {
        format!("...{kept}")
    } else {
        format!("{kept}...")
    }
}

/// The list and where it is looking. All movement goes through here so the
/// selection and the scroll offset cannot disagree.
struct List {
    roms: Vec<PathBuf>,
    selected: usize,
    scroll: usize,
}

impl List {
    fn new(roms: Vec<PathBuf>) -> List {
        List {
            roms,
            selected: 0,
            scroll: 0,
        }
    }

    fn reset(&mut self, roms: Vec<PathBuf>) {
        self.roms = roms;
        self.selected = 0;
        self.scroll = 0;
    }

    fn move_by(&mut self, delta: isize, visible: usize) {
        if self.roms.is_empty() {
            return;
        }
        let last = self.roms.len() - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last as isize) as usize;
        self.scroll = keep_visible(self.selected, self.scroll, visible, self.roms.len());
    }

    fn page(&mut self, direction: isize, visible: usize) {
        self.move_by(direction * visible.max(1) as isize, visible);
    }

    fn home(&mut self, visible: usize) {
        self.selected = 0;
        self.scroll = keep_visible(self.selected, self.scroll, visible, self.roms.len());
    }

    fn end(&mut self, visible: usize) {
        self.selected = self.roms.len().saturating_sub(1);
        self.scroll = keep_visible(self.selected, self.scroll, visible, self.roms.len());
    }
}

/// Draw one glyph row of text. `px` and `py` are the top-left of the first
/// character; text that runs past the right edge is cut, not wrapped.
fn put_text(fb: &mut [u32], px: usize, py: usize, text: &str, colour: u32) {
    let mut x = px;
    for ch in text.chars() {
        if x + CELL > W {
            break;
        }
        let glyph = BASIC_FONTS.get(ch).or_else(|| BASIC_FONTS.get('?'));
        if let Some(glyph) = glyph {
            for (row, bits) in glyph.iter().enumerate() {
                let y = py + row;
                if y >= H {
                    break;
                }
                for col in 0..CELL {
                    if bits & (1 << col) != 0 {
                        let x = x + col;
                        if x < W {
                            fb[y * W + x] = colour;
                        }
                    }
                }
            }
        }
        x += CELL;
    }
}

fn fill_row(fb: &mut [u32], y: usize, height: usize, colour: u32) {
    let top = y.min(H);
    let bottom = (y + height).min(H);
    for row in top..bottom {
        fb[row * W..(row + 1) * W].fill(colour);
    }
}

/// Paint the whole launcher frame from `dir` and `list`.
fn render(fb: &mut [u32], dir: &Path, list: &List) {
    fb.fill(BG);
    put_text(fb, MARGIN, 6, "NES LAUNCHER", FG);
    put_text(
        fb,
        MARGIN,
        18,
        &elide(&dir.display().to_string(), (W - 2 * MARGIN) / CELL, true),
        DIM,
    );
    let count = match list.roms.len() {
        0 => "no .nes files here".to_string(),
        1 => "1 ROM".to_string(),
        n => format!("{n} ROMs"),
    };
    put_text(fb, MARGIN, 30, &count, DIM);

    let visible = visible_rows();
    for row in 0..visible {
        let index = list.scroll + row;
        let Some(path) = list.roms.get(index) else {
            break;
        };
        let y = LIST_TOP + row * CELL;
        if index == list.selected {
            fill_row(fb, y, CELL, SEL_BG);
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        put_text(
            fb,
            MARGIN,
            y,
            &elide(&name, (W - 2 * MARGIN) / CELL, false),
            FG,
        );
    }

    if list.roms.is_empty() {
        put_text(
            fb,
            MARGIN,
            LIST_TOP + CELL,
            "Press O to choose a folder",
            DIM,
        );
    }

    put_text(
        fb,
        MARGIN,
        LIST_BOTTOM + 4,
        "ENTER play   O folder   ESC quit",
        DIM,
    );
}

/// Upload the software frame and show it. Same streaming ARGB texture path the
/// emulator uses, so the launcher needs no drawing library.
fn present(canvas: &mut Canvas<Window>, texture: &mut Texture, fb: &[u32]) -> Result<(), String> {
    // SAFETY: u32 is plain old data and every bit pattern is a valid u32, so
    // all len*4 bytes of the frame are valid to read as u8.
    let bytes = unsafe { std::slice::from_raw_parts(fb.as_ptr() as *const u8, fb.len() * 4) };
    texture
        .update(None, bytes, W * 4)
        .map_err(|e| e.to_string())?;
    canvas.clear();
    canvas
        .copy(texture, None, None)
        .map_err(|e| e.to_string())?;
    canvas.present();
    Ok(())
}

/// A click's logical row pixel, or `None` for the letterbox SDL leaves around
/// the scaled frame.
fn to_logical_y(canvas: &Canvas<Window>, x: i32, y: i32) -> Option<usize> {
    let viewport = canvas.viewport();
    if !viewport.contains_point((x, y)) {
        return None;
    }
    let ly = (y - viewport.y()) as i64 * H as i64 / viewport.height() as i64;
    Some(ly.clamp(0, H as i64 - 1) as usize)
}

/// Re-read a folder, keeping the current list when the read fails. A folder
/// that was removed or chmod-ed under the launcher must not close it too.
fn refresh(dir: &Path, list: &mut List) {
    match scan(dir) {
        Ok(roms) => list.reset(roms),
        Err(e) => eprintln!("launcher: {e}"),
    }
}

/// Hide the launcher, run the emulator on `rom`, then show it again. Failure
/// to start is reported and the list stays usable, so one bad file is not a
/// dead end.
fn launch(canvas: &mut Canvas<Window>, rom: &Path) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("launcher: cannot find own executable: {e}");
            return;
        }
    };
    canvas.window_mut().hide();
    let status = Command::new(exe).arg(rom).status();
    canvas.window_mut().show();
    canvas.window_mut().raise();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("{}: emulator exited with {status}", rom.display()),
        Err(e) => eprintln!("{}: cannot start emulator: {e}", rom.display()),
    }
}

/// Run the launcher until the window is closed. Returns an error only for a
/// failure that leaves nothing to interact with.
pub fn run() -> Result<(), String> {
    let mut dir = exe_dir()?;
    let mut roms = scan(&dir)?;
    // A saved folder that cannot be read falls back to the executable's own
    // folder rather than refusing to start.
    if let Some(saved) = saved_dir() {
        if let Ok(found) = scan(&saved) {
            dir = saved;
            roms = found;
        }
    }
    let mut list = List::new(roms);

    let sdl = sdl2::init().map_err(|e| e.to_string())?;
    let video = sdl.video().map_err(|e| e.to_string())?;
    let window = video
        .window("NES Launcher", (W as u32) * SCALE, (H as u32) * SCALE)
        .position_centered()
        .resizable()
        .build()
        .map_err(|e| e.to_string())?;
    let mut canvas: Canvas<Window> = window.into_canvas().build().map_err(|e| e.to_string())?;
    canvas
        .set_logical_size(W as u32, H as u32)
        .map_err(|e| e.to_string())?;

    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture(
            PixelFormatEnum::ARGB8888,
            TextureAccess::Streaming,
            W as u32,
            H as u32,
        )
        .map_err(|e| e.to_string())?;

    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;
    let mut frame = vec![0u32; W * H];
    let visible = visible_rows();

    render(&mut frame, &dir, &list);
    present(&mut canvas, &mut texture, &frame)?;

    loop {
        // A timeout keeps the loop alive for the window manager's expose event
        // even on a desktop that sends nothing while the pointer is still.
        let Some(event) = events.wait_event_timeout(250) else {
            continue;
        };
        let mut dirty = false;
        match event {
            Event::Quit { .. } => break,
            Event::KeyDown {
                keycode: Some(key), ..
            } => match key {
                Keycode::Escape | Keycode::Q => break,
                Keycode::Up => {
                    list.move_by(-1, visible);
                    dirty = true;
                }
                Keycode::Down => {
                    list.move_by(1, visible);
                    dirty = true;
                }
                Keycode::PageUp => {
                    list.page(-1, visible);
                    dirty = true;
                }
                Keycode::PageDown => {
                    list.page(1, visible);
                    dirty = true;
                }
                Keycode::Home => {
                    list.home(visible);
                    dirty = true;
                }
                Keycode::End => {
                    list.end(visible);
                    dirty = true;
                }
                Keycode::Return | Keycode::KpEnter => {
                    if let Some(rom) = list.roms.get(list.selected).cloned() {
                        launch(&mut canvas, &rom);
                        refresh(&dir, &mut list);
                        dirty = true;
                    }
                }
                Keycode::O => {
                    if let Some(picked) = rfd::FileDialog::new()
                        .set_title("NES ROM folder")
                        .set_directory(&dir)
                        .pick_folder()
                    {
                        // An unreadable pick leaves the old folder and list up
                        // rather than replacing them with nothing.
                        match scan(&picked) {
                            Ok(roms) => {
                                remember(&picked);
                                dir = picked;
                                list.reset(roms);
                                dirty = true;
                            }
                            Err(e) => eprintln!("launcher: {e}"),
                        }
                    }
                }
                _ => {}
            },
            Event::MouseButtonDown {
                mouse_btn: MouseButton::Left,
                x,
                y,
                ..
            } => {
                if let Some(ly) = to_logical_y(&canvas, x, y) {
                    if let Some(index) = row_at(ly, LIST_TOP, visible, list.roms.len(), list.scroll)
                    {
                        list.selected = index;
                        let rom = list.roms[index].clone();
                        launch(&mut canvas, &rom);
                        refresh(&dir, &mut list);
                        dirty = true;
                    }
                }
            }
            Event::MouseWheel { y, .. } => {
                if y != 0 {
                    list.move_by(3 * y as isize, visible);
                    dirty = true;
                }
            }
            Event::Window {
                win_event: WindowEvent::Exposed,
                ..
            } => dirty = true,
            _ => {}
        }
        if dirty {
            render(&mut frame, &dir, &list);
            present(&mut canvas, &mut texture, &frame)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        elide, is_rom, keep_visible, read_saved, row_at, visible_rows, List, CELL, LIST_TOP,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn only_nes_files_are_roms_whatever_their_case() {
        for name in ["game.nes", "GAME.NES", "Game.Nes", "a b.nes"] {
            assert!(is_rom(Path::new(name)), "{name} should boot");
        }
        for name in ["game.smc", "game.nes.bak", "nes", "game.zip", ".nes"] {
            assert!(!is_rom(Path::new(name)), "{name} should not boot");
        }
    }

    /// The config file is one path and a newline. A file left empty, or with
    /// only whitespace, must read as "nothing saved" -- otherwise the launcher
    /// would start in the current directory or fail.
    #[test]
    fn the_saved_folder_is_one_trimmed_path_or_nothing() {
        assert_eq!(read_saved("/roms/nes\n"), Some(PathBuf::from("/roms/nes")));
        assert_eq!(
            read_saved("  /roms/nes  \n"),
            Some(PathBuf::from("/roms/nes")),
            "spaces around the path are not part of it"
        );
        assert_eq!(read_saved(""), None, "empty file: nothing saved");
        assert_eq!(read_saved("\n\t \n"), None, "whitespace only: nothing");
    }

    #[test]
    fn long_names_keep_the_end_that_tells_them_apart() {
        assert_eq!(elide("short.nes", 20, false), "short.nes");
        let cut = elide("Super Mario Bros. + Duck Hunt (USA).nes", 20, false);
        assert_eq!(cut, "Super Mario Bros....");
        assert_eq!(cut.chars().count(), 20);
        // Paths keep their tail, because the last folder says where you are.
        let cut = elide("/home/charlie/DataSSD/projects/roms/nes", 20, true);
        assert_eq!(cut, "...projects/roms/nes");
        assert_eq!(cut.chars().count(), 20);
    }

    #[test]
    fn scrolling_follows_the_selection_and_clamps_to_the_list() {
        assert_eq!(keep_visible(0, 0, 5, 20), 0);
        assert_eq!(keep_visible(4, 0, 5, 20), 0, "last visible row: no scroll");
        assert_eq!(keep_visible(5, 0, 5, 20), 1, "one past: scroll by one");
        assert_eq!(keep_visible(0, 3, 5, 20), 0, "up past the top");
        assert_eq!(
            keep_visible(19, 0, 5, 20),
            15,
            "the end leaves a full window"
        );
        assert_eq!(keep_visible(0, 0, 5, 3), 0, "no scroll when all fit");
        assert_eq!(keep_visible(0, 0, 0, 3), 0, "no rows, no scroll");
        assert_eq!(keep_visible(0, 0, 5, 0), 0, "no entries, no scroll");
    }

    #[test]
    fn a_click_maps_to_the_row_it_lands_on() {
        let visible = 5;
        // Three rows scrolled away: the first visible pixel is entry 3.
        assert_eq!(row_at(LIST_TOP, LIST_TOP, visible, 20, 3), Some(3));
        assert_eq!(
            row_at(LIST_TOP + 2 * CELL, LIST_TOP, visible, 20, 3),
            Some(5)
        );
        assert_eq!(
            row_at(LIST_TOP + 5 * CELL, LIST_TOP, visible, 20, 3),
            None,
            "below the last visible row"
        );
        assert_eq!(
            row_at(LIST_TOP - 1, LIST_TOP, visible, 20, 3),
            None,
            "above the list"
        );
        assert_eq!(
            row_at(LIST_TOP + 3 * CELL, LIST_TOP, visible, 4, 3),
            None,
            "scrolled past the end of a short list"
        );
    }

    #[test]
    fn the_selection_never_leaves_the_visible_window() {
        let visible = visible_rows();
        let roms: Vec<PathBuf> = (0..100)
            .map(|n| PathBuf::from(format!("rom-{n}.nes")))
            .collect();
        let mut list = List::new(roms);
        for _ in 0..100 {
            list.move_by(1, visible);
            assert!(list.selected >= list.scroll);
            assert!(list.selected < list.scroll + visible);
        }
        assert_eq!(list.selected, 99, "stops at the last entry");
        for _ in 0..100 {
            list.move_by(-1, visible);
            assert!(list.selected >= list.scroll);
            assert!(list.selected < list.scroll + visible);
        }
        assert_eq!(list.selected, 0, "stops at the first entry");
    }

    #[test]
    fn an_empty_list_takes_movement_without_panicking() {
        let mut list = List::new(Vec::new());
        list.move_by(1, 20);
        list.move_by(-1, 20);
        list.page(1, 20);
        list.home(20);
        list.end(20);
        assert_eq!(list.selected, 0);
        assert!(list.roms.get(list.selected).is_none());
        // And scrolling an empty list is the top, not an underflow.
        assert_eq!(keep_visible(0, 5, 20, 0), 0);
    }
}
