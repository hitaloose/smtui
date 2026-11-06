//! Tab bar (top) and mode switcher + status line (bottom), generated from the registered modes.
//!
//! Both bars are 3 rows tall (1 in compact mode on small screens); text sits on the middle
//! row and every button's hit area covers the full height, like the prototype's 44px controls.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use super::{hit::HitMap, theme::Theme};
use crate::modes::TabInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeTarget {
    Tab(usize),
    CloseTab(usize),
    NewTab,
    Mode(usize),
    Quit,
}

const MAX_TITLE: usize = 24;

pub fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.width() + 1 >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

pub fn fill(f: &mut Frame, rect: Rect, style: Style) {
    f.render_widget(Block::default().style(style), rect);
}

/// The row where text goes inside a control of `area`'s height.
pub fn mid(area: Rect) -> u16 {
    area.y + area.height / 2
}

fn text_at(f: &mut Frame, x: u16, y: u16, spans: Vec<Span<'static>>) -> u16 {
    let line = Line::from(spans);
    let w = line.width() as u16;
    f.render_widget(Paragraph::new(line), Rect::new(x, y, w, 1));
    w
}

fn vline(f: &mut Frame, x: u16, area: Rect, theme: &Theme) {
    let lines: Vec<Line> = (0..area.height).map(|_| Line::raw("│")).collect();
    f.render_widget(
        Paragraph::new(lines).style(Style::new().fg(theme.border).bg(theme.bar_bg)),
        Rect::new(x, area.y, 1, area.height),
    );
}

pub fn render_tab_bar(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    icon: &str,
    tabs: &[TabInfo],
    active: usize,
    hits: &mut HitMap<ChromeTarget>,
) {
    fill(f, area, Style::new().bg(theme.bar_bg));
    let tall = area.height >= 3;
    let y = mid(area);
    let closable = tabs.len() > 1;
    let plus_w = 7u16;
    let avail = area.width.saturating_sub(plus_w);
    let close_w: u16 = if closable { 4 } else { 1 };

    let labels: Vec<String> = tabs.iter().map(|t| format!("  {icon}  {}", truncate(&t.title, MAX_TITLE))).collect();
    let widths: Vec<u16> = labels.iter().map(|l| l.width() as u16 + close_w + 1).collect();

    // Drop tabs from the left until the active one fits.
    let mut first = 0;
    while first < active && widths[first..=active].iter().sum::<u16>() > avail {
        first += 1;
    }

    let mut x = area.x;
    for (i, label) in labels.iter().enumerate().skip(first) {
        let w = widths[i];
        if x + w > area.x + avail {
            break;
        }
        let on = i == active;
        let bg = if on { theme.bg } else { theme.bar_bg };
        let rect = Rect::new(x, area.y, w - 1, area.height);
        fill(f, rect, Style::new().bg(bg));
        let fg = if on { theme.fg } else { theme.fg_muted };
        if tall {
            if on {
                // Accent line on top of the active tab.
                let line = "▔".repeat(rect.width as usize);
                f.render_widget(Paragraph::new(Span::styled(line, Style::new().fg(theme.accent).bg(bg))), Rect::new(x, area.y, rect.width, 1));
            }
        } else {
            let marker = if on { theme.accent } else { bg };
            text_at(f, x, y, vec![Span::styled("▎", Style::new().fg(marker).bg(bg))]);
        }
        let mut label_style = Style::new().fg(fg).bg(bg);
        if on {
            label_style = label_style.add_modifier(Modifier::BOLD);
        }
        let lw = text_at(f, x, y, vec![Span::styled(label.clone(), label_style)]);
        hits.push(Rect::new(x, area.y, lw, area.height), ChromeTarget::Tab(i));
        if closable {
            let cx = x + lw + 1;
            text_at(f, cx, y, vec![Span::styled("×", Style::new().fg(theme.fg_muted).bg(bg))]);
            hits.push(Rect::new(cx - 1, area.y, 3, area.height), ChromeTarget::CloseTab(i));
        }
        x += w - 1;
        vline(f, x, area, theme);
        x += 1;
    }

    let plus = Rect::new(area.right().saturating_sub(plus_w), area.y, plus_w, area.height);
    vline(f, plus.x, area, theme);
    text_at(f, plus.x + 3, y, vec![Span::styled("+", Style::new().fg(theme.fg).bg(theme.bar_bg).add_modifier(Modifier::BOLD))]);
    hits.push(Rect::new(plus.x + 1, plus.y, plus_w - 1, plus.height), ChromeTarget::NewTab);
}

pub fn render_bottom_bar(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    modes: &[(String, String)],
    active: usize,
    status: &str,
    hits: &mut HitMap<ChromeTarget>,
) {
    fill(f, area, Style::new().bg(theme.bar_bg));
    let y = mid(area);

    // Mode switcher: a raised group holding one full-height button per mode.
    let labels: Vec<String> = modes.iter().map(|(icon, name)| format!("  {icon}  {name}  ")).collect();
    let group_w: u16 = labels.iter().map(|l| l.width() as u16).sum::<u16>() + labels.len() as u16 + 1;
    let group = Rect::new(area.x + 1, area.y, group_w.min(area.width.saturating_sub(2)), area.height);
    fill(f, group, Style::new().bg(theme.raised_bg));
    let mut x = group.x + 1;
    for (i, label) in labels.iter().enumerate() {
        let w = label.width() as u16;
        if x + w > group.right() {
            break;
        }
        let on = i == active;
        let style = if on {
            Style::new().bg(theme.accent_tint).fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(theme.raised_bg).fg(theme.fg_muted)
        };
        let rect = Rect::new(x, area.y, w, area.height);
        fill(f, rect, style);
        text_at(f, x, y, vec![Span::styled(label.clone(), style)]);
        hits.push(rect, ChromeTarget::Mode(i));
        x += w + 1;
    }
    let x = group.right();

    // Quit button at the far right.
    let quit_label = "  × Quit  ";
    let quit_w = quit_label.width() as u16;
    let mut right = area.right();
    if x + quit_w + 2 < area.right() {
        let rect = Rect::new(area.right() - quit_w - 1, area.y, quit_w, area.height);
        let style = Style::new().bg(theme.raised_bg).fg(theme.fg_muted);
        fill(f, rect, style);
        text_at(f, rect.x, y, vec![Span::styled(quit_label, style)]);
        hits.push(rect, ChromeTarget::Quit);
        right = rect.x;
    }

    let room = right.saturating_sub(x + 4) as usize;
    if room > 0 {
        let text = truncate(status, room);
        let w = text.width() as u16;
        text_at(f, right.saturating_sub(w + 2), y, vec![Span::styled(text, Style::new().fg(theme.fg_muted).bg(theme.bar_bg))]);
    }
}
