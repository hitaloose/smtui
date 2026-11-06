//! Terminal mode: one local shell per tab, rendered with `tui-term`.

mod input;
mod session;

use std::path::PathBuf;

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
use super::{EventResult, KeyHint, Mode, ModeCtx, Shared, TabInfo, requests::OpenTerminal};
use crate::event::{AppEvent, EventSender};

struct TermTab {
    number: u64,
    cwd: PathBuf,
    /// Folder name for tabs opened in a specific folder.
    label: Option<String>,
    session: Option<TerminalSession>,
    /// Why the shell is not running (exited, or failed to start).
    notice: Option<String>,
    /// Lines scrolled back from the bottom.
    scroll: usize,
    /// The shell starts on the first render, once the real size is known.
    needs_start: bool,
}

pub struct TerminalMode {
    cwd: PathBuf,
    tx: EventSender,
    tabs: Vec<TermTab>,
    active: usize,
    next_number: u64,
    next_session: u64,
    area: Rect,
}

impl TerminalMode {
    pub fn new(cwd: PathBuf, tx: EventSender) -> Self {
        let mut mode = Self { cwd, tx, tabs: Vec::new(), active: 0, next_number: 1, next_session: 1, area: Rect::new(0, 0, 80, 24) };
        mode.open_tab();
        mode
    }

    fn open_tab(&mut self) {
        let cwd = self.cwd.clone();
        self.open_tab_at(cwd, None);
    }

    fn open_tab_at(&mut self, cwd: PathBuf, label: Option<String>) {
        let number = self.next_number;
        self.next_number += 1;
        self.tabs.push(TermTab { number, cwd, label, session: None, notice: None, scroll: 0, needs_start: true });
        self.active = self.tabs.len() - 1;
    }

    fn start_shell(&mut self, idx: usize) {
        let (rows, cols) = (self.area.height, self.area.width);
        let session_id = self.next_session;
        self.next_session += 1;
        let tab = &mut self.tabs[idx];
        tab.scroll = 0;
        tab.needs_start = false;
        match TerminalSession::spawn(session_id, &tab.cwd, rows, cols, self.tx.clone()) {
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

    fn tab_by_session(&mut self, id: u64) -> Option<&mut TermTab> {
        self.tabs.iter_mut().find(|t| t.session.as_ref().is_some_and(|s| s.id() == id))
    }
}

impl Mode for TerminalMode {
    fn name(&self) -> &str {
        "Terminal"
    }

    fn icon(&self) -> &str {
        ">_"
    }

    fn tabs(&self) -> Vec<TabInfo> {
        self.tabs
            .iter()
            .map(|t| TabInfo { title: t.label.clone().unwrap_or_else(|| format!("shell {}", t.number)) })
            .collect()
    }

    fn active_tab(&self) -> usize {
        self.active
    }

    fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active = idx;
        }
    }

    fn new_tab(&mut self, _ctx: &mut ModeCtx) {
        self.open_tab();
    }

    fn close_tab(&mut self, idx: usize, _ctx: &mut ModeCtx) {
        if self.tabs.len() > 1 && idx < self.tabs.len() {
            self.tabs.remove(idx); // dropping the session kills the shell
            if self.active > idx || self.active >= self.tabs.len() {
                self.active = self.active.saturating_sub(1);
            }
        }
    }

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
            if screen.hide_cursor() || tab.scroll > 0 {
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

    fn handle_mouse(&mut self, ev: MouseEvent, _ctx: &mut ModeCtx) -> EventResult {
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
            _ => return EventResult::Ignored,
        }
        EventResult::Consumed
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
                    tab.notice = Some("Process exited. Press Enter to start a new shell.".into());
                }
            }
        }
        EventResult::Consumed
    }

    fn handle_request(&mut self, request: &dyn std::any::Any, ctx: &mut ModeCtx) -> bool {
        let Some(req) = request.downcast_ref::<OpenTerminal>() else { return false };
        let label = req.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        if req.cwd.is_dir() {
            self.open_tab_at(req.cwd.clone(), Some(label));
        } else {
            // e.g. the in-memory demo file system: fall back to the start folder.
            ctx.toast(format!("{} does not exist on disk", req.cwd.display()), format!("Opened in {}", self.cwd.display()));
            self.open_tab();
        }
        true
    }

    fn status(&self, shared: &Shared) -> String {
        shared.host.label()
    }

    fn hints(&self, shared: &Shared) -> Vec<KeyHint> {
        vec![KeyHint::new(crate::keys::display(&shared.config.leader), "Shortcuts").opens_shortcuts()]
    }

    fn captures_input(&self) -> bool {
        true
    }
}
