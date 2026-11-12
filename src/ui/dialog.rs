//! Modal confirmation dialog drawn over everything. Focus starts on Cancel so a stray
//! Enter never confirms; destructive actions can also require typing a word (e.g. DELETE).

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::{bars::truncate, theme::Theme};

const MAX_ITEMS: usize = 6;

/// What a mode asks the user to confirm. On confirm, `action` comes back through
/// [`crate::modes::Mode::on_action`].
#[derive(Debug, Clone)]
pub struct Confirm {
    pub title: String,
    pub message: String,
    /// Optional list shown under the message (e.g. the files to delete).
    pub items: Vec<String>,
    pub confirm_label: String,
    /// Draws the confirm button in the danger color.
    pub danger: bool,
    /// When set, the user must type this word (case-insensitive) before confirming.
    pub require_text: Option<String>,
    /// When set, the dialog asks for free text (e.g. a folder name); on confirm the text
    /// comes back through [`crate::modes::Pane::on_text`].
    pub text: Option<TextInput>,
    pub action: u32,
}

/// A free text field in a dialog.
#[derive(Debug, Clone)]
pub struct TextInput {
    pub label: String,
    pub initial: String,
    /// Why the text cannot be used yet, if it cannot (shown under the field).
    pub validate: fn(&str) -> Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogHit {
    Cancel,
    Confirm,
    Inside,
}

pub struct DialogState {
    pub mode: usize,
    pub confirm: Confirm,
    focus_confirm: bool,
    input: String,
    /// The suggested text is selected: the first key typed replaces it.
    selected: bool,
    /// Cursor position in the free text field, in chars (the confirm word is always typed at the end).
    cursor: usize,
    rect: Rect,
    cancel_rect: Rect,
    confirm_rect: Rect,
}

impl DialogState {
    pub fn new(mode: usize, confirm: Confirm) -> Self {
        let input = confirm.text.as_ref().map(|t| t.initial.clone()).unwrap_or_default();
        Self {
            mode,
            // A text dialog confirms with Enter, so the confirm button starts focused.
            focus_confirm: confirm.text.is_some(),
            selected: !input.is_empty(),
            cursor: input.chars().count(),
            confirm,
            input,
            rect: Rect::default(),
            cancel_rect: Rect::default(),
            confirm_rect: Rect::default(),
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus_confirm = !self.focus_confirm;
    }

    pub fn focus_confirm(&mut self, value: bool) {
        self.focus_confirm = value;
    }

    pub fn confirm_focused(&self) -> bool {
        self.focus_confirm
    }

    pub fn has_input(&self) -> bool {
        self.confirm.require_text.is_some() || self.confirm.text.is_some()
    }

    /// Byte offset of the cursor in `input`.
    fn cursor_byte(&self) -> usize {
        self.input.char_indices().nth(self.cursor).map_or(self.input.len(), |(i, _)| i)
    }

    fn len_chars(&self) -> usize {
        self.input.chars().count()
    }

    pub fn type_text(&mut self, text: &str) {
        if !self.has_input() {
            return;
        }
        if std::mem::take(&mut self.selected) {
            self.clear_input();
        }
        if self.confirm.text.is_none() {
            self.cursor = self.len_chars();
        }
        let max = if self.confirm.text.is_some() { 255 } else { 64 };
        for c in text.chars().filter(|c| !c.is_control()) {
            if self.input.len() + c.len_utf8() > max {
                break;
            }
            let at = self.cursor_byte();
            self.input.insert(at, c);
            self.cursor += 1;
        }
    }

    pub fn backspace(&mut self) {
        if std::mem::take(&mut self.selected) {
            self.clear_input();
        } else if self.confirm.text.is_none() {
            self.input.pop();
            self.cursor = self.len_chars();
        } else if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.cursor_byte();
            self.input.remove(at);
        }
    }

    /// Delete: removes the char after the cursor (the whole suggestion when it is selected).
    pub fn delete_forward(&mut self) {
        if std::mem::take(&mut self.selected) {
            self.clear_input();
        } else if self.cursor < self.len_chars() {
            let at = self.cursor_byte();
            self.input.remove(at);
        }
    }

    /// ←/→/Home/End keep the text and move the cursor. On the selected suggestion,
    /// ← and Home go to its start, → and End to its end (like an editor).
    pub fn move_cursor(&mut self, key: KeyCode) {
        let was_selected = std::mem::take(&mut self.selected);
        if self.confirm.text.is_none() {
            return;
        }
        let len = self.len_chars();
        self.cursor = match key {
            KeyCode::Home => 0,
            KeyCode::End => len,
            KeyCode::Left if was_selected => 0,
            KeyCode::Right if was_selected => len,
            KeyCode::Left => self.cursor.saturating_sub(1),
            KeyCode::Right => (self.cursor + 1).min(len),
            _ => self.cursor,
        };
    }

