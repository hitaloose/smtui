//! The two panes of a workspace tab: Files (top) and Terminal (bottom). Each pane keeps one
//! entry per tab, aligned by index; the app keeps them in step (see `App::new_tab`).

pub mod files;
pub mod requests;
pub mod terminal;

use std::any::Any;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};

use crate::ui::dialog::Confirm;
use crate::{config::Config, event::AppEvent, event::EventSender, host::HostInfo, ui::theme::Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventResult {
    Consumed,
    Ignored,
}

#[derive(Debug, Clone)]
pub enum MenuItem {
    Action { label: String, hint: String, enabled: bool, danger: bool, action: u32 },
    Separator,
}

impl MenuItem {
    pub fn action(label: impl Into<String>, action: u32) -> Self {
        MenuItem::Action { label: label.into(), hint: String::new(), enabled: true, danger: false, action }
    }

    pub fn with_hint(mut self, text: impl Into<String>) -> Self {
        if let MenuItem::Action { hint, .. } = &mut self {
            *hint = text.into();
        }
        self
    }

    /// Draws the item in the danger color (destructive actions).
    pub fn danger(mut self) -> Self {
        if let MenuItem::Action { danger, .. } = &mut self {
            *danger = true;
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

/// Things a pane asks the app to do.
pub enum Effect {
    Toast { msg: String, detail: String },
    OpenMenu { x: u16, y: u16, items: Vec<MenuItem> },
    /// Modal confirmation; on confirm its `action` comes back through [`Pane::on_action`].
    Confirm(Confirm),
    /// A [`requests`] value the app carries out (it may touch both panes).
    Request(Box<dyn Any>),
    /// The "Run…" dialog; on accept the command is typed into the tab's terminal.
    Run(crate::run::RunPrompt),
}

/// App-wide state that panes may read.
pub struct Shared {
    pub theme: Theme,
    pub config: Config,
    pub host: HostInfo,
    pub tx: EventSender,
    /// Preferences remembered between runs.
    pub state: crate::state::StateStore,
    /// `--demo`: sample files that are not on disk, and nothing real on screen.
    pub demo: bool,
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
    /// through [`Pane::on_action`].
    pub fn open_menu(&mut self, x: u16, y: u16, items: Vec<MenuItem>) {
        self.effects.push(Effect::OpenMenu { x, y, items });
    }

    pub fn confirm(&mut self, confirm: Confirm) {
        self.effects.push(Effect::Confirm(confirm));
    }

    /// Opens the "Run…" dialog for files.
    pub fn run(&mut self, prompt: crate::run::RunPrompt) {
        self.effects.push(Effect::Run(prompt));
    }

    /// Asks the app to carry out a request (see [`requests`]).
    pub fn request<T: Any>(&mut self, request: T) {
        self.effects.push(Effect::Request(Box::new(request)));
    }
}

pub trait Pane {
    /// Height of the pane's own bar above its content (0 for none).
    fn toolbar_height(&self) -> u16 {
        0
    }
    fn render_toolbar(&mut self, _f: &mut Frame, _area: Rect, _shared: &Shared) {}
    /// Draws the active tab's content and records the pane's own hit targets.
    fn render(&mut self, f: &mut Frame, area: Rect, shared: &Shared);

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut ModeCtx) -> EventResult;
    fn handle_mouse(&mut self, ev: MouseEvent, ctx: &mut ModeCtx) -> EventResult;
    fn handle_paste(&mut self, _text: &str, _ctx: &mut ModeCtx) -> EventResult {
        EventResult::Ignored
    }
    fn handle_app_event(&mut self, _ev: &AppEvent, _ctx: &mut ModeCtx) -> EventResult {
        EventResult::Ignored
    }
    /// A context menu item was chosen, or a confirmation dialog was accepted.
    fn on_action(&mut self, _action: u32, _ctx: &mut ModeCtx) {}

    /// Status line text while this pane has the focus.
    fn status(&self, shared: &Shared) -> String;

    /// Items for the app's shortcuts menu; a chosen item comes back through
    /// [`Pane::on_action`]. The hint is the key (pressed directly, or after the leader).
    fn shortcut_items(&self) -> Vec<MenuItem> {
        Vec::new()
    }

    /// When true, every key goes to the pane (e.g. a terminal); only the leader key
    /// reaches the app's global shortcuts.
    fn captures_input(&self) -> bool {
        false
    }
}
