//! Context menu drawn as an overlay next to the click and kept inside the screen.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use super::{hit::HitMap, theme::Theme};
use crate::modes::MenuItem;

pub struct MenuState {
    /// Index of the mode that opened the menu.
    pub mode: usize,
    items: Vec<MenuItem>,
    anchor: (u16, u16),
    selected: Option<usize>,
    rect: Rect,
    hits: HitMap<usize>,
}

impl MenuState {
    pub fn new(mode: usize, anchor: (u16, u16), items: Vec<MenuItem>) -> Self {
        let mut menu = Self { mode, items, anchor, selected: None, rect: Rect::default(), hits: HitMap::default() };
        menu.selected = menu.enabled_indices().first().copied();
        menu
    }

    fn enabled_indices(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| matches!(it, MenuItem::Action { enabled: true, .. }))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.rect.contains((x, y).into())
    }

    /// Index of the enabled item under the pointer.
    pub fn hit(&self, x: u16, y: u16) -> Option<usize> {
        self.hits.hit(x, y).filter(|i| self.enabled_indices().contains(i))
    }

    pub fn hover(&mut self, x: u16, y: u16) {
        if let Some(i) = self.hit(x, y) {
            self.selected = Some(i);
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        let enabled = self.enabled_indices();
        if enabled.is_empty() {
            return;
        }
        let pos = self
            .selected
            .and_then(|s| enabled.iter().position(|&i| i == s))
            .map(|p| p as i32)
            .unwrap_or(-1);
        let next = (pos + delta).rem_euclid(enabled.len() as i32) as usize;
        self.selected = Some(enabled[next]);
    }

    pub fn action_at(&self, idx: usize) -> Option<u32> {
        match self.items.get(idx) {
            Some(MenuItem::Action { enabled: true, action, .. }) => Some(*action),
            _ => None,
        }
    }

    pub fn selected_action(&self) -> Option<u32> {
        self.selected.and_then(|i| self.action_at(i))
    }

    pub fn render(&mut self, f: &mut Frame, theme: &Theme) {
        let screen = f.area();
        let inner_w = self
            .items
            .iter()
            .map(|it| match it {
                MenuItem::Action { label, hint, .. } => {
                    label.width() + if hint.is_empty() { 0 } else { hint.width() + 3 }
                }
                MenuItem::Separator => 0,
            })
            .max()
            .unwrap_or(0)
            .max(24) as u16
            + 4;
        let w = (inner_w + 2).min(screen.width);
        let h = (self.items.len() as u16 + 4).min(screen.height);
        let x = self.anchor.0.min(screen.width.saturating_sub(w));
        let y = self.anchor.1.min(screen.height.saturating_sub(h));
        self.rect = Rect::new(x, y, w, h);
        self.hits.clear();

        f.render_widget(Clear, self.rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme.border))
            .style(Style::new().bg(theme.raised_bg).fg(theme.fg));
        let inner = block.inner(self.rect).inner(ratatui::layout::Margin::new(1, 1));
        f.render_widget(block, self.rect);

        for (i, item) in self.items.iter().enumerate() {
            let row = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
            if row.y >= inner.bottom() {
                break;
            }
            let line = match item {
                MenuItem::Separator => {
                    Line::styled("─".repeat(inner.width as usize), Style::new().fg(theme.border))
                }
                MenuItem::Action { label, hint, enabled, .. } => {
                    let selected = self.selected == Some(i);
                    let mut style = Style::new().fg(if *enabled { theme.fg } else { theme.fg_disabled });
                    if selected {
                        style = style.bg(theme.hover_bg).add_modifier(Modifier::BOLD);
                    }
                    let pad = (inner.width as usize).saturating_sub(label.width() + hint.width() + 4);
                    self.hits.push(row, i);
                    Line::from(vec![
                        Span::styled(format!("  {label}{}", " ".repeat(pad)), style),
                        Span::styled(format!("{hint}  "), style.fg(theme.fg_muted).remove_modifier(Modifier::BOLD)),
                    ])
                }
            };
            f.render_widget(Paragraph::new(line), row);
        }
    }
}
