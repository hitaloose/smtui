//! Terminal pane: one local shell per tab, rendered with `tui-term`. It follows the Files
//! pane above it: when the folder there changes, the shell is sent `cd` (see [`TerminalMode::follow`]).

mod input;
mod session;

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::Paragraph,
};
use tui_term::widget::{Cursor, PseudoTerminal};

use self::session::{TermEvent, TerminalSession};
use super::{EventResult, MenuItem, ModeCtx, Pane, Shared, requests::ShowInFiles};
use crate::event::{AppEvent, EventSender};

mod action {
    pub const SHOW_IN_FILES: u32 = 1;
    pub const COPY_PATH: u32 = 2;
}

struct TermTab {
    /// Where the shell starts, then the last folder it was sent to.
    cwd: PathBuf,
    session: Option<TerminalSession>,
    /// Why the shell is not running (exited, or failed to start).
    notice: Option<String>,
    /// Lines scrolled back from the bottom.
    scroll: usize,
    /// The shell starts on the first render, once the real size is known.
    needs_start: bool,
    /// Command typed into the shell when it first starts ("Run…" from Files).
    typed: Option<String>,
    /// Folder to `cd` into once the program in the foreground exits.
    pending: Option<PathBuf>,
    /// The program in the foreground (process group, name), refreshed by [`TerminalMode::poll`].
    busy: Option<(u32, String)>,
}

pub struct TerminalMode {
    tx: EventSender,
    tabs: Vec<TermTab>,
    active: usize,
    next_session: u64,
    area: Rect,
    /// Keys go here: the cursor is only drawn when focused.
    pub focused: bool,
}

impl TerminalMode {
    pub fn new(cwd: PathBuf, tx: EventSender) -> Self {
        let mut mode = Self { tx, tabs: Vec::new(), active: 0, next_session: 1, area: Rect::new(0, 0, 80, 24), focused: false };
        mode.open_tab(cwd, None);
        mode
    }

    /// Adds a tab at the end and makes it active; `typed` is run once the shell is ready.
    pub fn open_tab(&mut self, cwd: PathBuf, typed: Option<String>) {
        self.tabs.push(TermTab {
            cwd,
            session: None,
            notice: None,
            scroll: 0,
            needs_start: true,
            typed,
            pending: None,
            busy: None,
        });
        self.active = self.tabs.len() - 1;
    }

