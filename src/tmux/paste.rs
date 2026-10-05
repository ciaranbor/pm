//! Typing text into a pane as a paste. `send-keys -l` turns each newline
//! into Enter, which would submit a multi-line message line by line, so
//! the text goes through a paste buffer instead: loaded from stdin, so no
//! byte of it passes through tmux's command parser, then pasted with the
//! pane's bracketed-paste markers (`-p`) and its newlines kept as they are
//! (`-r`), and deleted (`-d`). Each paste has a buffer of its own, so two
//! at once never paste each other's text.

use std::io::Write;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};

use super::{exact, run_tmux, send_key, tmux_command};
use crate::error::{PmError, Result};

/// Paste `text` into `pane`, then press Enter. The pause before Enter is
/// [`super::send_text`]'s.
pub fn paste_text(server: Option<&str>, pane: &str, text: &str) -> Result<()> {
    static PASTES: AtomicU32 = AtomicU32::new(0);
    let buffer = format!(
        "pm-input-{}-{}",
        std::process::id(),
        PASTES.fetch_add(1, Ordering::Relaxed)
    );
    let mut load = tmux_command(server)
        .args(["load-buffer", "-b", &buffer, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let written = load
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(text.as_bytes());
    let loaded = load.wait_with_output()?;
    written?;
    if !loaded.status.success() {
        let stderr = String::from_utf8_lossy(&loaded.stderr).trim().to_string();
        return Err(PmError::Tmux(stderr));
    }
    let pasted = run_tmux(
        server,
        &[
            "paste-buffer",
            "-b",
            &buffer,
            "-d",
            "-p",
            "-r",
            "-t",
            &exact(pane),
        ],
    );
    if let Err(e) = pasted {
        // The buffer holds the user's text; don't leave it on the server.
        let _ = run_tmux(server, &["delete-buffer", "-b", &buffer]);
        return Err(e);
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    send_key(server, pane, "Enter")
}

/// Whether `pane` is in a mode — copy mode, most often — where keys drive
/// the mode rather than reach the program.
pub fn in_mode(server: Option<&str>, pane: &str) -> Result<bool> {
    let out = run_tmux(
        server,
        &[
            "display-message",
            "-p",
            "-t",
            &exact(pane),
            "#{pane_in_mode}",
        ],
    )?;
    Ok(out == "1")
}

/// Leave the mode `pane` is in.
pub fn cancel_mode(server: Option<&str>, pane: &str) -> Result<()> {
    run_tmux(server, &["send-keys", "-X", "-t", &exact(pane), "cancel"])?;
    Ok(())
}
