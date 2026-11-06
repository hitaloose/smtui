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
        menu::MenuState,
        terminal_guard::Tui,
        toast::Toast,
    },
};

pub struct App {
    shared: Shared,
    modes: Vec<Box<dyn Mode>>,
    active_mode: usize,
    leader: KeyChord,
    leader_pending: bool,
    menu: Option<MenuState>,
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
        Self {
            shared,
            modes,
            active_mode: 0,
            leader,
            leader_pending: false,
            menu: None,
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
            status = format!("[{}] t new · w close · n/p next/prev · 1-9 mode · q quit", self.shared.config.leader);
        }
        let mode_list: Vec<(String, String)> =
            self.modes.iter().map(|m| (m.icon().to_string(), m.name().to_string())).collect();
        bars::render_bottom_bar(
            f,
            self.layout.bottom_bar,
            theme,
            &mode_list,
            self.active_mode,
            &status,
            &mut self.chrome_hits,
        );

        if let Some(toast) = &self.toast {
            let mut area = self.layout.content;
            area.height = area.height.saturating_sub(1);
            toast.render(f, area, theme);
        }
        if let Some(menu) = &mut self.menu {
            menu.render(f, theme);
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
                Effect::Toast { msg, detail } => self.toast = Some(Toast::new(msg, detail)),
                Effect::OpenMenu { x, y, items } => {
                    self.menu = (!items.is_empty()).then(|| MenuState::new(idx, (x, y), items));
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
            Event::Paste(text) => {
                if self.menu.is_none() {
                    let i = self.active_mode;
                    self.with_mode(i, |m, ctx| m.handle_paste(&text, ctx));
                }
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
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
            KeyCode::Char('q') if key.modifiers.is_empty() => self.quit = true,
            _ => {}
        }
    }

    fn handle_mouse(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
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
                MouseEventKind::Down(_) if !menu.contains(x, y) => self.menu = None,
                _ => {}
            }
            return;
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
            self.with_mode(menu.mode, |m, ctx| m.on_menu_action(action, ctx));
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
