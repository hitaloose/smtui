//! Copying text to the clipboard of the user's *local* machine.
//!
//! smtui usually runs on a server inside tmux over SSH, so the only way to reach the
//! local clipboard is through the terminal: OSC 52. Inside tmux we also hand the text
//! to `tmux load-buffer -w`, which forwards it with tmux's own OSC 52 support.

use std::io::Write;
use std::process::{Command, Stdio};

use base64::Engine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    /// A clipboard tool confirmed the copy.
    Copied,
    /// OSC 52 was sent; whether it lands depends on the terminal, which cannot be detected.
    Sent,
    Failed,
}

pub fn copy(text: &str, in_tmux: bool) -> CopyOutcome {
    let mut confirmed = false;
    if in_tmux {
        confirmed |= tmux_load_buffer(text);
    }
    #[cfg(feature = "local-clipboard")]
    {
        confirmed |= arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_owned())).is_ok();
    }
    let sent = write_osc52(text, in_tmux).is_ok();
    match (confirmed, sent) {
        (true, _) => CopyOutcome::Copied,
        (false, true) => CopyOutcome::Sent,
        (false, false) => CopyOutcome::Failed,
    }
}

pub fn osc52_sequence(text: &str, in_tmux: bool) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let seq = format!("\x1b]52;c;{b64}\x07");
    if in_tmux {
        // DCS passthrough (needs `allow-passthrough on` in tmux >= 3.3); inner ESCs are doubled.
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
    } else {
        seq
    }
}

fn write_osc52(text: &str, in_tmux: bool) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(osc52_sequence(text, in_tmux).as_bytes())?;
    out.flush()
}

fn tmux_load_buffer(text: &str) -> bool {
    let child = Command::new("tmux")
        .args(["load-buffer", "-w", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    let wrote = child.stdin.take().is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    child.wait().is_ok_and(|s| s.success()) && wrote
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_osc52() {
        assert_eq!(osc52_sequence("hi", false), "\x1b]52;c;aGk=\x07");
        assert_eq!(osc52_sequence("hi", true), "\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\");
    }
}
