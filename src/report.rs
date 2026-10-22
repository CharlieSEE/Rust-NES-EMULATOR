//! One error path for the whole binary: every failure is written beside the
//! executable and, where there is a desktop to show it on, put in a message
//! box. A launcher started from a desktop icon has no terminal, so stderr
//! alone reaches nobody.

use sdl2::messagebox::{show_simple_message_box, MessageBoxFlag};
use sdl2::video::Window;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The log lives beside the executable, next to the ROM list and the config.
/// Appended to, never truncated: the error before this one is often the cause.
pub const LOG_NAME: &str = "nes-errors.log";

/// Set by the launcher on the emulator it starts. The launcher's window is
/// maximized and stays up, so a box opened by the child would land behind it;
/// the child keeps quiet and the launcher shows the captured text instead.
pub const NO_POPUP_ENV: &str = "NES_NO_POPUP";

/// The folder holding this executable. Both a terminal run and a desktop icon
/// land here, and it is where the launcher keeps its config and the report
/// keeps its log.
pub fn exe_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    Ok(exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".")))
}

/// One error, one line. Multi-line messages (the usage text is one) have their
/// newlines flattened so a log line stays greppable.
fn log_line(at: SystemTime, msg: &str) -> String {
    // Seconds since the epoch: sortable, greppable, and no calendar code.
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let msg = msg.lines().collect::<Vec<_>>().join(" | ");
    format!("{secs}  {msg}\n")
}

/// Append one error to the log. Failure to write it is reported on stderr and
/// then dropped: a log that cannot be written must not become a second error,
/// and certainly must not recurse into this function.
fn append(msg: &str) {
    let dir = match exe_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("nes: cannot locate log file: {e}");
            return;
        }
    };
    let path = dir.join(LOG_NAME);
    let line = log_line(SystemTime::now(), msg);
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
    {
        eprintln!("nes: cannot write {}: {e}", path.display());
    }
}

/// Record an error and keep saying it on stderr. Used where a second popup
/// would be noise, or where stderr is the only other channel.
pub fn log_error(msg: &str) {
    eprintln!("{msg}");
    append(msg);
}

/// Record an error and tell the user. The message box is best-effort: it needs
/// a desktop, and it can fail before SDL's video subsystem exists, so its
/// result is ignored. The log line is the durable half.
pub fn report(msg: &str) {
    log_error(msg);
    popup("NES", msg);
}

/// Show an error box. The log line is the durable half; the box needs a
/// desktop and is allowed to fail silently.
fn popup(title: &str, msg: &str) {
    // A terminal already shows the message, and a box would block a script.
    // The launcher's own `popup_for` bypasses this: its window is the report.
    if std::env::var_os(NO_POPUP_ENV).is_some() || std::io::stderr().is_terminal() {
        return;
    }
    if let Err(e) = show_simple_message_box(MessageBoxFlag::ERROR, title, msg, None::<&Window>) {
        // No display, no zenity: the box is gone, but the log should say why
        // the user saw nothing.
        append(&format!("cannot show message box: {e}"));
    }
}

/// Show an error box owned by `window`. The launcher uses this so its message
/// is modal to the launcher and cannot land behind the maximized window.
pub fn popup_for(window: &Window, msg: &str) {
    if let Err(e) = show_simple_message_box(MessageBoxFlag::ERROR, "NES", msg, window) {
        append(&format!("cannot show message box: {e}"));
    }
}

/// Log and announce a panic before the normal hook prints it, so a crash in
/// the game reaches a desktop user too. A panic is the one error that does not
/// travel through `Result`.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = panic_message(info);
        append(&msg);
        // The normal hook prints the thread and the message to stderr. Run it
        // before the box, so a machine with no display still gets that output
        // instead of hanging in a dialog that never appears.
        previous(info);
        popup("NES crashed", &msg);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let text = info.payload_as_str().unwrap_or("panic with no message");
    match info.location() {
        Some(at) => format!("panic at {}:{}: {text}", at.file(), at.line()),
        None => format!("panic: {text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::log_line;
    use std::time::UNIX_EPOCH;

    #[test]
    fn a_multi_line_error_is_one_log_line() {
        assert_eq!(
            log_line(UNIX_EPOCH, "usage: nes <rom.nes>\n  --dump N\n  --play S"),
            "0  usage: nes <rom.nes> |   --dump N |   --play S\n"
        );
        assert_eq!(log_line(UNIX_EPOCH, "single line"), "0  single line\n");
    }
}
