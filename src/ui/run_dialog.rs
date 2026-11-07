//! The "Run…" dialog: command field, the file path (not editable, always quoted), args
//! field and a preview of the exact command line that will be typed into a new terminal.

use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use super::{bars::truncate, theme::Theme};
use crate::run::{self, RunPrompt};

const FIELD_MAX: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Command,
    Args,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunHit {
    Field(Field),
    Suggestion(usize),
    Cancel,
    Run,
    Inside,
}

pub enum RunOutcome {
    /// Keep the dialog open.
    Stay,
    /// Run this command line.
    Run { command: String, prefix: String, args: String },
}

pub struct RunDialogState {
    pub prompt: RunPrompt,
    command: String,
    args: String,
    focus: Field,
    /// Past (command, args) for this file type, newest first.
    history: Vec<(String, String)>,
    /// Position in `history` while browsing it with ↑/↓.
    hist_pos: Option<usize>,
    /// A destructive-looking command needs a second Enter.
    armed: bool,
    rect: Rect,
    fields: [(Field, Rect); 2],
    suggestion_rects: Vec<Rect>,
    cancel_rect: Rect,
    run_rect: Rect,
}

impl RunDialogState {
    pub fn new(prompt: RunPrompt, history: Vec<(String, String)>) -> Self {
        let (command, args) = match history.first() {
            Some((c, a)) => (c.clone(), a.clone()),
            None => (prompt.suggestions.first().cloned().unwrap_or_default(), String::new()),
        };
        Self {
            prompt,
            command,
            args,
            focus: Field::Command,
            history,
            hist_pos: None,
            armed: false,
            rect: Rect::default(),
            fields: [(Field::Command, Rect::default()), (Field::Args, Rect::default())],
            suggestion_rects: Vec::new(),
            cancel_rect: Rect::default(),
            run_rect: Rect::default(),
        }
    }

    fn field_mut(&mut self) -> &mut String {
        match self.focus {
            Field::Command => &mut self.command,
            Field::Args => &mut self.args,
        }
    }

    fn edited(&mut self) {
        self.armed = false;
        self.hist_pos = None;
    }

    pub fn command_line(&self) -> String {
        run::build_command(&self.command, &self.prompt.paths, &self.args)
    }

