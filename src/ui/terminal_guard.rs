//! Puts the terminal in TUI mode and guarantees it is restored on exit and on panic.

use std::io::{self, Stdout};

use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub struct TerminalGuard {
    pub terminal: Tui,
}

impl TerminalGuard {
    pub fn new() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(e) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste) {
            restore();
            return Err(e);
        }
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        terminal.clear()?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
        let _ = self.terminal.show_cursor();
    }
}

/// Best effort: every step runs even if an earlier one fails.
pub fn restore() {
    let mut stdout = io::stdout();
    let _ = execute!(stdout, DisableBracketedPaste);
    let _ = execute!(stdout, DisableMouseCapture);
    let _ = execute!(stdout, LeaveAlternateScreen);
    let _ = execute!(stdout, crossterm::cursor::Show);
    let _ = disable_raw_mode();
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}
