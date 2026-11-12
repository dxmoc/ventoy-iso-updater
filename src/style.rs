//! Terminal styling and side notes. Colors only on a terminal and only while
//! NO_COLOR is unset, so piped output and logs stay plain text.

use std::io::IsTerminal;
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal() && vt_on()
    })
}

/// Older Windows consoles print escapes literally unless asked not to.
#[cfg(windows)]
fn vt_on() -> bool {
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
        SetConsoleMode,
    };
    // SAFETY: plain console calls on our own stdout handle.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        GetConsoleMode(h, &mut mode) != 0
            && SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn vt_on() -> bool {
    true
}

fn paint(code: &str, s: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn dim(s: &str) -> String {
    paint("2", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}
pub fn yellow(s: &str) -> String {
    paint("33", s)
}
pub fn red(s: &str) -> String {
    paint("31", s)
}

/// A summary status, colored by how it starts (see crate::Summary).
pub fn status(s: &str) -> String {
    if s.starts_with("OK") {
        green(s)
    } else if s.starts_with("FAIL") || s.starts_with("DELETE FAILED") {
        red(s)
    } else if s.starts_with("WOULD") || s.starts_with("MANUAL") {
        yellow(s)
    } else {
        s.to_string()
    }
}

static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A side remark from a resolver. They run on worker threads under a live
/// counter line, so the remark is kept for print_notes() instead.
pub fn note(msg: String) {
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).push(msg);
}

pub fn print_notes() {
    for n in NOTES.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        println!("{}", dim(&format!("  note: {n}")));
    }
}
