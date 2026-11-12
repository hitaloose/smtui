//! Event loop, layout and routing between the app chrome, overlays and the active mode.

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use ratatui::{Frame, style::Style, widgets::Paragraph};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    event::AppEvent,
    keys::KeyChord,
    modes::{Effect, EventResult, Mode, ModeCtx, Shared},
    ui::{
        AppLayout,
        bars::{self, ChromeTarget},
        hit::HitMap,
        dialog::{DialogHit, DialogState},
        menu::{MenuOwner, MenuState},
        run_dialog::{RunDialogState, RunHit, RunOutcome},
        terminal_guard::Tui,
        toast::Toast,
    },
};

/// Actions of the app's own menus (the shortcuts menu).
mod app_action {
    pub const NEW_TAB: u32 = 1;
    pub const CLOSE_TAB: u32 = 2;
    pub const NEXT_TAB: u32 = 3;
    pub const PREV_TAB: u32 = 4;
    pub const QUIT: u32 = 5;
    /// `MODE_BASE + i` switches to mode `i`.
    pub const MODE_BASE: u32 = 100;
    /// `MODE_ACTION_BASE + a` runs action `a` of the active mode (its shortcut items).
    pub const MODE_ACTION_BASE: u32 = 1_000_000;
}

pub struct App {
    shared: Shared,
    modes: Vec<Box<dyn Mode>>,
    active_mode: usize,
    leader: KeyChord,
    leader_pending: bool,
    menu: Option<MenuState>,
    dialog: Option<DialogState>,
    run_dialog: Option<RunDialogState>,
    run_history: crate::run::History,
    toast: Option<Toast>,
    chrome_hits: HitMap<ChromeTarget>,
    layout: AppLayout,
    quit: bool,
}

impl App {
    pub fn new(shared: Shared, modes: Vec<Box<dyn Mode>>) -> Self {
        let leader = KeyChord::parse(&shared.config.leader).unwrap_or_else(|| {
            tracing::warn!(leader = %shared.config.leader, "invalid leader key, using ctrl-]");
            KeyChord::parse("ctrl-]").expect("valid default")
        });
        // The demo keeps its Run history in memory only.
        let run_history =
            if shared.state.is_persistent() { crate::run::History::load_default() } else { crate::run::History::default() };
        Self {
            shared,
            modes,
            active_mode: 0,
            leader,
            leader_pending: false,
            menu: None,
            dialog: None,
            run_dialog: None,
            run_history,
            toast: None,
            chrome_hits: HitMap::default(),
            layout: AppLayout::default(),
            quit: false,
        }
    }

