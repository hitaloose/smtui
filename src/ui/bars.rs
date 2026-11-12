//! Tab bar (top), terminal header (between the panes) and status line (bottom).
//!
//! The tab and bottom bars are 3 rows tall (1 in compact mode on small screens); text sits
//! on the middle row and every button's hit area covers the full height, like the
//! prototype's 44px controls. The terminal header is a single row.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use std::path::Path;

use super::{TermSize, hit::HitMap, theme::Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeTarget {
    Tab(usize),
    CloseTab(usize),
    NewTab,
    /// The `TERMINAL` title: opens a minimized terminal, minimizes an open one.
    TermTitle,
    TermSize(TermSize),
    /// Opens the menu of leader-key shortcuts.
    Shortcuts,
    Quit,
}

#[derive(Debug, Clone)]
pub struct TabInfo {
    pub title: String,
    /// A program is running in the tab's terminal.
    pub busy: bool,
}

/// `~/apps` for folders under `$HOME`.
pub fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir().filter(|h| h.as_os_str().len() > 1)
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.display().to_string()
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

/// Like [`truncate`], but keeps the end: `…/scratchpad/play`.
pub fn truncate_start(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut w = 1;
    for ch in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max {
            break;
        }
        w += cw;
        tail.push(ch);
    }
    let mut out = String::from(if max > 0 { "…" } else { "" });
    out.extend(tail.into_iter().rev());
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
    let busy_w = |i: usize| if tabs[i].busy { 2 } else { 0 };
    let widths: Vec<u16> = labels.iter().enumerate().map(|(i, l)| l.width() as u16 + busy_w(i) + close_w + 1).collect();

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
        let mut spans = vec![Span::styled(label.clone(), label_style)];
        if tabs[i].busy {
            spans.push(Span::styled(" ●", Style::new().fg(theme.warn).bg(bg)));
        }
        let lw = text_at(f, x, y, spans);
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

/// What the terminal header shows.
pub struct TermHeader<'a> {
    pub dir: &'a Path,
    pub focused: bool,
    pub size: TermSize,
    /// The program in the foreground, if any.
    pub busy: Option<&'a str>,
    /// Folder the shell will `cd` into once that program exits.
    pub pending: Option<&'a Path>,
    /// False when the Files folder does not exist on disk (the demo), so nothing is followed.
    pub follows: bool,
}

/// `▸ TERMINAL  ~/apps  ● follows Files ……… min  half  full`
pub fn render_term_header(f: &mut Frame, area: Rect, theme: &Theme, h: &TermHeader, hits: &mut HitMap<ChromeTarget>) {
    if area.height == 0 {
        return;
    }
    fill(f, area, Style::new().bg(theme.bar_bg));
    let y = area.y;
    let base = Style::new().bg(theme.bar_bg);

    // Size buttons on the right: the current one is highlighted.
    let buttons = [(TermSize::Min, " min "), (TermSize::Normal, " half "), (TermSize::Full, " full ")];
    let buttons_w: u16 = buttons.iter().map(|(_, l)| l.width() as u16 + 1).sum::<u16>() + 1;
    let mut bx = area.right().saturating_sub(buttons_w);
    let left_limit = bx.saturating_sub(1);
    for (size, label) in buttons {
        let style = if size == h.size {
            base.bg(theme.accent_tint).fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            base.fg(theme.fg_muted)
        };
        let w = text_at(f, bx, y, vec![Span::styled(label, style)]);
        hits.push(Rect::new(bx, y, w, 1), ChromeTarget::TermSize(size));
        bx += w + 1;
    }

    let arrow = if h.size == TermSize::Min { "▸" } else { "▾" };
    let title_style = if h.focused { base.fg(theme.accent).add_modifier(Modifier::BOLD) } else { base.fg(theme.fg_muted) };
    let title = format!(" {arrow} TERMINAL ");
    let tw = text_at(f, area.x, y, vec![Span::styled(title, title_style)]);
    hits.push(Rect::new(area.x, y, tw, 1), ChromeTarget::TermTitle);

    let mut x = area.x + tw + 1;
    let room = |x: u16| left_limit.saturating_sub(x) as usize;
    let dir = truncate_start(&tilde(h.dir), room(x).min(40));
    if !dir.is_empty() {
        x += text_at(f, x, y, vec![Span::styled(dir, base.fg(theme.fg_muted))]) + 2;
    }
    let (state, color) = match (h.busy, h.pending) {
        (Some(prog), Some(dir)) => {
            let name = dir.file_name().map_or_else(|| "/".into(), |n| n.to_string_lossy().into_owned());
            (format!("● {prog} is running · cd {name} when it exits"), theme.warn)
        }
        (Some(prog), None) => (format!("● {prog} is running"), theme.warn),
        (None, _) if h.follows => ("● follows Files".to_string(), theme.accent),
        (None, _) => (String::new(), theme.fg_muted),
    };
    if room(x) > 3 && !state.is_empty() {
        text_at(f, x, y, vec![Span::styled(truncate(&state, room(x)), base.fg(color))]);
    }
}

/// Status on the left, then `Ctrl-] Shortcuts │ Ctrl-q Quit` on the right.
pub fn render_bottom_bar(f: &mut Frame, area: Rect, theme: &Theme, status: &str, leader: &str, hits: &mut HitMap<ChromeTarget>) {
    fill(f, area, Style::new().bg(theme.bar_bg));
    let y = mid(area);
    let keycap = |k: &str| Span::styled(format!(" {k} "), Style::new().bg(theme.fg_muted).fg(theme.bar_bg).add_modifier(Modifier::BOLD));
    let label = |l: &str| Span::styled(format!(" {l}"), Style::new().fg(theme.fg).bg(theme.bar_bg));
    let groups = [
        (vec![keycap(leader), label("Shortcuts")], ChromeTarget::Shortcuts),
        (vec![keycap("Ctrl-q"), label("Quit")], ChromeTarget::Quit),
    ];

    const SEP: u16 = 5; // "  │  "
    let width = |spans: &[Span]| spans.iter().map(|s| s.width() as u16).sum::<u16>();
    let mut right = area.right().saturating_sub(2);
    let min_x = area.x + 2;
    let n = groups.len();
    for (i, (spans, target)) in groups.into_iter().enumerate().rev() {
        let w = width(&spans);
        let sep = if i + 1 < n { SEP } else { 0 };
        if right < min_x + w + sep {
            break;
        }
        if sep > 0 {
            right -= sep;
            text_at(f, right + 2, y, vec![Span::styled("│", Style::new().fg(theme.border).bg(theme.bar_bg))]);
        }
        right -= w;
        text_at(f, right, y, spans);
        hits.push(Rect::new(right.saturating_sub(1), area.y, w + 2, area.height), target);
    }

    // Status takes what is left on the left, truncated from the end.
    let room = right.saturating_sub(min_x + SEP) as usize;
    if room > 3 {
        let text = truncate(status, room);
        text_at(f, min_x, y, vec![Span::styled(text, Style::new().fg(theme.fg_muted).bg(theme.bar_bg))]);
    }
}
