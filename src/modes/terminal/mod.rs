//! Terminal pane: every tab has its own terminals. The first one is the navigation shell: it
//! follows the Files pane above (see [`TerminalMode::follow`]). A command still running in it
//! after [`PROMOTE_AFTER`], or one that takes the whole screen, is promoted: that terminal
//! becomes a command tab of its own and a fresh navigation shell takes its place. The process
//! never moves; the navigation role does.

mod input;
mod session;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// A command still running in the navigation shell after this long gets its own terminal.
pub const PROMOTE_AFTER: Duration = Duration::from_secs(1);

mod action {
    pub const SHOW_IN_FILES: u32 = 1;
    pub const COPY_PATH: u32 = 2;
    pub const CLOSE_CONFIRMED: u32 = 3;
}
pub use action::CLOSE_CONFIRMED;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// Follows Files; always the first terminal of a tab, never closed.
    Nav,
    /// A promoted command, or a shell opened by hand; named after what runs in it.
    Extra { label: String },
}

/// What a terminal tab shows next to its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermState {
    Idle,
    Running,
    /// The last command finished; its exit code when the shell reported it (bash).
    Done(Option<i32>),
}

/// One terminal tab, for the terminal header.
#[derive(Debug, Clone)]
pub struct TermTabInfo {
    pub label: String,
    pub nav: bool,
    pub state: TermState,
}

struct Term {
    kind: Kind,
    /// Where the shell starts, then (navigation shell) the last folder it was sent to.
    cwd: PathBuf,
    session: Option<TerminalSession>,
    /// Why the shell is not running (exited, or failed to start).
    notice: Option<String>,
    /// Lines scrolled back from the bottom.
    scroll: usize,
    /// The shell starts on the first render, once the real size is known.
    needs_start: bool,
    /// Command typed into the shell when it first starts.
    typed: Option<String>,
    /// Navigation shell: folder to `cd` into once the program in the foreground exits.
    pending: Option<PathBuf>,
    /// The foreground job (process group) and since when, refreshed by [`TerminalMode::poll`].
    job: Option<(u32, Instant)>,
    /// Exit codes reported before the current job started, to spot its own.
    exits_at_start: u64,
    state: TermState,
}

impl Term {
    fn new(kind: Kind, cwd: PathBuf, typed: Option<String>) -> Self {
        Self {
            kind,
            cwd,
            session: None,
            notice: None,
            scroll: 0,
            needs_start: true,
            typed,
            pending: None,
            job: None,
            exits_at_start: 0,
            state: TermState::Idle,
        }
    }

    fn is_nav(&self) -> bool {
        self.kind == Kind::Nav
    }

    fn running(&self) -> bool {
        self.job.is_some()
    }
}

/// The terminals of one workspace tab: `terms[0]` is the navigation shell.
struct Group {
    terms: Vec<Term>,
    active: usize,
}

impl Group {
    fn nav(&mut self) -> &mut Term {
        &mut self.terms[0]
    }
}

/// Something the app may want to tell the user after [`TerminalMode::poll`].
pub struct Promoted {
    pub tab: usize,
    pub label: String,
}

pub struct TerminalMode {
    tx: EventSender,
    groups: Vec<Group>,
    active: usize,
    next_session: u64,
    next_shell: u64,
    area: Rect,
    /// Keys go here: the cursor is only drawn when focused.
    pub focused: bool,
    /// Terminal waiting for the "close a running command" confirmation: (tab, session id).
    pending_close: Option<(usize, u64)>,
}

impl TerminalMode {
    pub fn new(cwd: PathBuf, tx: EventSender) -> Self {
        let mut mode = Self {
            tx,
            groups: Vec::new(),
            active: 0,
            next_session: 1,
            next_shell: 1,
            area: Rect::new(0, 0, 80, 24),
            focused: false,
            pending_close: None,
        };
        mode.open_tab(cwd, None);
        mode
    }

