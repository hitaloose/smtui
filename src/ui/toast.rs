//! Short-lived notice shown above the bottom bar.

use std::time::{Duration, Instant};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;

const LIFETIME: Duration = Duration::from_secs(5);

pub struct Toast {
    msg: String,
    detail: String,
    until: Instant,
}

impl Toast {
    pub fn new(msg: String, detail: String) -> Self {
        Self { msg, detail, until: Instant::now() + LIFETIME }
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.until
    }

    /// `area` is the region the toast may cover; it is anchored to its bottom center.
    pub fn render(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let max_inner = area.width.saturating_sub(6).clamp(1, 96) as usize;
        let inner_w = self.msg.width().max(self.detail.width()).clamp(1, max_inner);
        let detail_lines = wrap_chars(&self.detail, inner_w);
        let h = (3 + detail_lines.len() as u16).min(area.height);
        let w = inner_w as u16 + 4;
        let rect = Rect::new(
            area.x + area.width.saturating_sub(w) / 2,
            area.bottom().saturating_sub(h),
            w.min(area.width),
            h,
        );

        let mut lines = vec![Line::styled(self.msg.clone(), Style::new().fg(theme.fg).add_modifier(Modifier::BOLD))];
        lines.extend(detail_lines.into_iter().map(|l| Line::styled(l, Style::new().fg(theme.fg_muted))));

        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme.border))
            .style(Style::new().bg(theme.hover_bg));
        let inner = block.inner(rect).inner(ratatui::layout::Margin::new(1, 0));
        f.render_widget(block, rect);
        f.render_widget(Paragraph::new(lines), inner);
    }
}

/// Hard wrap at `width` columns: details are usually commands and paths without spaces.
fn wrap_chars(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if cur.width() + unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) > width {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn wraps_long_text() {
        assert_eq!(super::wrap_chars("abcdef", 4), vec!["abcd", "ef"]);
        assert!(super::wrap_chars("", 4).is_empty());
    }
}