    pub fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() > 1 && idx < self.tabs.len() {
            self.tabs.remove(idx); // dropping the session kills the shell
            if self.active > idx || self.active >= self.tabs.len() {
                self.active = self.active.saturating_sub(1);
            }
        }
    }

    pub fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active = idx;
        }
    }

    /// Makes the shell of tab `idx` follow the Files pane into `dir`: right away when the
    /// shell is at its prompt, otherwise once the program in the foreground exits.
    pub fn follow(&mut self, idx: usize, dir: &Path) {
        // e.g. the in-memory demo file system.
        if !dir.is_dir() {
            return;
        }
        let Some(tab) = self.tabs.get_mut(idx) else { return };
        tab.pending = None;
        match &mut tab.session {
            Some(session) if !session.is_idle() => tab.pending = Some(dir.to_path_buf()),
            Some(session) => {
                send_cd(session, dir);
                tab.cwd = dir.to_path_buf();
                tab.scroll = 0;
            }
            // Starts (or restarts) there.
            None => tab.cwd = dir.to_path_buf(),
        }
    }

    /// Refreshes which tabs run a program and sends pending `cd`s to shells that are back
    /// at their prompt. Returns true when something visible changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for tab in &mut self.tabs {
            let job = tab.session.as_ref().and_then(TerminalSession::foreground_job);
            if job != tab.busy.as_ref().map(|(pgid, _)| *pgid) {
                // The name is looked up only when the foreground job changes.
                tab.busy = job.map(|pgid| (pgid, session::process_name(pgid).unwrap_or_else(|| "a program".into())));
                changed = true;
            }
            if tab.busy.is_none()
                && let (Some(dir), Some(session)) = (tab.pending.take(), &mut tab.session)
            {
                send_cd(session, &dir);
                tab.cwd = dir;
                tab.scroll = 0;
                changed = true;
            }
        }
        changed
    }

    /// Types `command` into the shell of tab `idx`. False when a program is running there
    /// (or the shell has exited), so the caller can open a new tab instead.
    pub fn run_in(&mut self, idx: usize, command: &str) -> bool {
        let Some(tab) = self.tabs.get_mut(idx) else { return false };
        if tab.needs_start {
            tab.typed = Some(command.to_string());
            return true;
        }
        match &mut tab.session {
            Some(session) if session.is_idle() => {
                session.type_line(command);
                tab.scroll = 0;
                true
            }
            _ => false,
        }
    }

    /// The program running in tab `idx`, if any.
    pub fn busy(&self, idx: usize) -> Option<&str> {
        self.tabs.get(idx).and_then(|t| t.busy.as_ref()).map(|(_, name)| name.as_str())
    }

    /// Folder the shell of tab `idx` waits to `cd` into.
    pub fn pending(&self, idx: usize) -> Option<&Path> {
        self.tabs.get(idx).and_then(|t| t.pending.as_deref())
    }

    /// The folder shown in the terminal header: the shell's own where cheap to read,
    /// else the last folder it was sent to.
    pub fn shown_dir(&self, idx: usize) -> PathBuf {
        let tab = &self.tabs[idx];
        tab.session.as_ref().and_then(TerminalSession::shell_cwd).unwrap_or_else(|| tab.cwd.clone())
    }

    fn start_shell(&mut self, idx: usize) {
        let (rows, cols) = (self.area.height, self.area.width);
        let session_id = self.next_session;
        self.next_session += 1;
        let tab = &mut self.tabs[idx];
        tab.scroll = 0;
        tab.needs_start = false;
        tab.pending = None;
        match TerminalSession::spawn(session_id, &tab.cwd, rows, cols, tab.typed.take(), self.tx.clone()) {
            Ok(s) => {
                tab.session = Some(s);
                tab.notice = None;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to start shell");
                tab.session = None;
                tab.notice = Some(format!("{e:#}. Press Enter to try again."));
            }
        }
    }

    /// The active shell's current folder, falling back to the folder the tab started in.
    fn current_dir(&self) -> PathBuf {
        let tab = &self.tabs[self.active];
        tab.session.as_ref().and_then(TerminalSession::cwd).unwrap_or_else(|| tab.cwd.clone())
    }

    fn tab_by_session(&mut self, id: u64) -> Option<&mut TermTab> {
        self.tabs.iter_mut().find(|t| t.session.as_ref().is_some_and(|s| s.id() == id))
    }
}

/// ` cd ..`, ` cd -- name` or ` cd -- '/abs/path'`: the shortest that works from where the
/// shell is. The leading space keeps it out of the history (HISTCONTROL=ignorespace, zsh's
/// HIST_IGNORE_SPACE). Skipped when the shell is already there.
fn send_cd(session: &mut TerminalSession, dir: &Path) {
    let here = session.cwd();
    if here.as_deref() == Some(dir) {
        return;
    }
    session.type_line(&cd_line(here.as_deref(), dir));
}

fn cd_line(here: Option<&Path>, dir: &Path) -> String {
    let quote = |s: &str| crate::modes::files::ops::shell_quote(s).into_owned();
    if let Some(here) = here {
        if here.parent() == Some(dir) {
            return " cd ..".into();
        }
        if let (Some(parent), Some(name)) = (dir.parent(), dir.file_name())
            && parent == here
        {
            let name = name.to_string_lossy();
            // `-` alone means the previous folder to `cd`, even after `--` in some shells.
            if name != "-" {
                return format!(" cd -- {}", quote(&name));
            }
        }
    }
    format!(" cd -- {}", quote(&dir.to_string_lossy()))
}