    /// Ctrl+U: clears the field.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.selected = false;
    }

    /// The text typed in a free text dialog.
    pub fn text(&self) -> Option<&str> {
        self.confirm.text.as_ref().map(|_| self.input.as_str())
    }

    fn text_problem(&self) -> Option<&'static str> {
        self.confirm.text.as_ref().and_then(|t| (t.validate)(&self.input))
    }

    /// True when the confirm button may be used.
    pub fn can_confirm(&self) -> bool {
        if self.confirm.text.is_some() {
            return self.text_problem().is_none();
        }
        match &self.confirm.require_text {
            Some(word) => self.input.trim().eq_ignore_ascii_case(word),
            None => true,
        }
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<DialogHit> {
        let p = (x, y).into();
        if self.confirm_rect.contains(p) {
            Some(DialogHit::Confirm)
        } else if self.cancel_rect.contains(p) {
            Some(DialogHit::Cancel)
        } else if self.rect.contains(p) {
            Some(DialogHit::Inside)
        } else {
            None
        }
    }

    pub fn render(&mut self, f: &mut Frame, theme: &Theme) {
        let screen = f.area();
        let c = &self.confirm;
        let w = 64.min(screen.width.saturating_sub(4)).max(20);
        let inner_w = w.saturating_sub(6) as usize;

        let mut lines = vec![Line::from(Span::styled(
            c.title.clone(),
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        ))];
        lines.push(Line::raw(""));
        lines.push(Line::styled(c.message.clone(), Style::new().fg(theme.fg_muted)));
        let message_rows = c.message.width().div_ceil(inner_w.max(1)).max(1);
        if !c.items.is_empty() {
            lines.push(Line::raw(""));
            for item in c.items.iter().take(MAX_ITEMS) {
                lines.push(Line::styled(
                    format!("  • {}", truncate(item, inner_w.saturating_sub(4))),
                    Style::new().fg(theme.fg),
                ));
            }
            if c.items.len() > MAX_ITEMS {
                lines.push(Line::styled(
                    format!("  … and {} more", c.items.len() - MAX_ITEMS),
                    Style::new().fg(theme.fg_muted),
                ));
            }
        }
        let input_rows: u16 = if c.require_text.is_some() || c.text.is_some() { 3 } else { 0 };
        let text_rows = lines.len() + message_rows - 1;
        // border + padding + text + [label + input + gap] + gap + buttons + padding + border
        let h = (text_rows as u16 + input_rows + 6).min(screen.height);
        let rect = Rect::new(
            screen.x + screen.width.saturating_sub(w) / 2,
            screen.y + screen.height.saturating_sub(h) / 2,
            w,
            h,
        );
        self.rect = rect;

        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if c.danger { theme.danger } else { theme.border }))
            .style(Style::new().bg(theme.raised_bg));
        let inner = block.inner(rect).inner(Margin::new(2, 1));
        f.render_widget(block, rect);
        let below = if input_rows > 0 { 5 } else { 2 };
        let text_area = Rect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(below));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text_area);

        if let Some(t) = &c.text {
            let y = inner.bottom().saturating_sub(4);
            f.render_widget(
                Paragraph::new(Span::styled(t.label.clone(), Style::new().fg(theme.fg_muted))),
                Rect::new(inner.x, y, inner.width, 1),
            );
            let field = Rect::new(inner.x, y + 1, inner.width, 1);
            let width = inner.width.saturating_sub(3).max(1) as usize;
            let mut spans = vec![Span::raw(" ")];
            if self.selected {
                // Selected text looks like a selection in an editor: typing replaces it.
                let shown = crate::ui::bars::truncate_start(&self.input, width);
                spans.push(Span::styled(shown, Style::new().fg(theme.fg).bg(theme.accent_tint)));
            } else {
                let chars: Vec<char> = self.input.chars().collect();
                // Scroll so the cursor (one cell past the end at most) stays visible.
                let start = (self.cursor + 1).saturating_sub(width);
                let end = (start + width).min(chars.len());
                let text = Style::new().fg(theme.fg);
                let before: String = chars[start..self.cursor].iter().collect();
                spans.push(Span::styled(before, text));
                match chars.get(self.cursor) {
                    Some(c) => {
                        spans.push(Span::styled(c.to_string(), Style::new().fg(theme.bg).bg(theme.accent)));
                        let after: String = chars[self.cursor + 1..end.max(self.cursor + 1)].iter().collect();
                        spans.push(Span::styled(after, text));
                    }
                    None => spans.push(Span::styled("█", Style::new().fg(theme.accent))),
                }
            }
            f.render_widget(Paragraph::new(Line::from(spans)).style(Style::new().bg(theme.bg)), field);
            if let Some(problem) = self.text_problem().filter(|_| !self.input.is_empty()) {
                f.render_widget(
                    Paragraph::new(Span::styled(problem, Style::new().fg(theme.danger))),
                    Rect::new(inner.x, y + 2, inner.width, 1),
                );
            }
        }

        if let Some(word) = &c.require_text {
            let y = inner.bottom().saturating_sub(4);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Type ", Style::new().fg(theme.fg_muted)),
                    Span::styled(word.clone(), Style::new().fg(theme.fg).add_modifier(Modifier::BOLD)),
                    Span::styled(" to confirm:", Style::new().fg(theme.fg_muted)),
                ])),
                Rect::new(inner.x, y, inner.width, 1),
            );
            let ok = self.can_confirm();
            let field = Rect::new(inner.x, y + 1, inner.width, 1);
            let shown = truncate(&self.input, inner.width.saturating_sub(4) as usize);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" ", Style::new()),
                    Span::styled(shown, Style::new().fg(if ok { theme.danger } else { theme.fg })),
                    Span::styled("█", Style::new().fg(theme.accent)),
                ]))
                .style(Style::new().bg(theme.bg)),
                field,
            );
        }

        // Buttons, right-aligned on the last inner row.
        let y = inner.bottom().saturating_sub(1);
        let cancel = "  Cancel  ";
        let confirm = format!("  {}  ", c.confirm_label);
        let (cw, kw) = (cancel.width() as u16, confirm.width() as u16);
        self.confirm_rect = Rect::new(inner.right().saturating_sub(kw), y, kw, 1);
        self.cancel_rect = Rect::new(self.confirm_rect.x.saturating_sub(cw + 2), y, cw, 1);

        let focused = |on: bool, s: Style| if on { s.add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { s };
        let cancel_style = focused(!self.focus_confirm, Style::new().bg(theme.hover_bg).fg(theme.fg));
        let confirm_style = if !self.can_confirm() {
            Style::new().bg(theme.hover_bg).fg(theme.fg_disabled)
        } else if c.danger {
            Style::new().bg(theme.danger_tint).fg(theme.danger)
        } else {
            Style::new().bg(theme.accent_tint).fg(theme.accent)
        };
        let confirm_style = focused(self.focus_confirm, confirm_style);
        f.render_widget(Paragraph::new(Span::styled(cancel, cancel_style)), self.cancel_rect);
        f.render_widget(Paragraph::new(Span::styled(confirm, confirm_style)), self.confirm_rect);
        let hint = if c.require_text.is_some() || c.text.is_some() { "Esc cancel · Enter confirm" } else { "Esc cancel · Tab switch · Enter choose" };
        if (hint.width() as u16) + cw + kw + 4 <= inner.width {
            f.render_widget(
                Paragraph::new(Span::styled(hint, Style::new().fg(theme.fg_disabled))),
                Rect::new(inner.x, y, hint.width() as u16, 1),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_dialog() -> DialogState {
        DialogState::new(0, Confirm {
            title: "New file".into(),
            message: String::new(),
            items: Vec::new(),
            confirm_label: "Create".into(),
            danger: false,
            require_text: None,
            text: Some(TextInput { label: "Name:".into(), initial: "New file.txt".into(), validate: crate::modes::files::ops::new_name_problem }),
            action: 1,
        })
    }

    #[test]
    fn typing_replaces_the_selected_suggestion() {
        let mut d = name_dialog();
        assert_eq!(d.text(), Some("New file.txt"));
        assert!(d.can_confirm());
        d.type_text("n");
        d.type_text("otes.md");
        assert_eq!(d.text(), Some("notes.md"));
    }

    #[test]
    fn backspace_clears_the_selection_and_arrows_keep_it() {
        let mut d = name_dialog();
        d.backspace();
        assert_eq!(d.text(), Some(""));
        assert!(!d.can_confirm());

        let mut d = name_dialog();
        d.move_cursor(KeyCode::Right);
        d.type_text("x");
        assert_eq!(d.text(), Some("New file.txtx"));
        d.backspace();
        assert_eq!(d.text(), Some("New file.txt"));
    }

    #[test]
    fn arrows_move_the_cursor_inside_the_name() {
        let mut d = name_dialog();
        // ← on the selection goes to the start.
        d.move_cursor(KeyCode::Left);
        d.type_text("My ");
        assert_eq!(d.text(), Some("My New file.txt"));
        // Before ".txt": End, then 4 × ←.
        d.move_cursor(KeyCode::End);
        for _ in 0..4 {
            d.move_cursor(KeyCode::Left);
        }
        d.backspace();
        d.type_text("é");
        assert_eq!(d.text(), Some("My New filé.txt"));
        d.delete_forward();
        assert_eq!(d.text(), Some("My New filétxt"));
        d.move_cursor(KeyCode::Home);
        d.delete_forward();
        assert_eq!(d.text(), Some("y New filétxt"));
    }
}