    // ---- Workspace tabs ----

    /// Adds a tab at the end, with its navigation shell, and makes it active; `typed` runs
    /// once the shell is ready.
    pub fn open_tab(&mut self, cwd: PathBuf, typed: Option<String>) {
        self.groups.push(Group { terms: vec![Term::new(Kind::Nav, cwd, typed)], active: 0 });
        self.active = self.groups.len() - 1;
    }

    pub fn close_tab(&mut self, idx: usize) {
        if self.groups.len() > 1 && idx < self.groups.len() {
            self.groups.remove(idx); // dropping the sessions kills the shells
            if self.active > idx || self.active >= self.groups.len() {
                self.active = self.active.saturating_sub(1);
            }
            self.pending_close = None;
        }
    }

    pub fn select_tab(&mut self, idx: usize) {
        if idx < self.groups.len() {
            self.active = idx;
        }
    }

    /// Commands running in tab `idx` (promoted ones and hand-opened shells).
    pub fn running(&self, idx: usize) -> usize {
        self.groups.get(idx).map_or(0, |g| g.terms.iter().filter(|t| !t.is_nav() && t.running()).count())
    }

    // ---- Terminal tabs of the active workspace ----

    pub fn term_tabs(&self, idx: usize) -> Vec<TermTabInfo> {
        let g = &self.groups[idx];
        g.terms
            .iter()
            .enumerate()
            .map(|(i, t)| TermTabInfo {
                label: match &t.kind {
                    Kind::Nav => crate::ui::bars::tilde(&self.term_dir(idx, i)),
                    Kind::Extra { label } => label.clone(),
                },
                nav: t.is_nav(),
                state: if t.running() { TermState::Running } else { t.state },
            })
            .collect()
    }

    pub fn active_term(&self, idx: usize) -> usize {
        self.groups[idx].active
    }

    pub fn select_term(&mut self, t: usize) {
        let g = &mut self.groups[self.active];
        if t < g.terms.len() {
            g.active = t;
        }
    }

    pub fn cycle_term(&mut self, delta: i32) {
        let g = &mut self.groups[self.active];
        let n = g.terms.len() as i32;
        g.active = (g.active as i32 + delta).rem_euclid(n) as usize;
    }

    /// A plain shell in a new terminal tab, in the navigation shell's folder.
    pub fn new_term(&mut self) {
        let cwd = self.term_dir(self.active, 0);
        let label = format!("shell {}", self.next_shell);
        self.next_shell += 1;
        let g = &mut self.groups[self.active];
        g.terms.push(Term::new(Kind::Extra { label }, cwd, None));
        g.active = g.terms.len() - 1;
    }

    /// True when terminal `t` of the active tab runs a command (closing it would stop it).
    pub fn is_running(&self, t: usize) -> bool {
        self.groups[self.active].terms.get(t).is_some_and(Term::running)
    }

    /// Label of terminal `t` of the active tab.
    pub fn label(&self, t: usize) -> String {
        self.term_tabs(self.active).get(t).map(|i| i.label.clone()).unwrap_or_default()
    }

    /// Closes terminal `t` of the active tab (never the navigation shell).
    pub fn close_term(&mut self, t: usize) {
        let g = &mut self.groups[self.active];
        if t == 0 || t >= g.terms.len() {
            return;
        }
        g.terms.remove(t);
        if g.active >= t {
            g.active = g.active.saturating_sub(1).min(g.terms.len() - 1);
        }
    }

    /// Remembers terminal `t` for [`CLOSE_CONFIRMED`].
    pub fn ask_close(&mut self, t: usize) {
        let id = self.groups[self.active].terms.get(t).and_then(|x| x.session.as_ref()).map(TerminalSession::id);
        self.pending_close = id.map(|id| (self.active, id));
    }

