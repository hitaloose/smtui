//! One shell running in a pseudo-terminal, with its screen emulated by `vt100`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::event::EventSender;

const SCROLLBACK: usize = 5_000;
/// A queued command is typed once the shell has been quiet this long (its prompt is drawn)…
const READY_QUIET: Duration = Duration::from_millis(300);
/// …or after this long, whatever happens.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

pub enum TermEvent {
    /// New output is ready to draw.
    Output,
    Exited(u64),
    /// A program asked for text to go to the clipboard (OSC 52), already base64-decoded.
    Clipboard(String),
}

/// Answers the terminal queries vt100 does not handle, collects OSC 52 requests and reads
/// the exit codes the shell reports (OSC 133;D, see [`PROMPT_HOOK`]).
#[derive(Default)]
pub struct TermCallbacks {
    replies: Vec<u8>,
    clipboard: Vec<Vec<u8>>,
    /// How many exit codes were reported so far, and the last one.
    exits: u64,
    last_exit: i32,
}

/// Run by bash before each prompt (`PROMPT_COMMAND`): reports `$?` as OSC 133;D, the
/// sequence VS Code and other terminals use, and keeps `$?` for the user's own prompt.
const PROMPT_HOOK: &str = r#"__smtui_s=$?; printf '\033]133;D;%s\007' "$__smtui_s"; (exit $__smtui_s)"#;

impl vt100::Callbacks for TermCallbacks {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let reply = match (i1, c, first) {
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                format!("\x1b[{};{}R", row + 1, col + 1)
            }
            (None, 'n', 5) => "\x1b[0n".into(),
            (None, 'c', 0) => "\x1b[?62;22c".into(),
            (Some(b'>'), 'c', 0) => "\x1b[>0;10;1c".into(),
            _ => return,
        };
        self.replies.extend_from_slice(reply.as_bytes());
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _ty: &[u8], data: &[u8]) {
        self.clipboard.push(data.to_vec());
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        if let [b"133", kind, rest @ ..] = params
            && kind.first() == Some(&b'D')
        {
            let code = rest.first().and_then(|c| std::str::from_utf8(c).ok()).and_then(|c| c.parse().ok());
            self.exits += 1;
            self.last_exit = code.unwrap_or(0);
        }
    }
}

type Parser = vt100::Parser<TermCallbacks>;
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct TerminalSession {
    id: u64,
    parser: Arc<Mutex<Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: SharedWriter,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    /// Set while an `Output` event is queued, so a busy program does not flood the channel.
    pending: Arc<AtomicBool>,
}

pub fn default_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
}

impl TerminalSession {
    /// `typed`: text to type into the shell once it is ready (e.g. a command from "Run…").
    pub fn spawn(id: u64, cwd: &Path, rows: u16, cols: u16, typed: Option<String>, tx: EventSender) -> Result<Self> {
        let (rows, cols) = (rows.max(2), cols.max(2));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("failed to open a pseudo-terminal")?;
        let shell = default_shell();
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        if shell.rsplit('/').next() == Some("bash") {
            // Keeps a PROMPT_COMMAND the user exported; one set in ~/.bashrc replaces ours,
            // which only costs the exit codes on finished terminal tabs.
            let hook = match std::env::var("PROMPT_COMMAND") {
                Ok(theirs) if !theirs.trim().is_empty() => format!("{PROMPT_HOOK}; {theirs}"),
                _ => PROMPT_HOOK.to_string(),
            };
            cmd.env("PROMPT_COMMAND", hook);
        }
        let child = pair.slave.spawn_command(cmd).with_context(|| format!("failed to start {shell}"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer: SharedWriter = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser = Arc::new(Mutex::new(Parser::new_with_callbacks(rows, cols, SCROLLBACK, TermCallbacks::default())));
        let pending = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        // Milliseconds since `started` of the last output; 0 = nothing yet.
        let last_output = Arc::new(AtomicU64::new(0));

        if let Some(text) = typed {
            // Typing before the prompt is drawn can be eaten by shell start-up (rc files,
            // prompts that query the terminal), so wait until the output settles.
            let (last, w) = (last_output.clone(), writer.clone());
            std::thread::Builder::new().name(format!("pty-{id}-typed")).spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                    let elapsed = started.elapsed();
                    let last = Duration::from_millis(last.load(Ordering::Acquire));
                    let quiet = !last.is_zero() && elapsed.saturating_sub(last) >= READY_QUIET;
                    if quiet || elapsed >= READY_TIMEOUT {
                        break;
                    }
                }
                let mut w = w.lock().expect("writer lock");
                let _ = w.write_all(format!("{text}\r").as_bytes()).and_then(|_| w.flush());
            })?;
        }