impl Pane for TerminalMode {
    fn render(&mut self, f: &mut Frame, area: Rect, shared: &Shared) {
        self.area = area;
        if self.tabs[self.active].needs_start {
            self.start_shell(self.active);
        }
        let theme = &shared.theme;
        let tab = &mut self.tabs[self.active];
        let mut notice_area = area;
        if let Some(session) = &mut tab.session {
            session.resize(area.height, area.width);
            let mut parser = session.screen();
            parser.screen_mut().set_scrollback(tab.scroll);
            tab.scroll = parser.screen().scrollback();
            let screen = parser.screen();
            let mut cursor = Cursor::default().style(Style::new().bg(theme.accent).fg(theme.bg));
            if screen.hide_cursor() || tab.scroll > 0 || !self.focused {
                cursor.hide();
            }
            f.render_widget(
                PseudoTerminal::new(screen).cursor(cursor).style(Style::new().bg(theme.bg).fg(theme.fg)),
                area,
            );
            drop(parser);
            session.output_drawn();
            let buf = f.buffer_mut();
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    let cell = &mut buf[(x, y)];
                    if cell.bg == ratatui::style::Color::Reset {
                        cell.bg = theme.bg;
                    }
                    if cell.fg == ratatui::style::Color::Reset {
                        cell.fg = theme.fg;
                    }
                }
            }
            if tab.scroll > 0 {
                let label = format!(" scrollback -{} ", tab.scroll);
                let w = label.len() as u16;
                let r = Rect::new(area.right().saturating_sub(w + 1), area.y, w, 1);
                f.render_widget(Paragraph::new(Span::styled(label, Style::new().bg(theme.accent_tint).fg(theme.accent))), r);
            }
            notice_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        }
        if let Some(notice) = &tab.notice {
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {notice}"),
                    Style::new().bg(theme.raised_bg).fg(theme.fg_muted).add_modifier(Modifier::ITALIC),
                )),
                Rect::new(notice_area.x, notice_area.bottom().saturating_sub(1), notice_area.width, 1.min(notice_area.height)),
            );
        }
    }

    fn handle_key(&mut self, key: KeyEvent, _ctx: &mut ModeCtx) -> EventResult {
        let idx = self.active;
        let tab = &mut self.tabs[idx];
        if tab.notice.is_some() {
            if key.code == KeyCode::Enter {
                self.start_shell(idx);
            }
            return EventResult::Consumed;
        }
        if let Some(session) = &mut tab.session {
            tab.scroll = 0;
            let app_cursor = session.screen().screen().application_cursor();
            session.write(&input::encode_key(&key, app_cursor));
        }
        EventResult::Consumed
    }

    fn handle_paste(&mut self, text: &str, _ctx: &mut ModeCtx) -> EventResult {
        let tab = &mut self.tabs[self.active];
        if let Some(session) = &mut tab.session {
            tab.scroll = 0;
            let bracketed = session.screen().screen().bracketed_paste();
            let mut bytes = Vec::new();
            if bracketed {
                bytes.extend_from_slice(b"\x1b[200~");
            }
            bytes.extend_from_slice(text.as_bytes());
            if bracketed {
                bytes.extend_from_slice(b"\x1b[201~");
            }
            session.write(&bytes);
        }
        EventResult::Consumed
    }

    fn handle_mouse(&mut self, ev: MouseEvent, ctx: &mut ModeCtx) -> EventResult {
        let area = self.area;
        if !area.contains((ev.column, ev.row).into()) {
            return EventResult::Ignored;
        }
        let tab = &mut self.tabs[self.active];
        let Some(session) = &mut tab.session else { return EventResult::Ignored };
        let (mode, encoding, alternate) = {
            let p = session.screen();
            let s = p.screen();
            (s.mouse_protocol_mode(), s.mouse_protocol_encoding(), s.alternate_screen())
        };
        if mode != vt100::MouseProtocolMode::None {
            let (x, y) = (ev.column - area.x, ev.row - area.y);
            if let Some(bytes) = input::encode_mouse(&ev, x, y, mode, encoding) {
                session.write(&bytes);
            }
            return EventResult::Consumed;
        }
        match ev.kind {
            MouseEventKind::ScrollUp if !alternate => tab.scroll += 3,
            MouseEventKind::ScrollDown if !alternate => tab.scroll = tab.scroll.saturating_sub(3),
            // Full-screen programs without mouse support get arrow keys, like most terminals do.
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = ev.kind == MouseEventKind::ScrollUp;
                let app_cursor = session.screen().screen().application_cursor();
                let key = KeyEvent::from(if up { KeyCode::Up } else { KeyCode::Down });
                let bytes = input::encode_key(&key, app_cursor);
                session.write(&bytes.repeat(3));
            }
            // Right click on a plain shell (the program did not ask for the mouse).
            MouseEventKind::Down(crossterm::event::MouseButton::Right) => {
                let leader = crate::keys::display(&ctx.shared.config.leader);
                ctx.open_menu(ev.column, ev.row, vec![
                    MenuItem::action("Show shell folder in Files", action::SHOW_IN_FILES).with_hint(format!("{leader} f")),
                    MenuItem::action("Copy current folder path", action::COPY_PATH),
                ]);
            }
            _ => return EventResult::Ignored,
        }
        EventResult::Consumed
    }

    fn on_action(&mut self, action: u32, ctx: &mut ModeCtx) {
        match action {
            action::SHOW_IN_FILES => ctx.request(ShowInFiles { path: self.current_dir() }),
            action::COPY_PATH => {
                let path = self.current_dir().to_string_lossy().into_owned();
                let msg = match crate::clipboard::copy(&path, ctx.shared.host.in_tmux) {
                    crate::clipboard::CopyOutcome::Failed => "Could not access the clipboard. Copy it manually:",
                    _ => "Folder path copied",
                };
                ctx.toast(msg, path);
            }
            _ => {}
        }
    }

    fn shortcut_items(&self) -> Vec<MenuItem> {
        vec![MenuItem::action("Show shell folder in Files", action::SHOW_IN_FILES).with_hint("f")]
    }

    fn handle_app_event(&mut self, ev: &AppEvent, ctx: &mut ModeCtx) -> EventResult {
        let Some(ev) = ev.get::<TermEvent>() else { return EventResult::Ignored };
        match ev {
            TermEvent::Output => {}
            TermEvent::Clipboard(text) => {
                crate::clipboard::copy(text, ctx.shared.host.in_tmux);
            }
            TermEvent::Exited(id) => {
                if let Some(tab) = self.tab_by_session(*id) {
                    tab.session = None;
                    tab.busy = None;
                    // A new shell starts in the folder Files moved to meanwhile.
                    if let Some(dir) = tab.pending.take() {
                        tab.cwd = dir;
                    }
                    tab.notice = Some("Process exited. Press Enter to start a new shell.".into());
                }
            }
        }
        EventResult::Consumed
    }

    fn status(&self, shared: &Shared) -> String {
        // The demo's shell runs in a real folder of this machine: keep it off screen.
        if shared.demo {
            return shared.host.label();
        }
        format!("{} · shell in {}", shared.host.label(), crate::ui::bars::tilde(&self.shown_dir(self.active)))
    }

    fn captures_input(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_takes_the_shortest_form() {
        let p = Path::new;
        assert_eq!(cd_line(Some(p("/srv/app")), p("/srv")), " cd ..");
        assert_eq!(cd_line(Some(p("/srv")), p("/srv/app")), " cd -- app");
        assert_eq!(cd_line(Some(p("/srv")), p("/srv/my app")), " cd -- 'my app'");
        assert_eq!(cd_line(Some(p("/srv")), p("/srv/-")), " cd -- /srv/-");
        assert_eq!(cd_line(Some(p("/srv")), p("/etc/nginx")), " cd -- /etc/nginx");
        assert_eq!(cd_line(None, p("/var/log")), " cd -- /var/log");
    }
}