    /// Closes the finished command tabs of the active workspace; returns how many.
    pub fn close_finished(&mut self) -> usize {
        let g = &mut self.groups[self.active];
        let current = g.active;
        let before = g.terms.len();
        let mut kept = Vec::with_capacity(before);
        let mut active = 0;
        for (i, t) in std::mem::take(&mut g.terms).into_iter().enumerate() {
            let finished = !t.is_nav() && !t.running() && matches!(t.state, TermState::Done(_));
            if !finished {
                if i <= current {
                    active = kept.len();
                }
                kept.push(t);
            }
        }
        g.terms = kept;
        g.active = active;
        before - g.terms.len()
    }

    pub fn finished(&self) -> usize {
        self.groups[self.active].terms.iter().filter(|t| !t.running() && matches!(t.state, TermState::Done(_))).count()
    }

    // ---- Following Files, running commands, promotion ----

    /// Makes the navigation shell of tab `idx` follow the Files pane into `dir`: right away
    /// when it is at its prompt, otherwise once its command finishes (or is promoted).
    pub fn follow(&mut self, idx: usize, dir: &Path) {
        // e.g. the in-memory demo file system.
        if !dir.is_dir() {
            return;
        }
        let Some(g) = self.groups.get_mut(idx) else { return };
        let nav = g.nav();
        nav.pending = None;
        match &mut nav.session {
            Some(session) if !session.is_idle() => nav.pending = Some(dir.to_path_buf()),
            Some(session) => {
                send_cd(session, dir);
                nav.cwd = dir.to_path_buf();
                nav.scroll = 0;
            }
            // Starts (or restarts) there.
            None => nav.cwd = dir.to_path_buf(),
        }
    }

    /// Types `command` into the navigation shell of tab `idx` and shows it; if a command is
    /// already running there, runs it in a new terminal tab instead. Returns true in that case.
    pub fn run_in(&mut self, idx: usize, command: &str) -> bool {
        let Some(g) = self.groups.get_mut(idx) else { return false };
        let nav = g.nav();
        if nav.needs_start {
            nav.typed = Some(command.to_string());
            g.active = 0;
            return false;
        }
        if let Some(session) = nav.session.as_mut().filter(|s| s.is_idle()) {
            session.type_line(command);
            nav.scroll = 0;
            g.active = 0;
            return false;
        }
        let cwd = nav.cwd.clone();
        g.terms.push(Term::new(Kind::Extra { label: command.to_string() }, cwd, Some(command.to_string())));
        g.active = g.terms.len() - 1;
        true
    }

    /// Refreshes which terminals run a command, promotes long commands out of the navigation
    /// shells and sends pending `cd`s. Returns whether anything visible changed, and the
    /// commands that were promoted.
    pub fn poll(&mut self) -> (bool, Vec<Promoted>) {
        let mut changed = false;
        let mut promoted = Vec::new();
        for (gi, g) in self.groups.iter_mut().enumerate() {
            for t in &mut g.terms {
                changed |= refresh_job(t);
            }
            let nav = &g.terms[0];
            let promote = match (&nav.job, &nav.session) {
                (Some((_, since)), Some(s)) => since.elapsed() >= PROMOTE_AFTER || s.alternate_screen(),
                _ => false,
            };
            if promote {
                let mut old = g.terms.remove(0);
                let label = old.job.and_then(|(pgid, _)| session::process_cmdline(pgid)).unwrap_or_else(|| "command".into());
                // The new navigation shell starts where Files is now.
                let dir = old.pending.take().unwrap_or_else(|| old.cwd.clone());
                old.kind = Kind::Extra { label: label.clone() };
                g.terms.insert(0, old);
                g.terms.insert(0, Term::new(Kind::Nav, dir, None));
                // Every index moved by one; the focus stays on the command.
                g.active += 1;
                promoted.push(Promoted { tab: gi, label });
                changed = true;
                continue;
            }
            let nav = g.nav();
            if !nav.running()
                && let (Some(dir), Some(session)) = (nav.pending.take(), &mut nav.session)
            {
                send_cd(session, &dir);
                nav.cwd = dir;
                nav.scroll = 0;
                changed = true;
            }
        }
        (changed, promoted)
    }

