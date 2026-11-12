//! Event loop, layout and routing between the app chrome, overlays and the two panes.
//!
//! Every tab is a workspace: a Files pane on top and a terminal below it that follows the
//! Files folder. Both panes keep one entry per tab, aligned by index; this file keeps them
//! in step (new, close, select) and owns what belongs to the tab as a whole: the terminal
//! size, which pane has the focus, and the folder the shell was last sent to.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    event::AppEvent,
    keys::KeyChord,
    modes::{
        Effect, EventResult, MenuItem, ModeCtx, Pane, Shared,
        files::FilesMode,
        requests::{OpenTab, ShowInFiles},
        terminal::{self, TerminalMode},
    },
    ui::{
        AppLayout, TermSize,
        bars::{self, ChromeTarget, TabInfo, TermHeader},
        dialog::{Confirm, DialogHit, DialogState},
        hit::HitMap,
        menu::{MenuOwner, MenuState},
        run_dialog::{RunDialogState, RunHit, RunOutcome},
        terminal_guard::Tui,
        toast::Toast,
    },
};

/// Actions of the app's own menu (the shortcuts menu).
mod app_action {
    pub const NEW_TAB: u32 = 1;
    pub const CLOSE_TAB: u32 = 2;
    pub const NEXT_TAB: u32 = 3;
    pub const PREV_TAB: u32 = 4;
    pub const QUIT: u32 = 5;
    pub const SWAP_FOCUS: u32 = 6;
    pub const CYCLE_SIZE: u32 = 7;
    pub const NEW_TERM: u32 = 8;
    pub const NEXT_TERM: u32 = 9;
    pub const PREV_TERM: u32 = 10;
    pub const NAV_TERM: u32 = 11;
    pub const CLOSE_FINISHED: u32 = 12;
    /// `PANE_ACTION_BASE * (pane + 1) + a` runs action `a` of that pane (its shortcut items).
    pub const PANE_ACTION_BASE: u32 = 1_000_000;
}

/// Which pane gets the keys. Doubles as the index used by menus and dialogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Files = 0,
    Term = 1,
}

impl Focus {
    fn from_index(i: usize) -> Self {
        if i == 1 { Focus::Term } else { Focus::Files }
    }
}

/// What a tab owns besides its two panes.
struct Workspace {
    size: TermSize,
    focus: Focus,
    /// The Files folder the shell was last told to follow.
    synced: PathBuf,
}

impl Workspace {
    fn new(dir: PathBuf, focus: Focus) -> Self {
        Self { size: TermSize::Normal, focus, synced: dir }
    }

    /// Full screen leaves only the terminal; minimized leaves only Files.
    fn focus(&self) -> Focus {
        match self.size {
            TermSize::Full => Focus::Term,
            TermSize::Min => Focus::Files,
            TermSize::Normal => self.focus,
        }
    }

    fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
        match (focus, self.size) {
            (Focus::Term, TermSize::Min) => self.size = TermSize::Normal,
            (Focus::Files, TermSize::Full) => self.size = TermSize::Normal,
            _ => {}
        }
    }

    fn set_size(&mut self, size: TermSize) {
        self.size = size;
        // Opening the terminal means you want to type in it; minimizing hands keys back.
        self.focus = if size == TermSize::Min { Focus::Files } else { Focus::Term };
    }
}

pub struct App {
    shared: Shared,
    files: FilesMode,
    term: TerminalMode,
    workspaces: Vec<Workspace>,
    leader: KeyChord,
    leader_pending: bool,
    menu: Option<MenuState>,
    dialog: Option<DialogState>,
    run_dialog: Option<RunDialogState>,
    run_history: crate::run::History,
    toast: Option<Toast>,
    chrome_hits: HitMap<ChromeTarget>,
    layout: AppLayout,
    /// The pane a mouse button went down in gets the drag and the release.
    mouse_owner: Option<Focus>,
    quit: bool,
}

