//! One shell running in a pseudo-terminal, with its screen emulated by `vt100`.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::event::EventSender;

const SCROLLBACK: usize = 5_000;

pub enum TermEvent {
    /// New output is ready to draw.
    Output,
    Exited(u64),
    /// A program asked for text to go to the clipboard (OSC 52), already base64-decoded.
    Clipboard(String),
}

/// Answers the terminal queries vt100 does not handle and collects OSC 52 requests.
#[derive(Default)]
pub struct TermCallbacks {
    replies: Vec<u8>,
    clipboard: Vec<Vec<u8>>,
}

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
    pub fn spawn(id: u64, cwd: &Path, rows: u16, cols: u16, tx: EventSender) -> Result<Self> {
        let (rows, cols) = (rows.max(2), cols.max(2));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("failed to open a pseudo-terminal")?;
        let shell = default_shell();
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cmd).with_context(|| format!("failed to start {shell}"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer: SharedWriter = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser = Arc::new(Mutex::new(Parser::new_with_callbacks(rows, cols, SCROLLBACK, TermCallbacks::default())));
        let pending = Arc::new(AtomicBool::new(false));

        let (p, flag, w) = (parser.clone(), pending.clone(), writer.clone());
        std::thread::Builder::new().name(format!("pty-{id}")).spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
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