    /// The folder of terminal `t` of tab `idx`: the shell's own where cheap to read, else the
    /// last folder it was sent to.
    fn term_dir(&self, idx: usize, t: usize) -> PathBuf {
        let term = &self.groups[idx].terms[t];
        term.session.as_ref().and_then(TerminalSession::shell_cwd).unwrap_or_else(|| term.cwd.clone())
    }

    /// The navigation shell's folder.
    pub fn nav_dir(&self, idx: usize) -> PathBuf {
        self.term_dir(idx, 0)
    }

    fn cur(&mut self) -> &mut Term {
        let g = &mut self.groups[self.active];
        &mut g.terms[g.active]
    }

    fn cur_ref(&self) -> &Term {
        let g = &self.groups[self.active];
        &g.terms[g.active]
    }

    fn start_shell(&mut self) {
        let (rows, cols) = (self.area.height, self.area.width);
        let session_id = self.next_session;
        self.next_session += 1;
        let tx = self.tx.clone();
        let t = self.cur();
        t.scroll = 0;
        t.needs_start = false;
        t.pending = None;
        t.job = None;
        t.state = TermState::Idle;
        match TerminalSession::spawn(session_id, &t.cwd, rows, cols, t.typed.take(), tx) {
            Ok(s) => {
                t.session = Some(s);
                t.notice = None;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to start shell");
                t.session = None;
                t.notice = Some(format!("{e:#}. Press Enter to try again."));
            }
        }
    }

    /// The active shell's current folder, falling back to the folder it started in.
    fn current_dir(&self) -> PathBuf {
        let t = self.cur_ref();
        t.session.as_ref().and_then(TerminalSession::cwd).unwrap_or_else(|| t.cwd.clone())
    }

    fn term_by_session(&mut self, id: u64) -> Option<(usize, usize)> {
        self.groups.iter().enumerate().find_map(|(gi, g)| {
            g.terms.iter().position(|t| t.session.as_ref().is_some_and(|s| s.id() == id)).map(|ti| (gi, ti))
        })
    }
}