    fn blocked(&self) -> Option<&'static str> {
        run::blocked_reason(&self.prompt, &self.command)
    }

    fn destructive(&self) -> bool {
        run::looks_destructive(&self.command_line())
    }

    pub fn focus(&mut self, field: Field) {
        self.focus = field;
    }

    pub fn next_field(&mut self) {
        self.focus = match self.focus {
            Field::Command => Field::Args,
            Field::Args => Field::Command,
        };
    }

    pub fn type_text(&mut self, text: &str) {
        let field = self.field_mut();
        field.extend(text.chars().filter(|c| !c.is_control()));
        if field.chars().count() > FIELD_MAX {
            *field = field.chars().take(FIELD_MAX).collect();
        }
        self.edited();
    }

    pub fn backspace(&mut self) {
        self.field_mut().pop();
        self.edited();
    }

    /// Ctrl-W: delete the last word.
    pub fn delete_word(&mut self) {
        let field = self.field_mut();
        let trimmed = field.trim_end().len();
        let cut = field[..trimmed].rfind(' ').map_or(0, |i| i + 1);
        field.truncate(cut);
        self.edited();
    }

    /// Ctrl-U: clear the field.
    pub fn clear_field(&mut self) {
        self.field_mut().clear();
        self.edited();
    }

    pub fn use_suggestion(&mut self, idx: usize) {
        if let Some(s) = self.prompt.suggestions.get(idx) {
            self.command = s.clone();
            self.focus = Field::Args;
            self.edited();
        }
    }

    /// ↑ (older) / ↓ (newer) through past runs of this file type.
    pub fn browse_history(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.hist_pos, older) {
            (None, true) => Some(0),
            (None, false) => return,
            (Some(i), true) => Some((i + 1).min(self.history.len() - 1)),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
        match next {
            Some(i) => {
                let (c, a) = self.history[i].clone();
                self.command = c;
                self.args = a;
            }
            None => {
                self.command.clear();
                self.args.clear();
            }
        }
        self.hist_pos = next;
        self.armed = false;
    }

    /// Enter: runs, or asks for a second Enter when the command looks destructive.
    pub fn submit(&mut self) -> RunOutcome {
        if self.blocked().is_some() {
            return RunOutcome::Stay;
        }
        if self.destructive() && !self.armed {
            self.armed = true;
            return RunOutcome::Stay;
        }
        RunOutcome::Run { command: self.command_line(), prefix: self.command.trim().into(), args: self.args.trim().into() }
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<RunHit> {
        let p = (x, y).into();
        if self.run_rect.contains(p) {
            return Some(RunHit::Run);
        }
        if self.cancel_rect.contains(p) {
            return Some(RunHit::Cancel);
        }
        if let Some(i) = self.suggestion_rects.iter().position(|r| r.contains(p)) {
            return Some(RunHit::Suggestion(i));
        }
        if let Some((f, _)) = self.fields.iter().find(|(_, r)| r.contains(p)) {
            return Some(RunHit::Field(*f));
        }
        self.rect.contains(p).then_some(RunHit::Inside)
    }

    pub fn render(&mut self, f: &mut Frame, theme: &Theme) {
        let screen = f.area();
        let w = 76.min(screen.width.saturating_sub(4)).max(30);
        let inner_w = w.saturating_sub(6);
        let command_line = format!("$ {}", self.command_line());
        let preview_rows = (command_line.width() as u16).div_ceil(inner_w.max(1)).clamp(1, 4);
        let destructive = self.destructive();
        let blocked = self.blocked();
        let p = &self.prompt;

        let status_line: Option<(String, Style)> = if let Some(reason) = blocked {
            Some((reason.to_string(), Style::new().fg(theme.fg_muted)))
        } else if self.armed {
            Some(("This command looks destructive. Press Enter again to run it.".into(), Style::new().fg(theme.danger)))
        } else {
            None
        };
        let notes = p.notes.len() as u16 + status_line.is_some() as u16;
        // title, gap, command, file, args, gap, suggestions, gap, preview, where, notes, gap, buttons
        let h = (15 + preview_rows + notes).min(screen.height); // + borders and padding
        let rect = Rect::new(screen.x + screen.width.saturating_sub(w) / 2, screen.y + screen.height.saturating_sub(h) / 2, w, h);
        self.rect = rect;

        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if destructive { theme.danger } else { theme.border }))
            .style(Style::new().bg(theme.raised_bg));
        let inner = block.inner(rect).inner(Margin::new(2, 1));
        f.render_widget(block, rect);
        let row = |i: u16| Rect::new(inner.x, inner.y + i, inner.width, 1);
        let muted = Style::new().fg(theme.fg_muted);

        // Title, with a badge when smtui runs as root.
        let mut title = vec![Span::styled("Run", Style::new().fg(theme.fg).add_modifier(Modifier::BOLD))];
        if p.as_root {
            title.push(Span::raw("  "));
            title.push(Span::styled(" running as root ", Style::new().bg(theme.danger_tint).fg(theme.danger)));
        }
        f.render_widget(Paragraph::new(Line::from(title)), row(0));

        const LABEL: u16 = 10;
        let field_w = inner.width.saturating_sub(LABEL);
        let draw_field = |f: &mut Frame, y: u16, label: &str, value: &str, focused: bool, placeholder: &str| {
            f.render_widget(Paragraph::new(Span::styled(label.to_string(), muted)), Rect::new(inner.x, y, LABEL, 1));
            let area = Rect::new(inner.x + LABEL, y, field_w, 1);
            let max = field_w.saturating_sub(3) as usize;
            // Show the end of long values, where the user is typing.
            let shown: String = if value.width() > max {
                let skip = value.chars().count().saturating_sub(max.saturating_sub(1));
                format!("…{}", value.chars().skip(skip).collect::<String>())
            } else {
                value.to_string()
            };
            let mut spans = vec![Span::raw(" ")];
            if shown.is_empty() && !focused {
                spans.push(Span::styled(placeholder.to_string(), Style::new().fg(theme.fg_disabled)));
            } else {
                spans.push(Span::styled(shown, Style::new().fg(theme.fg)));
            }
            if focused {
                spans.push(Span::styled("█", Style::new().fg(theme.accent)));
            }
            let bg = if focused { theme.bg } else { theme.hover_bg };
            f.render_widget(Paragraph::new(Line::from(spans)).style(Style::new().bg(bg)), area);
            area
        };
        let cmd_placeholder = if p.executable { "(run the file itself)" } else { "e.g. vim" };
        let cmd_rect = draw_field(f, inner.y + 2, "Command", &self.command, self.focus == Field::Command, cmd_placeholder);

        // The path line is a label: it is always quoted and cannot be edited.
        let file_label = if p.paths.len() == 1 { "File" } else { "Files" };
        let quoted: Vec<String> =
            p.paths.iter().map(|x| crate::modes::files::ops::shell_quote(&x.to_string_lossy()).into_owned()).collect();
        let joined = match quoted.as_slice() {
            [one] => one.clone(),
            many => format!("{} files: {}", many.len(), many.join(" ")),
        };
        f.render_widget(Paragraph::new(Span::styled(file_label, muted)), Rect::new(inner.x, inner.y + 3, LABEL, 1));
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {}", truncate_start(&joined, field_w.saturating_sub(1) as usize)), Style::new().fg(theme.accent))),
            Rect::new(inner.x + LABEL, inner.y + 3, field_w, 1),
        );
        let args_rect = draw_field(f, inner.y + 4, "Args", &self.args, self.focus == Field::Args, "optional, e.g. -n 100");
        self.fields = [(Field::Command, cmd_rect), (Field::Args, args_rect)];

        // Suggestions as clickable chips.
        let y = inner.y + 6;
        f.render_widget(Paragraph::new(Span::styled("Try", muted)), Rect::new(inner.x, y, LABEL, 1));
        self.suggestion_rects.clear();
        let mut x = inner.x + LABEL;
        for s in &p.suggestions {
            let text = if s.is_empty() { " ./file ".to_string() } else { format!(" {s} ") };
            let cw = text.width() as u16;
            if x + cw > inner.right() {
                break;
            }
            let r = Rect::new(x, y, cw, 1);
            let active = *s == self.command.trim();
            let style = if active { Style::new().bg(theme.accent_tint).fg(theme.accent) } else { Style::new().bg(theme.hover_bg).fg(theme.fg) };
            f.render_widget(Paragraph::new(Span::styled(text, style)), r);
            self.suggestion_rects.push(r);
            x += cw + 1;
        }

        // Preview of the exact command line.
        let y = inner.y + 8;
        let preview_style = if destructive { Style::new().fg(theme.danger) } else { Style::new().fg(theme.fg) };
        // Hard-wrapped by characters: paths have no spaces to break at.
        let preview: Vec<Line> = chunks(&command_line, inner.width as usize, preview_rows as usize)
            .into_iter()
            .map(|c| Line::styled(c, preview_style.add_modifier(Modifier::BOLD)))
            .collect();
        f.render_widget(Paragraph::new(preview), Rect::new(inner.x, y, inner.width, preview_rows));
        let mut y = y + preview_rows;
        let prefix = "New terminal tab in ";
        let cwd = format!("{prefix}{}", truncate_start(&p.cwd.to_string_lossy(), (inner.width as usize).saturating_sub(prefix.len())));
        f.render_widget(Paragraph::new(Span::styled(cwd, muted)), Rect::new(inner.x, y, inner.width, 1));
        y += 1;
        for note in &p.notes {
            let style = if note.danger { Style::new().fg(theme.danger) } else { muted };
            let text = format!("{} {}", if note.danger { "!" } else { "·" }, note.text);
            f.render_widget(Paragraph::new(Span::styled(truncate(&text, inner.width as usize), style)), Rect::new(inner.x, y, inner.width, 1));
            y += 1;
        }
        if let Some((text, style)) = status_line {
            f.render_widget(Paragraph::new(Span::styled(truncate(&text, inner.width as usize), style)), Rect::new(inner.x, y, inner.width, 1));
        }

        // Buttons.
        let y = inner.bottom().saturating_sub(1);
        let cancel = "  Cancel  ";
        let run_label = if self.armed { "  Run anyway ⏎  " } else { "  Run ⏎  " };
        let (cw, rw) = (cancel.width() as u16, run_label.width() as u16);
        self.run_rect = Rect::new(inner.right().saturating_sub(rw), y, rw, 1);
        self.cancel_rect = Rect::new(self.run_rect.x.saturating_sub(cw + 2), y, cw, 1);
        let run_style = if blocked.is_some() {
            Style::new().bg(theme.hover_bg).fg(theme.fg_disabled)
        } else if destructive {
            Style::new().bg(theme.danger_tint).fg(theme.danger).add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(theme.accent_tint).fg(theme.accent).add_modifier(Modifier::BOLD)
        };
        f.render_widget(Paragraph::new(Span::styled(cancel, Style::new().bg(theme.hover_bg).fg(theme.fg))), self.cancel_rect);
        f.render_widget(Paragraph::new(Span::styled(run_label, run_style)), self.run_rect);
        let hint = if self.history.is_empty() { "Esc cancel · Tab next field" } else { "Esc cancel · Tab next field · ↑↓ history" };
        if (hint.width() as u16) + cw + rw + 4 <= inner.width {
            f.render_widget(Paragraph::new(Span::styled(hint, Style::new().fg(theme.fg_disabled))), Rect::new(inner.x, y, hint.width() as u16, 1));
        }
    }
}

/// Keeps the end of `s` (the file name), e.g. `…/app/notes.txt`.
fn truncate_start(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out: Vec<char> = Vec::new();
    let mut w = 1;
    for c in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max {
            break;
        }
        w += cw;
        out.push(c);
    }
    out.push('…');
    out.into_iter().rev().collect()
}

/// Splits `s` into at most `rows` lines of `width` columns; the last one ends in `…` if cut.
fn chunks(s: &str, width: usize, rows: usize) -> Vec<String> {
    let width = width.max(2);
    let mut lines = vec![String::new()];
    let mut w = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > width {
            if lines.len() == rows {
                let last = lines.last_mut().expect("one line");
                last.pop();
                last.push('…');
                return lines;
            }
            lines.push(String::new());
            w = 0;
        }
        lines.last_mut().expect("one line").push(c);
        w += cw;
    }
    lines
}