impl App {
    pub fn new(shared: Shared, files: FilesMode, term: TerminalMode) -> Self {
        let leader = KeyChord::parse(&shared.config.leader).unwrap_or_else(|| {
            tracing::warn!(leader = %shared.config.leader, "invalid leader key, using ctrl-]");
            KeyChord::parse("ctrl-]").expect("valid default")
        });
        // The demo keeps its Run history in memory only.
        let run_history =
            if shared.state.is_persistent() { crate::run::History::load_default() } else { crate::run::History::default() };
        let workspaces = vec![Workspace::new(files.path(0).to_path_buf(), Focus::Files)];
        Self {
            shared,
            files,
            term,
            workspaces,
            leader,
            leader_pending: false,
            menu: None,
            dialog: None,
            run_dialog: None,
            run_history,
            toast: None,
            chrome_hits: HitMap::default(),
            layout: AppLayout::default(),
            mouse_owner: None,
            quit: false,
        }
    }

    pub async fn run(mut self, terminal: &mut Tui, mut rx: UnboundedReceiver<AppEvent>) -> Result<()> {
        let mut input = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut dirty = true;
        while !self.quit {
            if dirty {
                self.sync_folders();
                terminal.draw(|f| self.render(f))?;
                dirty = false;
            }
            tokio::select! {
                ev = input.next() => match ev {
                    Some(Ok(ev)) => { self.handle_input(ev); dirty = true; }
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                Some(ev) = rx.recv() => {
                    self.dispatch_app_event(ev);
                    // Coalesce bursts (e.g. PTY output) into a single redraw.
                    while let Ok(ev) = rx.try_recv() {
                        self.dispatch_app_event(ev);
                    }
                    dirty = true;
                }
                _ = tick.tick() => {
                    if self.toast.as_ref().is_some_and(Toast::expired) {
                        self.toast = None;
                        dirty = true;
                    }
                    // Commands starting and ending, promotions and pending `cd`s.
                    let (changed, promoted) = self.term.poll();
                    dirty |= changed;
                    let active = self.active();
                    if let Some(p) = promoted.into_iter().find(|p| p.tab == active) {
                        self.toast = Some(Toast::new(
                            format!("“{}” is still running, so it keeps its own terminal", bars::truncate(&p.label, 48)),
                            "A new navigation shell is ready in the first terminal tab.".into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn active(&self) -> usize {
        self.files.active_tab()
    }

    fn ws(&mut self) -> &mut Workspace {
        let i = self.active();
        &mut self.workspaces[i]
    }

    /// The terminal follows the Files pane: any tab whose folder changed (navigation, a
    /// moved or deleted folder) sends its shell a `cd`.
    fn sync_folders(&mut self) {
        for (i, ws) in self.workspaces.iter_mut().enumerate() {
            let path = self.files.path(i);
            if ws.synced != path {
                ws.synced = path.to_path_buf();
                self.term.follow(i, path);
            }
        }
    }

    // ---- Rendering ----

    fn render(&mut self, f: &mut Frame) {
        let theme = &self.shared.theme;
        f.render_widget(Paragraph::new("").style(Style::new().bg(theme.bg).fg(theme.fg)), f.area());
        let i = self.active();
        let (size, focus) = (self.workspaces[i].size, self.workspaces[i].focus());
        self.layout = AppLayout::new(f.area(), self.files.toolbar_height(), size);
        self.chrome_hits.clear();
        self.files.focused = focus == Focus::Files;
        self.term.focused = focus == Focus::Term;

        let tabs: Vec<TabInfo> = (0..self.files.tab_count())
            .map(|t| TabInfo { title: self.files.title(t), running: self.term.running(t) })
            .collect();
        bars::render_tab_bar(f, self.layout.tab_bar, theme, "▤", &tabs, i, &mut self.chrome_hits);

        if self.layout.files_pane().height > 0 {
            if self.layout.toolbar.height > 0 {
                self.files.render_toolbar(f, self.layout.toolbar, &self.shared);
            }
            self.files.render(f, self.layout.files, &self.shared);
        }
        let mut term_tabs = self.term.term_tabs(i);
        // The demo's shell runs in a real folder of this machine: show the sample one instead.
        if self.shared.demo {
            term_tabs[0].label = bars::tilde(self.files.path(i));
        }
        let header =
            TermHeader { tabs: &term_tabs, active: self.term.active_term(i), focused: focus == Focus::Term, size };
        bars::render_term_header(f, self.layout.term_header, theme, &header, &mut self.chrome_hits);
        if self.layout.term.height > 0 {
            self.term.render(f, self.layout.term, &self.shared);
        }

        let mut status = match focus {
            Focus::Files => self.files.status(&self.shared),
            Focus::Term => self.term.status(&self.shared),
        };
        if self.leader_pending {
            status = format!(
                "{}: t new tab · c new terminal · j/k terminals · ` focus · z size · ? menu · q quit",
                crate::keys::display(&self.shared.config.leader)
            );
        }
        let leader = crate::keys::display(&self.shared.config.leader);
        bars::render_bottom_bar(f, self.layout.bottom_bar, theme, &status, &leader, &mut self.chrome_hits);

        if let Some(toast) = &self.toast {
            // Above the terminal header when Files is visible, else at the bottom of the terminal.
            let mut area = if self.layout.files.height > 0 { self.layout.files } else { self.layout.term };
            area.height = area.height.saturating_sub(1);
            toast.render(f, area, theme);
        }
        if let Some(menu) = &mut self.menu {
            menu.render(f, theme);
        }
        if let Some(dialog) = &mut self.dialog {
            dialog.render(f, theme);
        }
        if let Some(run) = &mut self.run_dialog {
            run.render(f, theme);
        }
    }

    // ---- Effects ----

    /// Runs `f` against a pane with a fresh context and applies the effects it produced.
    fn with_pane<R>(&mut self, pane: Focus, f: impl FnOnce(&mut dyn Pane, &mut ModeCtx) -> R) -> R {
        let mut effects = Vec::new();
        let result = {
            let mut ctx = ModeCtx::new(&self.shared, &mut effects);
            let p: &mut dyn Pane = match pane {
                Focus::Files => &mut self.files,
                Focus::Term => &mut self.term,
            };
            f(p, &mut ctx)
        };
        for effect in effects {
            match effect {
                Effect::Request(req) => self.handle_request(req.as_ref()),
                Effect::Confirm(c) => {
                    self.menu = None;
                    self.dialog = Some(DialogState::new(pane as usize, c));
                }
                Effect::Toast { msg, detail } => self.toast = Some(Toast::new(msg, detail)),
                Effect::Run(prompt) => {
                    self.menu = None;
                    let history = self.run_history.for_key(&prompt.key);
                    self.run_dialog = Some(RunDialogState::new(prompt, history));
                }
                Effect::OpenMenu { x, y, items } => {
                    self.menu = (!items.is_empty()).then(|| MenuState::new(MenuOwner::Mode(pane as usize), (x, y), items));
                }
            }
        }
        result
    }

    fn handle_request(&mut self, req: &dyn std::any::Any) {
        if let Some(r) = req.downcast_ref::<OpenTab>() {
            self.open_tab(r.cwd.clone(), None);
        } else if let Some(r) = req.downcast_ref::<ShowInFiles>() {
            // The shell is already there: no `cd` back.
            self.files.show(r.path.clone());
            let ws = self.ws();
            ws.synced = r.path.clone();
            ws.set_focus(Focus::Files);
        } else {
            tracing::warn!("unknown request");
        }
    }

    fn dispatch_app_event(&mut self, ev: AppEvent) {
        for pane in [Focus::Files, Focus::Term] {
            if self.with_pane(pane, |p, ctx| p.handle_app_event(&ev, ctx)) == EventResult::Consumed {
                break;
            }
        }
    }

    // ---- Input ----

    fn handle_input(&mut self, ev: Event) {
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) if self.run_dialog.is_some() => {
                if let Some(d) = &mut self.run_dialog {
                    d.type_text(&text);
                }
            }
            Event::Paste(text) if self.dialog.is_some() => {
                if let Some(d) = &mut self.dialog {
                    d.type_text(&text);
                }
            }
            Event::Paste(text) if self.menu.is_none() => {
                let focus = self.ws().focus();
                self.with_pane(focus, |p, ctx| p.handle_paste(&text, ctx));
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if let Some(run) = &mut self.run_dialog {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Esc => self.run_dialog = None,
                KeyCode::Tab | KeyCode::BackTab => run.next_field(),
                KeyCode::Up => run.browse_history(true),
                KeyCode::Down => run.browse_history(false),
                KeyCode::Enter => self.submit_run(),
                KeyCode::Backspace => run.backspace(),
                KeyCode::Char('w') if ctrl => run.delete_word(),
                KeyCode::Char('u') if ctrl => run.clear_field(),
                KeyCode::Char(c) if !ctrl => run.type_text(&c.to_string()),
                _ => {}
            }
            return;
        }
        if let Some(dialog) = &mut self.dialog {
            match key.code {
                KeyCode::Esc => self.dialog = None,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right if !dialog.has_input() => {
                    dialog.toggle_focus()
                }
                KeyCode::Left | KeyCode::Right | KeyCode::End | KeyCode::Home => dialog.deselect(),
                KeyCode::Enter => {
                    let confirm = if dialog.has_input() { dialog.can_confirm() } else { dialog.confirm_focused() };
                    if confirm {
                        self.accept_dialog();
                    } else if !dialog.has_input() {
                        self.dialog = None;
                    }
                }
                KeyCode::Backspace => dialog.backspace(),
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => dialog.clear_input(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => dialog.type_text(&c.to_string()),
                _ => {}
            }
            return;
        }
        if let Some(menu) = &mut self.menu {
            match key.code {
                KeyCode::Esc => self.menu = None,
                KeyCode::Up | KeyCode::BackTab => menu.move_selection(-1),
                KeyCode::Down | KeyCode::Tab => menu.move_selection(1),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(action) = menu.selected_action() {
                        self.run_menu_action(action);
                    }
                }
                _ => {}
            }
            return;
        }

        // Ctrl+Q quits from anywhere, even when a terminal has focus.
        if key.code == KeyCode::Char('q') && key.modifiers == KeyModifiers::CONTROL {
            self.quit = true;
            return;
        }

        let focus = self.ws().focus();
        if self.leader_pending {
            self.leader_pending = false;
            if self.leader.matches(&key) {
                // Pressing the leader twice sends it to the pane.
                self.with_pane(focus, |p, ctx| p.handle_key(key, ctx));
            } else {
                self.leader_command(key);
            }
            return;
        }
        if self.leader.matches(&key) {
            self.leader_pending = true;
            return;
        }

        if self.with_pane(focus, |p, ctx| p.handle_key(key, ctx)) == EventResult::Consumed {
            return;
        }
        let captures = match focus {
            Focus::Files => self.files.captures_input(),
            Focus::Term => self.term.captures_input(),
        };
        if !captures {
            self.global_shortcut(key);
        }
    }

    fn leader_command(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('t') => self.new_tab(),
            KeyCode::Char('c') => self.new_term(),
            KeyCode::Char('j') | KeyCode::Down => self.cycle_term(1),
            KeyCode::Char('k') | KeyCode::Up => self.cycle_term(-1),
            KeyCode::Char('0') => self.select_term(0),
            KeyCode::Char('w') | KeyCode::Char('x') => self.close_tab(self.active()),
            KeyCode::Char('n') | KeyCode::Right => self.cycle_tab(1),
            KeyCode::Char('p') | KeyCode::Left => self.cycle_tab(-1),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('`') | KeyCode::Tab => self.swap_focus(),
            KeyCode::Char('z') => self.cycle_size(),
            KeyCode::Char('?') | KeyCode::Char(' ') => self.open_shortcuts_menu_from_keyboard(),
            // The panes' own shortcut items, by the key shown as their hint (`f`, `.`, `r`).
            KeyCode::Char(c) => {
                let wanted = c.to_string();
                for pane in [Focus::Term, Focus::Files] {
                    let items = match pane {
                        Focus::Files => self.files.shortcut_items(),
                        Focus::Term => self.term.shortcut_items(),
                    };
                    let action = items.into_iter().find_map(|it| match it {
                        MenuItem::Action { hint, enabled: true, action, .. } if hint == wanted => Some(action),
                        _ => None,
                    });
                    if let Some(action) = action {
                        self.with_pane(pane, |p, ctx| p.on_action(action, ctx));
                        return;
                    }
                }
            }
            _ => {}
        }
    }

    /// Shortcuts while the Files pane has the focus (it does not capture every key).
    fn global_shortcut(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('t') if ctrl => self.new_tab(),
            KeyCode::Char('w') if ctrl => self.close_tab(self.active()),
            KeyCode::Tab => self.cycle_tab(1),
            KeyCode::BackTab => self.cycle_tab(-1),
            KeyCode::Char('`') => self.swap_focus(),
            KeyCode::Char('?') => self.open_shortcuts_menu_from_keyboard(),
            KeyCode::Char('q') if key.modifiers.is_empty() => self.quit = true,
            _ => {}
        }
    }

    fn handle_mouse(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        if let Some(run) = &mut self.run_dialog {
            // Clicking outside does not close it, so typed text is never lost by accident.
            if ev.kind == MouseEventKind::Down(MouseButton::Left) {
                match run.hit(x, y) {
                    Some(RunHit::Field(field)) => run.focus(field),
                    Some(RunHit::Suggestion(i)) => run.use_suggestion(i),
                    Some(RunHit::Cancel) => self.run_dialog = None,
                    Some(RunHit::Run) => self.submit_run(),
                    _ => {}
                }
            }
            return;
        }
        if let Some(dialog) = &mut self.dialog {
            if ev.kind == MouseEventKind::Down(MouseButton::Left) {
                match dialog.hit(x, y) {
                    Some(DialogHit::Confirm) if dialog.can_confirm() => self.accept_dialog(),
                    Some(DialogHit::Cancel) | None => self.dialog = None,
                    _ => {}
                }
            } else if let MouseEventKind::Moved = ev.kind {
                match dialog.hit(x, y) {
                    Some(DialogHit::Confirm) => dialog.focus_confirm(true),
                    Some(DialogHit::Cancel) => dialog.focus_confirm(false),
                    _ => {}
                }
            }
            return;
        }
        if let Some(menu) = &mut self.menu {
            match ev.kind {
                MouseEventKind::Moved | MouseEventKind::Drag(_) => menu.hover(x, y),
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(action) = menu.hit(x, y).and_then(|i| menu.action_at(i)) {
                        self.run_menu_action(action);
                    } else if !menu.contains(x, y) {
                        self.menu = None;
                    }
                }
                // A right click elsewhere moves the menu there instead of only closing it,
                // so right-clicking file after file opens a menu every time.
                MouseEventKind::Down(MouseButton::Right) if !menu.contains(x, y) => self.menu = None,
                MouseEventKind::Down(_) if !menu.contains(x, y) => {
                    self.menu = None;
                    return;
                }
                _ => {}
            }
            if !(ev.kind == MouseEventKind::Down(MouseButton::Right) && self.menu.is_none()) {
                return;
            }
        }

        let inside = |r: Rect| r.contains((x, y).into());
        let l = self.layout;
        if matches!(ev.kind, MouseEventKind::Down(_)) && (inside(l.tab_bar) || inside(l.bottom_bar) || inside(l.term_header)) {
            if ev.kind == MouseEventKind::Down(MouseButton::Left) {
                self.chrome_click(x, y);
            }
            return;
        }

        // Drags and releases go to the pane the button went down in.
        let pane = match ev.kind {
            MouseEventKind::Drag(_) | MouseEventKind::Up(_) => self.mouse_owner,
            _ if inside(l.files_pane()) => Some(Focus::Files),
            _ if inside(l.term) => Some(Focus::Term),
            _ => None,
        };
        let Some(pane) = pane else { return };
        match ev.kind {
            MouseEventKind::Down(_) => {
                self.mouse_owner = Some(pane);
                // Clicking a pane gives it the keys (scrolling does not).
                self.ws().set_focus(pane);
            }
            MouseEventKind::Up(_) => self.mouse_owner = None,
            _ => {}
        }
        self.with_pane(pane, |p, ctx| p.handle_mouse(ev, ctx));
    }

    fn chrome_click(&mut self, x: u16, y: u16) {
        match self.chrome_hits.hit(x, y) {
            Some(ChromeTarget::Tab(i)) => self.select_tab(i),
            Some(ChromeTarget::CloseTab(i)) => self.close_tab(i),
            Some(ChromeTarget::NewTab) => self.new_tab(),
            Some(ChromeTarget::TermTitle) => {
                let ws = self.ws();
                let size = if ws.size == TermSize::Min { TermSize::Normal } else { TermSize::Min };
                ws.set_size(size);
            }
            Some(ChromeTarget::TermSize(size)) => self.ws().set_size(size),
            Some(ChromeTarget::Term(t)) => self.select_term(t),
            Some(ChromeTarget::CloseTerm(t)) => self.close_term(t),
            Some(ChromeTarget::NewTerm) => self.new_term(),
            Some(ChromeTarget::Quit) => self.quit = true,
            Some(ChromeTarget::Shortcuts) => self.open_shortcuts_menu(x, y),
            None => {}
        }
    }

    fn run_menu_action(&mut self, action: u32) {
        if let Some(menu) = self.menu.take() {
            match menu.owner {
                MenuOwner::Mode(i) => {
                    self.with_pane(Focus::from_index(i), |p, ctx| p.on_action(action, ctx));
                }
                MenuOwner::App => self.run_app_action(action),
            }
        }
    }

    /// The tab, focus and size commands plus both panes' own items, each with its key.
    fn open_shortcuts_menu(&mut self, x: u16, _y: u16) {
        let closable = self.files.tab_count() > 1;
        let focus = self.ws().focus();
        // In the terminal every key goes to the shell, so its keys come after the leader;
        // Files also has direct shortcuts (see `global_shortcut`), which are shorter to show.
        let leader = crate::keys::display(&self.shared.config.leader);
        let in_term = focus == Focus::Term;
        let key = |after_leader: &str, direct: &str| if in_term { after_leader.to_string() } else { direct.to_string() };
        let leader_only = |k: &str| if in_term { k.to_string() } else { format!("{leader} {k}") };
        let swap = if in_term { "Focus Files" } else { "Focus terminal" };
        let size = match self.ws().size {
            TermSize::Min => "minimized",
            TermSize::Normal => "half",
            TermSize::Full => "full",
        };
        let mut items = vec![
            MenuItem::action("New tab", app_action::NEW_TAB).with_hint(key("t", "Ctrl-t")),
            MenuItem::action("Close tab", app_action::CLOSE_TAB).with_hint(key("w", "Ctrl-w")).enabled(closable),
            MenuItem::action("Next tab", app_action::NEXT_TAB).with_hint(key("n", "Tab")),
            MenuItem::action("Previous tab", app_action::PREV_TAB).with_hint(key("p", "Shift-Tab")),
            MenuItem::Separator,
            MenuItem::action(swap, app_action::SWAP_FOCUS).with_hint("`"),
            MenuItem::action(format!("Terminal size: {size}"), app_action::CYCLE_SIZE).with_hint(leader_only("z")),
            MenuItem::Separator,
            MenuItem::action("New terminal", app_action::NEW_TERM).with_hint(leader_only("c")),
            MenuItem::action("Next terminal", app_action::NEXT_TERM).with_hint(leader_only("j")),
            MenuItem::action("Previous terminal", app_action::PREV_TERM).with_hint(leader_only("k")),
            MenuItem::action("Navigation terminal", app_action::NAV_TERM).with_hint(leader_only("0")),
            MenuItem::action("Close finished terminals", app_action::CLOSE_FINISHED)
                .with_hint(match self.term.finished() {
                    0 => String::new(),
                    n => n.to_string(),
                })
                .enabled(self.term.finished() > 0),
            MenuItem::Separator,
        ];
        let base = app_action::PANE_ACTION_BASE;
        // Files keys work directly in Files and after the leader in the terminal.
        let files_items = self.files.shortcut_items();
        let term_items: Vec<MenuItem> = self
            .term
            .shortcut_items()
            .into_iter()
            .map(|it| match it {
                MenuItem::Action { label, hint, enabled, danger, action } => {
                    MenuItem::Action { label, hint: leader_only(&hint), enabled, danger, action }
                }
                sep => sep,
            })
            .collect();
        for (pane, own) in [(Focus::Files, files_items), (Focus::Term, term_items)] {
            let offset = base * (pane as u32 + 1);
            items.extend(own.into_iter().map(|it| match it {
                MenuItem::Action { label, hint, enabled, danger, action } => {
                    MenuItem::Action { label, hint, enabled, danger, action: offset + action }
                }
                sep => sep,
            }));
        }
        items.push(MenuItem::Separator);
        items.push(MenuItem::action("Quit", app_action::QUIT).with_hint(key("q", "q")));
        let top_of_bar = self.layout.bottom_bar.y;
        self.menu = Some(MenuState::new(MenuOwner::App, (x.saturating_sub(4), top_of_bar), items).above());
    }

    /// Opens the shortcuts menu at the right of the bottom bar, where its button is.
    fn open_shortcuts_menu_from_keyboard(&mut self) {
        let x = self.layout.bottom_bar.right().saturating_sub(40);
        self.open_shortcuts_menu(x + 4, self.layout.bottom_bar.y);
    }

    fn run_app_action(&mut self, action: u32) {
        let base = app_action::PANE_ACTION_BASE;
        match action {
            app_action::NEW_TAB => self.new_tab(),
            app_action::CLOSE_TAB => self.close_tab(self.active()),
            app_action::NEXT_TAB => self.cycle_tab(1),
            app_action::PREV_TAB => self.cycle_tab(-1),
            app_action::QUIT => self.quit = true,
            app_action::SWAP_FOCUS => self.swap_focus(),
            app_action::CYCLE_SIZE => self.cycle_size(),
            app_action::NEW_TERM => self.new_term(),
            app_action::NEXT_TERM => self.cycle_term(1),
            app_action::PREV_TERM => self.cycle_term(-1),
            app_action::NAV_TERM => self.select_term(0),
            app_action::CLOSE_FINISHED => {
                let n = self.term.close_finished();
                let s = if n == 1 { "" } else { "s" };
                self.toast = Some(Toast::new(format!("Closed {n} finished terminal{s}"), String::new()));
            }
            a if a >= 2 * base => self.with_pane(Focus::Term, |p, ctx| p.on_action(a - 2 * base, ctx)),
            a if a >= base => self.with_pane(Focus::Files, |p, ctx| p.on_action(a - base, ctx)),
            _ => {}
        }
    }

    /// Enter / Run in the Run dialog: types the command into this tab's navigation shell (a
    /// long command is then promoted like any other), or into a new terminal tab when the
    /// navigation shell is still busy.
    fn submit_run(&mut self) {
        let Some(run) = &mut self.run_dialog else { return };
        match run.submit() {
            RunOutcome::Stay => {}
            RunOutcome::Run { command, prefix, args } => {
                let Some(run) = self.run_dialog.take() else { return };
                let p = run.prompt;
                self.run_history.record(&p.key, &prefix, &args);
                let i = self.active();
                let in_new_terminal = self.term.run_in(i, &command);
                self.ws().set_focus(Focus::Term);
                if in_new_terminal {
                    self.toast = Some(Toast::new("The navigation shell is busy, so this runs in a new terminal".into(), command));
                }
            }
        }
    }

    fn accept_dialog(&mut self) {
        if let Some(d) = self.dialog.take() {
            let action = d.confirm.action;
            let pane = Focus::from_index(d.mode);
            match d.text().map(str::to_string) {
                Some(text) => self.with_pane(pane, |p, ctx| p.on_text(action, &text, ctx)),
                None => self.with_pane(pane, |p, ctx| p.on_action(action, ctx)),
            }
        }
    }

    // ---- Tabs, focus and terminal size ----

    /// A new tab (Files + terminal) in `cwd`, at the end; `typed` runs once its shell is ready.
    fn open_tab(&mut self, cwd: PathBuf, typed: Option<String>) {
        let focus = if typed.is_some() { Focus::Term } else { Focus::Files };
        // The shell starts in `cwd` when it exists on disk (not in the demo file system).
        let shell_dir = if cwd.is_dir() { cwd.clone() } else { self.term.nav_dir(self.active()) };
        self.files.open_tab(cwd.clone());
        self.term.open_tab(shell_dir, typed);
        self.workspaces.push(Workspace::new(cwd, focus));
        self.menu = None;
    }

    /// A new tab in the current tab's folder.
    fn new_tab(&mut self) {
        let cwd = self.files.path(self.active()).to_path_buf();
        self.open_tab(cwd, None);
    }

    fn close_tab(&mut self, idx: usize) {
        if self.files.tab_count() > 1 && idx < self.workspaces.len() {
            self.files.close_tab(idx);
            self.term.close_tab(idx);
            self.workspaces.remove(idx);
            self.menu = None;
        }
    }

    fn select_tab(&mut self, idx: usize) {
        self.files.select_tab(idx);
        self.term.select_tab(idx);
        self.menu = None;
    }

    fn cycle_tab(&mut self, delta: i32) {
        let n = self.files.tab_count() as i32;
        if n > 0 {
            let next = (self.active() as i32 + delta).rem_euclid(n);
            self.select_tab(next as usize);
        }
    }

    fn new_term(&mut self) {
        self.term.new_term();
        self.ws().set_focus(Focus::Term);
    }

    fn select_term(&mut self, t: usize) {
        self.term.select_term(t);
        self.ws().set_focus(Focus::Term);
    }

    fn cycle_term(&mut self, delta: i32) {
        self.term.cycle_term(delta);
        self.ws().set_focus(Focus::Term);
    }

    /// Closes a terminal tab; one still running a command asks first.
    fn close_term(&mut self, t: usize) {
        if !self.term.is_running(t) {
            self.term.close_term(t);
            return;
        }
        let label = self.term.label(t);
        self.term.ask_close(t);
        self.menu = None;
        self.dialog = Some(DialogState::new(
            Focus::Term as usize,
            Confirm {
                title: format!("Stop {}?", bars::truncate(&label, 40)),
                message: "It is still running. Closing its terminal stops it.".into(),
                items: Vec::new(),
                confirm_label: "Stop and close".into(),
                danger: true,
                require_text: None,
                text: None,
                action: terminal::CLOSE_CONFIRMED,
            },
        ));
    }

    fn swap_focus(&mut self) {
        let ws = self.ws();
        let next = if ws.focus() == Focus::Files { Focus::Term } else { Focus::Files };
        ws.set_focus(next);
    }

    fn cycle_size(&mut self) {
        let ws = self.ws();
        let next = ws.size.next();
        ws.set_size(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_follows_the_terminal_size() {
        let mut ws = Workspace::new(PathBuf::from("/"), Focus::Files);
        ws.set_size(TermSize::Full);
        assert_eq!(ws.focus(), Focus::Term);
        // Focusing Files from full screen brings the Files pane back.
        ws.set_focus(Focus::Files);
        assert_eq!((ws.size, ws.focus()), (TermSize::Normal, Focus::Files));
        ws.set_size(TermSize::Min);
        assert_eq!(ws.focus(), Focus::Files);
        // Focusing a minimized terminal opens it.
        ws.set_focus(Focus::Term);
        assert_eq!((ws.size, ws.focus()), (TermSize::Normal, Focus::Term));
    }

    #[test]
    fn sizes_cycle() {
        assert_eq!(TermSize::Min.next(), TermSize::Normal);
        assert_eq!(TermSize::Normal.next(), TermSize::Full);
        assert_eq!(TermSize::Full.next(), TermSize::Min);
    }
}