/// Updates the foreground job of `t`; a command tab whose job ended shows ✓ / ✗.
fn refresh_job(t: &mut Term) -> bool {
    let Some(session) = &t.session else { return false };
    let job = session.foreground_job();
    let before = t.job.map(|(pgid, _)| pgid);
    let mut changed = false;
    if job != before {
        changed = true;
        match job {
            Some(pgid) => {
                t.job = Some((pgid, Instant::now()));
                t.exits_at_start = session.exits().0;
                t.state = TermState::Running;
                // Extra terminals are named after what runs in them.
                if let Kind::Extra { label } = &mut t.kind
                    && let Some(cmd) = session::process_cmdline(pgid)
                {
                    *label = cmd;
                }
            }
            None => {
                t.job = None;
                t.state = if t.is_nav() { TermState::Idle } else { TermState::Done(None) };
            }
        }
    }
    // The exit code arrives with the next prompt, a moment after the job ends.
    if let TermState::Done(None) = t.state {
        let (count, code) = session.exits();
        if count > t.exits_at_start {
            t.state = TermState::Done(Some(code));
            changed = true;
        }
    }
    changed
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
        if self.cur().needs_start {
            self.start_shell();
        }
        let theme = &shared.theme;
        let focused = self.focused;
        let tab = self.cur();
        let mut notice_area = area;
        if let Some(session) = &mut tab.session {
            session.resize(area.height, area.width);
            let mut parser = session.screen();
            parser.screen_mut().set_scrollback(tab.scroll);
            tab.scroll = parser.screen().scrollback();
            let screen = parser.screen();
            let mut cursor = Cursor::default().style(Style::new().bg(theme.accent).fg(theme.bg));
            if screen.hide_cursor() || tab.scroll > 0 || !focused {
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
        let tab = self.cur();
        if tab.notice.is_some() {
            if key.code == KeyCode::Enter {
                self.start_shell();
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
        let tab = self.cur();
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
        let tab = self.cur();
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
            action::CLOSE_CONFIRMED => {
                if let Some((gi, id)) = self.pending_close.take()
                    && let Some((g, t)) = self.term_by_session(id)
                    && g == gi
                {
                    let keep = self.active;
                    self.active = g;
                    self.close_term(t);
                    self.active = keep;
                }
            }
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
                if let Some((g, t)) = self.term_by_session(*id) {
                    let term = &mut self.groups[g].terms[t];
                    term.session = None;
                    term.job = None;
                    if !term.is_nav() && !matches!(term.state, TermState::Done(_)) {
                        term.state = TermState::Done(None);
                    }
                    // A new shell starts in the folder Files moved to meanwhile.
                    if let Some(dir) = term.pending.take() {
                        term.cwd = dir;
                    }
                    term.notice = Some("Shell exited. Press Enter to start a new one.".into());
                }
            }
        }
        EventResult::Consumed
    }

    fn status(&self, shared: &Shared) -> String {
        let host = shared.host.label();
        // The demo's shell runs in a real folder of this machine: keep it off screen.
        if shared.demo {
            return host;
        }
        let g = &self.groups[self.active];
        let t = &g.terms[g.active];
        let dir = crate::ui::bars::tilde(&self.term_dir(self.active, g.active));
        match (&t.kind, t.running(), t.state) {
            (Kind::Nav, false, _) => format!("{host} · navigation shell in {dir} · follows Files"),
            (Kind::Nav, true, _) => format!("{host} · navigation shell busy · gets its own terminal after 1 s"),
            (Kind::Extra { label }, true, _) => format!("{host} · {label} · running in {dir}"),
            (Kind::Extra { label }, false, TermState::Done(Some(0))) => format!("{host} · {label} · finished"),
            (Kind::Extra { label }, false, TermState::Done(Some(code))) => format!("{host} · {label} · exited with {code}"),
            (Kind::Extra { label }, false, _) => format!("{host} · {label} · in {dir}"),
        }
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

    fn done(label: &str, code: Option<i32>) -> Term {
        let mut t = Term::new(Kind::Extra { label: label.into() }, PathBuf::from("/"), None);
        t.state = TermState::Done(code);
        t
    }

    #[test]
    fn closing_finished_terminals_keeps_the_right_one_active() {
        let (tx, _rx) = crate::event::channel();
        let mut m = TerminalMode::new(PathBuf::from("/"), tx);
        let g = &mut m.groups[0];
        g.terms.push(done("npm test", Some(0)));
        g.terms.push(Term::new(Kind::Extra { label: "shell 1".into() }, PathBuf::from("/"), None));
        g.terms.push(done("make", Some(2)));
        g.active = 2; // shell 1
        assert_eq!(m.finished(), 2);
        assert_eq!(m.close_finished(), 2);
        let tabs = m.term_tabs(0);
        assert_eq!(tabs.iter().map(|t| t.label.as_str()).collect::<Vec<_>>()[1..], ["shell 1"]);
        assert_eq!(m.active_term(0), 1);
        assert!(tabs[0].nav);
    }

    #[test]
    fn the_navigation_shell_is_never_closed() {
        let (tx, _rx) = crate::event::channel();
        let mut m = TerminalMode::new(PathBuf::from("/"), tx);
        m.new_term();
        assert_eq!(m.active_term(0), 1);
        m.close_term(0);
        assert_eq!(m.term_tabs(0).len(), 2);
        m.close_term(1);
        assert_eq!((m.term_tabs(0).len(), m.active_term(0)), (1, 0));
    }
}