    pub async fn run(mut self, terminal: &mut Tui, mut rx: UnboundedReceiver<AppEvent>) -> Result<()> {
        let mut input = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut dirty = true;
        while !self.quit {
            if dirty {
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
                }
            }
        }
        Ok(())
    }

    // ---- Rendering ----

    fn render(&mut self, f: &mut Frame) {
        let theme = &self.shared.theme;
        f.render_widget(Paragraph::new("").style(Style::new().bg(theme.bg).fg(theme.fg)), f.area());
        let mode = &mut self.modes[self.active_mode];
        self.layout = AppLayout::new(f.area(), mode.toolbar_height());
        self.chrome_hits.clear();

        let tabs = mode.tabs();
        bars::render_tab_bar(f, self.layout.tab_bar, theme, mode.icon(), &tabs, mode.active_tab(), &mut self.chrome_hits);
        if self.layout.toolbar.height > 0 {
            mode.render_toolbar(f, self.layout.toolbar, &self.shared);
        }
        mode.render(f, self.layout.content, &self.shared);

        let mut status = mode.status(&self.shared);
        if self.leader_pending {
            status = format!(
                "{}: t new · w close · n/p next/prev · 1-9 mode · ? menu · q quit",
                crate::keys::display(&self.shared.config.leader)
            );
        }
        let mode_list: Vec<(String, String)> =
            self.modes.iter().map(|m| (m.icon().to_string(), m.name().to_string())).collect();
        let hints = self.modes[self.active_mode].hints(&self.shared);
        let bar = bars::BottomBar { modes: &mode_list, active: self.active_mode, status: &status, hints: &hints };
        bars::render_bottom_bar(f, self.layout.bottom_bar, theme, &bar, &mut self.chrome_hits);

        if let Some(toast) = &self.toast {
            let mut area = self.layout.content;
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

    /// Runs `f` against mode `idx` with a fresh context and applies the effects it produced.
    fn with_mode<R>(&mut self, idx: usize, f: impl FnOnce(&mut dyn Mode, &mut ModeCtx) -> R) -> R {
        let mut effects = Vec::new();
        let result = {
            let mut ctx = ModeCtx::new(&self.shared, &mut effects);
            f(self.modes[idx].as_mut(), &mut ctx)
        };
        let mut requests = Vec::new();
        for effect in effects {
            match effect {
                Effect::Request(req) => requests.push(req),
                Effect::Confirm(c) => {
                    self.menu = None;
                    self.dialog = Some(DialogState::new(idx, c));
                }
                Effect::Toast { msg, detail } => self.toast = Some(Toast::new(msg, detail)),
                Effect::Run(prompt) => {
                    self.menu = None;
                    let history = self.run_history.for_key(&prompt.key);
                    self.run_dialog = Some(RunDialogState::new(prompt, history));
                }
                Effect::OpenMenu { x, y, items } => {
                    self.menu = (!items.is_empty()).then(|| MenuState::new(MenuOwner::Mode(idx), (x, y), items));
                }
            }
        }
        for req in requests {
            self.route_request(req.as_ref());
        }
        result
    }

    fn route_request(&mut self, req: &dyn std::any::Any) {
        for i in 0..self.modes.len() {
            if self.with_mode(i, |m, ctx| m.handle_request(req, ctx)) {
                self.switch_mode(i);
                return;
            }
        }
        tracing::warn!("no mode handled a request");
    }

    fn dispatch_app_event(&mut self, ev: AppEvent) {
        for i in 0..self.modes.len() {
            if self.with_mode(i, |m, ctx| m.handle_app_event(&ev, ctx)) == EventResult::Consumed {
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
                let i = self.active_mode;
                self.with_mode(i, |m, ctx| m.handle_paste(&text, ctx));
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
                KeyCode::Enter => {
                    let confirm = if dialog.has_input() { dialog.can_confirm() } else { dialog.confirm_focused() };
                    if confirm {
                        self.accept_dialog();
                    } else if !dialog.has_input() {
                        self.dialog = None;
                    }
                }
                KeyCode::Backspace => dialog.backspace(),
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

        // Ctrl+Q quits from any mode, even when a terminal has focus.
        if key.code == KeyCode::Char('q') && key.modifiers == KeyModifiers::CONTROL {
            self.quit = true;
            return;
        }

        if self.leader_pending {
            self.leader_pending = false;
            if self.leader.matches(&key) {
                // Pressing the leader twice sends it to the mode.
                let i = self.active_mode;
                self.with_mode(i, |m, ctx| m.handle_key(key, ctx));
            } else {
                self.leader_command(key);
            }
            return;
        }
        if self.leader.matches(&key) {
            self.leader_pending = true;
            return;
        }

        let i = self.active_mode;
        if self.with_mode(i, |m, ctx| m.handle_key(key, ctx)) == EventResult::Consumed {
            return;
        }
        if !self.modes[i].captures_input() {
            self.global_shortcut(key);
        }
    }

    fn leader_command(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('t') | KeyCode::Char('c') => self.new_tab(),
            KeyCode::Char('w') | KeyCode::Char('x') => self.close_tab(self.modes[self.active_mode].active_tab()),
            KeyCode::Char('n') | KeyCode::Right => self.cycle_tab(1),
            KeyCode::Char('p') | KeyCode::Left => self.cycle_tab(-1),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char(c @ '1'..='9') => self.switch_mode(c as usize - '1' as usize),
            KeyCode::Char('m') | KeyCode::Tab => self.switch_mode((self.active_mode + 1) % self.modes.len()),
            KeyCode::Char('?') | KeyCode::Char(' ') => self.open_shortcuts_menu_from_keyboard(),
            // The mode's own shortcut items, by the key shown as their hint (e.g. `f` in Terminal).
            KeyCode::Char(c) => {
                let wanted = c.to_string();
                let action = self.modes[self.active_mode].shortcut_items().into_iter().find_map(|it| match it {
                    crate::modes::MenuItem::Action { hint, enabled: true, action, .. } if hint == wanted => Some(action),
                    _ => None,
                });
                if let Some(action) = action {
                    let i = self.active_mode;
                    self.with_mode(i, |m, ctx| m.on_action(action, ctx));
                }
            }
            _ => {}
        }
    }

    /// Shortcuts for modes that do not capture every key.
    fn global_shortcut(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('t') if ctrl => self.new_tab(),
            KeyCode::Char('w') if ctrl => self.close_tab(self.modes[self.active_mode].active_tab()),
            KeyCode::Tab => self.cycle_tab(1),
            KeyCode::BackTab => self.cycle_tab(-1),
            KeyCode::F(n @ 1..=9) => self.switch_mode(n as usize - 1),
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

        let on_chrome = |r: ratatui::layout::Rect| r.contains((x, y).into());
        if matches!(ev.kind, MouseEventKind::Down(_)) && (on_chrome(self.layout.tab_bar) || on_chrome(self.layout.bottom_bar)) {
            if ev.kind == MouseEventKind::Down(MouseButton::Left) {
                match self.chrome_hits.hit(x, y) {
                    Some(ChromeTarget::Tab(i)) => self.modes[self.active_mode].select_tab(i),
                    Some(ChromeTarget::CloseTab(i)) => self.close_tab(i),
                    Some(ChromeTarget::NewTab) => self.new_tab(),
                    Some(ChromeTarget::Mode(i)) => self.switch_mode(i),
                    Some(ChromeTarget::Quit) => self.quit = true,
                    Some(ChromeTarget::Shortcuts) => self.open_shortcuts_menu(x, y),
                    Some(ChromeTarget::Hint(action)) => {
                        let i = self.active_mode;
                        self.with_mode(i, |m, ctx| m.on_action(action, ctx));
                    }
                    None => {}
                }
            }
            return;
        }
        let i = self.active_mode;
        self.with_mode(i, |m, ctx| m.handle_mouse(ev, ctx));
    }

    fn run_menu_action(&mut self, action: u32) {
        if let Some(menu) = self.menu.take() {
            match menu.owner {
                MenuOwner::Mode(i) => self.with_mode(i, |m, ctx| m.on_action(action, ctx)),
                MenuOwner::App => self.run_app_action(action),
            }
        }
    }

    /// The leader-key commands as a clickable menu, each with its key as the hint.
    fn open_shortcuts_menu(&mut self, x: u16, _y: u16) {
        use crate::modes::MenuItem;
        let mode = &self.modes[self.active_mode];
        let closable = mode.tabs().len() > 1;
        // Modes that capture input (Terminal) use the keys after the leader; the others
        // also have direct shortcuts (see `global_shortcut`), which are shorter to show.
        let leader = mode.captures_input();
        let key = |after_leader: &str, direct: &str| if leader { after_leader.to_string() } else { direct.to_string() };
        let mut items = vec![
            MenuItem::action("New tab", app_action::NEW_TAB).with_hint(key("t", "Ctrl-t")),
            MenuItem::action("Close tab", app_action::CLOSE_TAB).with_hint(key("w", "Ctrl-w")).enabled(closable),
            MenuItem::action("Next tab", app_action::NEXT_TAB).with_hint(key("n", "Tab")),
            MenuItem::action("Previous tab", app_action::PREV_TAB).with_hint(key("p", "Shift-Tab")),
            MenuItem::Separator,
        ];
        let own = mode.shortcut_items();
        if !own.is_empty() {
            items.extend(own.into_iter().map(|it| match it {
                MenuItem::Action { label, hint, enabled, danger, action } => {
                    MenuItem::Action { label, hint, enabled, danger, action: app_action::MODE_ACTION_BASE + action }
                }
                sep => sep,
            }));
            items.push(MenuItem::Separator);
        }
        for (i, m) in self.modes.iter().enumerate().take(9) {
            if i != self.active_mode {
                items.push(
                    MenuItem::action(format!("Switch to {}", m.name()), app_action::MODE_BASE + i as u32)
                        .with_hint(key(&(i + 1).to_string(), &format!("F{}", i + 1))),
                );
            }
        }
        items.push(MenuItem::Separator);
        items.push(MenuItem::action("Quit", app_action::QUIT).with_hint(key("q", "q")));
        let top_of_bar = self.layout.bottom_bar.y;
        self.menu = Some(MenuState::new(MenuOwner::App, (x.saturating_sub(4), top_of_bar), items).above());
    }

    /// Opens the shortcuts menu at the right of the bottom bar, where its hint is.
    fn open_shortcuts_menu_from_keyboard(&mut self) {
        let x = self.layout.bottom_bar.right().saturating_sub(34);
        self.open_shortcuts_menu(x + 4, self.layout.bottom_bar.y);
    }

    fn run_app_action(&mut self, action: u32) {
        match action {
            app_action::NEW_TAB => self.new_tab(),
            app_action::CLOSE_TAB => self.close_tab(self.modes[self.active_mode].active_tab()),
            app_action::NEXT_TAB => self.cycle_tab(1),
            app_action::PREV_TAB => self.cycle_tab(-1),
            app_action::QUIT => self.quit = true,
            a if a >= app_action::MODE_ACTION_BASE => {
                let i = self.active_mode;
                self.with_mode(i, |m, ctx| m.on_action(a - app_action::MODE_ACTION_BASE, ctx));
            }
            a if a >= app_action::MODE_BASE => self.switch_mode((a - app_action::MODE_BASE) as usize),
            _ => {}
        }
    }

    /// Enter / Run in the Run dialog: types the command into a new terminal tab.
    fn submit_run(&mut self) {
        let Some(run) = &mut self.run_dialog else { return };
        match run.submit() {
            RunOutcome::Stay => {}
            RunOutcome::Run { command, prefix, args } => {
                let Some(run) = self.run_dialog.take() else { return };
                let p = run.prompt;
                self.run_history.record(&p.key, &prefix, &args);
                let label = crate::run::tab_label(&prefix, &p.paths);
                self.route_request(&crate::modes::requests::OpenTerminal { cwd: p.cwd, command: Some(command), label: Some(label) });
            }
        }
    }

    fn accept_dialog(&mut self) {
        if let Some(d) = self.dialog.take() {
            let action = d.confirm.action;
            self.with_mode(d.mode, |m, ctx| m.on_action(action, ctx));
        }
    }

    // ---- Tabs and modes ----

    fn new_tab(&mut self) {
        let i = self.active_mode;
        self.with_mode(i, |m, ctx| m.new_tab(ctx));
    }

    fn close_tab(&mut self, idx: usize) {
        let i = self.active_mode;
        if self.modes[i].tabs().len() > 1 {
            self.with_mode(i, |m, ctx| m.close_tab(idx, ctx));
        }
    }

    fn cycle_tab(&mut self, delta: i32) {
        let mode = &mut self.modes[self.active_mode];
        let n = mode.tabs().len() as i32;
        if n > 0 {
            let next = (mode.active_tab() as i32 + delta).rem_euclid(n);
            mode.select_tab(next as usize);
        }
    }

    fn switch_mode(&mut self, idx: usize) {
        if idx < self.modes.len() {
            self.active_mode = idx;
            self.menu = None;
        }
    }
}