        let (p, flag, w, last) = (parser.clone(), pending.clone(), writer.clone(), last_output.clone());
        std::thread::Builder::new().name(format!("pty-{id}")).spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        last.store((started.elapsed().as_millis() as u64).max(1), Ordering::Release);
                        let (replies, clips) = {
                            let mut parser = p.lock().expect("parser lock");
                            parser.process(&buf[..n]);
                            let cb = parser.callbacks_mut();
                            (std::mem::take(&mut cb.replies), std::mem::take(&mut cb.clipboard))
                        };
                        if !replies.is_empty() {
                            let mut w = w.lock().expect("writer lock");
                            let _ = w.write_all(&replies).and_then(|_| w.flush());
                        }
                        for data in clips {
                            use base64::Engine;
                            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                                tx.send(TermEvent::Clipboard(String::from_utf8_lossy(&bytes).into_owned()));
                            }
                        }
                        if !flag.swap(true, Ordering::AcqRel) {
                            tx.send(TermEvent::Output);
                        }
                    }
                }
            }
            tx.send(TermEvent::Exited(id));
        })?;

        Ok(Self { id, parser, master: pair.master, writer, child, size: (rows, cols), pending })
    }

    /// The folder the shell (or the program in the foreground) is in right now.
    pub fn cwd(&self) -> Option<PathBuf> {
        let foreground = self.master.process_group_leader().and_then(|p| u32::try_from(p).ok());
        [foreground, self.child.process_id()].into_iter().flatten().find_map(process_cwd)
    }

    /// True when the shell itself is in the foreground (at its prompt), not a program it
    /// started (vim, `tail -f`, a nested shell…). Unknown counts as busy.
    pub fn is_idle(&self) -> bool {
        match (self.master.process_group_leader(), self.child.process_id()) {
            (Some(fg), Some(shell)) => u32::try_from(fg).ok() == Some(shell),
            _ => false,
        }
    }

    /// Process group in the foreground, when it is not the shell (cheap: one ioctl).
    pub fn foreground_job(&self) -> Option<u32> {
        if self.is_idle() {
            return None;
        }
        Some(self.master.process_group_leader().and_then(|p| u32::try_from(p).ok()).unwrap_or(0))
    }

    /// `(count, last)` of the exit codes the shell reported (bash only).
    pub fn exits(&self) -> (u64, i32) {
        let p = self.screen();
        let cb = p.callbacks();
        (cb.exits, cb.last_exit)
    }

    /// A full-screen program (vim, less, htop) is drawing.
    pub fn alternate_screen(&self) -> bool {
        self.screen().screen().alternate_screen()
    }

    /// The shell's own folder, only where that is cheap to read (`/proc` on Linux).
    pub fn shell_cwd(&self) -> Option<PathBuf> {
        let pid = self.child.process_id()?;
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    /// Replaces whatever is typed at the prompt with `line` and runs it. Only call this
    /// when [`Self::is_idle`]: Ctrl-E Ctrl-U clears the line in bash, zsh and fish.
    pub fn type_line(&mut self, line: &str) {
        self.write(format!("\x05\x15{line}\r").as_bytes());
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn screen(&self) -> MutexGuard<'_, Parser> {
        self.parser.lock().expect("parser lock")
    }

    pub fn output_drawn(&self) {
        self.pending.store(false, Ordering::Release);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(2));
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.screen().screen_mut().set_size(rows, cols);
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut w = self.writer.lock().expect("writer lock");
        if let Err(e) = w.write_all(bytes).and_then(|_| w.flush()) {
            tracing::debug!(id = self.id, error = %e, "write to pty failed");
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Command line of process `pid` (`tail -f app.log`): `/proc` on Linux, `ps` elsewhere.
pub fn process_cmdline(pid: u32) -> Option<String> {
    let line = match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(raw) => raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(String::from_utf8_lossy).collect::<Vec<_>>().join(" "),
        Err(_) => {
            let out = std::process::Command::new("ps")
                .args(["-o", "command=", "-p", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    };
    let line: String = line.chars().filter(|c| !c.is_control()).collect();
    (!line.trim().is_empty()).then(|| line.trim().to_string())
}

/// Current folder of process `pid`: `/proc` on Linux, `lsof` elsewhere (macOS, for development).
fn process_cwd(pid: u32) -> Option<PathBuf> {
    if let Ok(path) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
        return Some(path);
    }
    let out = std::process::Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix('n')).map(PathBuf::from).filter(|p| p.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exit_codes_reported_by_the_prompt_hook() {
        let mut p = Parser::new_with_callbacks(5, 40, 0, TermCallbacks::default());
        p.process(b"ls\r\n\x1b]133;D;0\x07$ false\r\n\x1b]133;D;1\x1b\\$ ");
        let cb = p.callbacks();
        assert_eq!((cb.exits, cb.last_exit), (2, 1));
        // Other OSC sequences (window titles) are ignored.
        p.process(b"\x1b]0;title\x07");
        assert_eq!(p.callbacks().exits, 2);
    }
}
