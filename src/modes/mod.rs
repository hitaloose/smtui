//! The `Mode` trait. The tab bar and the bottom bar are generated from the list of
//! registered modes, so a new mode (processes, docker, logs…) only has to implement
//! this trait and be registered in `App::new`.

pub mod files;
pub mod requests;
pub mod terminal;

use std::any::Any;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};

use crate::{config::Config, event::AppEvent, event::EventSender, host::HostInfo, ui::theme::Theme};

#[derive(Debug, Clone)]
pub struct TabInfo {
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventResult {
    Consumed,
    Ignored,
}

#[derive(Debug, Clone)]
pub enum MenuItem {
    Action { label: String, hint: String, enabled: bool, action: u32 },
    Separator,
}

impl MenuItem {
    pub fn action(label: impl Into<String>, action: u32) -> Self {
        MenuItem::Action { label: label.into(), hint: String::new(), enabled: true, action }
    }

    pub fn with_hint(mut self, text: impl Into<String>) -> Self {
        if let MenuItem::Action { hint, .. } = &mut self {
            *hint = text.into();
        }
        self
    }

    pub fn enabled(mut self, value: bool) -> Self {
        if let MenuItem::Action { enabled, .. } = &mut self {
            *enabled = value;
        }
        self
    }
}

/// Things a mode asks the app to do.
pub enum Effect {
    Toast { msg: String, detail: String },
    OpenMenu { x: u16, y: u16, items: Vec<MenuItem> },
    /// Offered to every mode; the first one that accepts it becomes the active mode.
    Request(Box<dyn Any>),
}

/// App-wide state that modes may read.
pub struct Shared {
    pub theme: Theme,
    pub config: Config,
    pub host: HostInfo,
    pub tx: EventSender,
}

pub struct ModeCtx<'a> {
    pub shared: &'a Shared,
    effects: &'a mut Vec<Effect>,
}

impl<'a> ModeCtx<'a> {
    pub fn new(shared: &'a Shared, effects: &'a mut Vec<Effect>) -> Self {
        Self { shared, effects }
    }

    pub fn toast(&mut self, msg: impl Into<String>, detail: impl Into<String>) {
        self.effects.push(Effect::Toast { msg: msg.into(), detail: detail.into() });
    }

    /// Opens a context menu at screen position `(x, y)`; the chosen `action` comes back
    /// through [`Mode::on_menu_action`].
    pub fn open_menu(&mut self, x: u16, y: u16, items: Vec<MenuItem>) {
        self.effects.push(Effect::OpenMenu { x, y, items });
    }

    /// Sends a request (see [`requests`]) to the mode that handles it and switches to that mode.
    pub fn request<T: Any>(&mut self, request: T) {
        self.effects.push(Effect::Request(Box::new(request)));
    }
}

pub trait Mode {
    fn name(&self) -> &str;
    /// A short glyph shown before the name, e.g. `>_`.
    fn icon(&self) -> &str;

    fn tabs(&self) -> Vec<TabInfo>;
    fn active_tab(&self) -> usize;
    fn select_tab(&mut self, idx: usize);
    fn new_tab(&mut self, ctx: &mut ModeCtx);
    /// Only called when the mode has more than one tab.
    fn close_tab(&mut self, idx: usize, ctx: &mut ModeCtx);

    /// Height of the mode's own bar below the tab bar (0 for none).
    fn toolbar_height(&self) -> u16 {
        0
    }
    fn render_toolbar(&mut self, _f: &mut Frame, _area: Rect, _shared: &Shared) {}
    /// Draws the content and records the mode's own hit targets.
    fn render(&mut self, f: &mut Frame, area: Rect, shared: &Shared);

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut ModeCtx) -> EventResult;
    fn handle_mouse(&mut self, ev: MouseEvent, ctx: &mut ModeCtx) -> EventResult;
    fn handle_paste(&mut self, _text: &str, _ctx: &mut ModeCtx) -> EventResult {
        EventResult::Ignored
    }
    fn handle_app_event(&mut self, _ev: &AppEvent, _ctx: &mut ModeCtx) -> EventResult {
        EventResult::Ignored
    }
    fn on_menu_action(&mut self, _action: u32, _ctx: &mut ModeCtx) {}
    /// Returns true when the mode handled a request sent by another mode.
    fn handle_request(&mut self, _request: &dyn Any, _ctx: &mut ModeCtx) -> bool {
        false
    }

    fn status(&self, shared: &Shared) -> String;

    /// When true, every key goes to the mode (e.g. a terminal); only the leader key
    /// reaches the app's global shortcuts.
    fn captures_input(&self) -> bool {
        false
    }
}
