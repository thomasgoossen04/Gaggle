//! Clipboard writes that actually stick on Linux.
//!
//! gpui 0.2's Wayland backend only offers the selection while the window holds a
//! fresh input serial; a plain mouse click frequently has a stale one, so
//! [`gpui::App::write_to_clipboard`] silently no-ops there. Callers still invoke
//! that (it is the right path on macOS, Windows and X11) and *also* call
//! [`copy`], which hands the text to whatever external clipboard tool is present
//! so the copy survives the click and outlives the process.

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::process::{Command, Stdio};

/// Best-effort push of `text` onto the system clipboard via an external helper.
///
/// No-op on non-Unix targets (the gpui path is enough there). On Unix it tries,
/// in order, `wl-copy` (Wayland), `xclip` and `xsel` (X11); the first one that
/// spawns wins. Each of those daemonizes itself to keep serving the selection,
/// so a short reaper thread collects the shim process without blocking the UI.
pub fn copy(text: &str) {
    #[cfg(unix)]
    {
        const CANDIDATES: &[(&str, &[&str])] = &[
            ("wl-copy", &["--type", "text/plain"]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        for (bin, args) in CANDIDATES {
            if try_pipe(bin, args, text) {
                return;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = text;
}

#[cfg(unix)]
fn try_pipe(bin: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
        // Drop closes the pipe so the helper reads EOF and takes ownership.
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    true
}
